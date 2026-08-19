//! Durable orchestration state for a draft's test-case runs.
//!
//! This module owns reservations, immutable run mappings, reconciliation, and
//! report evidence. It deliberately does not admit or execute a case; the
//! sequential executor composes those operations later against these rows.
//!
//! Residency (wamn-0h0g.8.18): these relations live in the CONTROL database, and
//! the statements here name nothing else. The relation names are unqualified and
//! resolve through the pinned `search_path`, so residency — not a renamed schema
//! — is what distinguishes this store from the project copy. A project fact this
//! module needs, such as a run that ended effect-uncertain, arrives as an
//! already-observed argument rather than as a join, because a transaction cannot
//! span the two databases and nothing here may claim it can.

use std::collections::BTreeSet;

use anyhow::{Context as _, bail};
use serde_json::Value;
use tokio_postgres::{Client, GenericClient};

use super::sha256;

/// Per-case execution horizon selected for the MVP test-set runner.
pub const TEST_CASE_DEADLINE_SECONDS: i32 = 60;

/// Whole-set horizon, large enough for 256 sequential per-case horizons.
pub const TEST_REPORT_DEADLINE_SECONDS: i32 = 18_000;

/// One accepted command's immutable selection and ordered case identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestReportReservation {
    pub tenant_id: String,
    pub report_id: String,
    pub command_hash: String,
    /// The validated-draft hash is the narrowed candidate-override identity,
    /// and covers the draft's cases as well as its graph.
    pub validated_draft_id: String,
    pub catalog_id: String,
    pub catalog_version: i32,
    pub case_ids: Vec<String>,
}

/// Stable receipt returned once the reservation and every run mapping exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedTestReport {
    pub report_id: String,
}

/// Result of reconciling only the lifecycle state owned by this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestReportReconciliation {
    Pending,
    Finalized { passed: bool },
}

/// Failure kinds stored on one finalized case mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestCaseFailure {
    AssertionFailed,
    DeadlineExhausted,
    EffectUncertain,
}

/// One executor-produced terminal result for an immutable case mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct TestCaseFinalization {
    pub tenant_id: String,
    pub report_id: String,
    pub ordinal: i32,
    pub passed: bool,
    pub failure: Option<TestCaseFailure>,
    pub summary: Value,
}

impl TestCaseFailure {
    const fn as_sql(self) -> &'static str {
        match self {
            Self::AssertionFailed => "assertion-failed",
            Self::DeadlineExhausted => "deadline-exhausted",
            Self::EffectUncertain => "effect-uncertain",
        }
    }
}

/// Insert one pending report reservation.
pub fn insert_test_report_reservation_sql() -> &'static str {
    "INSERT INTO authoring_test_run_reservations \
       (tenant_id, report_id, command_hash, validated_draft_id, \
        catalog_id, catalog_version, case_count, whole_deadline_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, \
             clock_timestamp() + make_interval(secs => $8::int)) \
     ON CONFLICT (tenant_id, report_id) DO NOTHING"
}

/// Insert one deterministic ordinal-to-run mapping beneath its reservation.
pub fn insert_test_case_run_sql() -> &'static str {
    "INSERT INTO authoring_test_case_runs \
       (tenant_id, report_id, ordinal, case_id, run_id, catalog_id, \
        catalog_version, validated_draft_id, case_deadline_at) \
     SELECT reservation.tenant_id, reservation.report_id, $3, $4, $5, \
            reservation.catalog_id, reservation.catalog_version, \
            reservation.validated_draft_id, \
            LEAST(reservation.created_at + \
                      make_interval(secs => (($3 + 1) * $6)::int), \
                  reservation.whole_deadline_at) \
       FROM authoring_test_run_reservations AS reservation \
      WHERE reservation.tenant_id = $1 AND reservation.report_id = $2 \
        AND reservation.state = 'pending' \
     ON CONFLICT (tenant_id, report_id, ordinal) DO NOTHING"
}

/// Read an existing reservation and its exact selection after idempotent insert.
pub fn select_test_report_reservation_sql() -> &'static str {
    "SELECT command_hash, validated_draft_id, catalog_id, \
            catalog_version, case_count, state \
       FROM authoring_test_run_reservations \
      WHERE tenant_id = $1 AND report_id = $2"
}

/// Read all deterministic case mappings in ordinal order.
pub fn select_test_case_mappings_sql() -> &'static str {
    "SELECT ordinal, case_id, run_id \
       FROM authoring_test_case_runs \
      WHERE tenant_id = $1 AND report_id = $2 ORDER BY ordinal"
}

fn validate_reservation(reservation: &TestReportReservation) -> anyhow::Result<()> {
    for (value, name) in [
        (&reservation.tenant_id, "tenant id"),
        (&reservation.report_id, "report id"),
        (&reservation.validated_draft_id, "validated draft id"),
        (&reservation.catalog_id, "catalog id"),
    ] {
        if value.is_empty() {
            bail!("test report {name} must not be empty");
        }
    }
    if reservation.catalog_version <= 0 {
        bail!("test report catalog version must be positive");
    }
    if !is_sha256(&reservation.command_hash) {
        bail!("test report command hash must be lowercase sha256");
    }
    if reservation.case_ids.is_empty() || reservation.case_ids.len() > wamn_flow::MAX_TEST_SET_CASES
    {
        bail!("test report case count is outside the stored bound");
    }
    let mut unique = BTreeSet::new();
    if reservation
        .case_ids
        .iter()
        .any(|case_id| case_id.is_empty() || !unique.insert(case_id))
    {
        bail!("test report case ids must be non-empty and unique");
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Derive the only run identity an ordinal may ever receive.
pub fn test_case_run_id(tenant_id: &str, report_id: &str, ordinal: usize, case_id: &str) -> String {
    let preimage = format!("{tenant_id}\0{report_id}\0{ordinal}\0{case_id}");
    format!("test-run-{}", &sha256(preimage.as_bytes())[7..])
}

/// Persist an accepted report identity and every case mapping atomically.
pub async fn reserve_test_report(
    client: &mut Client,
    reservation: &TestReportReservation,
) -> anyhow::Result<AcceptedTestReport> {
    validate_reservation(reservation)?;
    let case_count =
        i32::try_from(reservation.case_ids.len()).context("test report case count exceeds i32")?;
    let transaction = client
        .transaction()
        .await
        .context("begin test report reservation")?;
    transaction
        .execute(
            insert_test_report_reservation_sql(),
            &[
                &reservation.tenant_id,
                &reservation.report_id,
                &reservation.command_hash,
                &reservation.validated_draft_id,
                &reservation.catalog_id,
                &reservation.catalog_version,
                &case_count,
                &TEST_REPORT_DEADLINE_SECONDS,
            ],
        )
        .await
        .context("insert test report reservation")?;
    for (ordinal, case_id) in reservation.case_ids.iter().enumerate() {
        let ordinal = i32::try_from(ordinal).expect("bounded test case ordinal fits i32");
        let run_id = test_case_run_id(
            &reservation.tenant_id,
            &reservation.report_id,
            usize::try_from(ordinal).expect("non-negative ordinal fits usize"),
            case_id,
        );
        transaction
            .execute(
                insert_test_case_run_sql(),
                &[
                    &reservation.tenant_id,
                    &reservation.report_id,
                    &ordinal,
                    &case_id,
                    &run_id,
                    &TEST_CASE_DEADLINE_SECONDS,
                ],
            )
            .await
            .with_context(|| format!("reserve test case ordinal {ordinal}"))?;
    }
    verify_reservation(&transaction, reservation).await?;
    transaction
        .commit()
        .await
        .context("commit test report reservation")?;
    Ok(AcceptedTestReport {
        report_id: reservation.report_id.clone(),
    })
}

async fn verify_reservation<C>(client: &C, expected: &TestReportReservation) -> anyhow::Result<()>
where
    C: GenericClient + Sync,
{
    let row = client
        .query_opt(
            select_test_report_reservation_sql(),
            &[&expected.tenant_id, &expected.report_id],
        )
        .await
        .context("read test report reservation")?
        .context("test report reservation disappeared")?;
    let expected_count = i32::try_from(expected.case_ids.len()).expect("bounded count fits i32");
    let state: String = row.get(5);
    if row.get::<_, String>(0) != expected.command_hash
        || row.get::<_, String>(1) != expected.validated_draft_id
        || row.get::<_, String>(2) != expected.catalog_id
        || row.get::<_, i32>(3) != expected.catalog_version
        || row.get::<_, i32>(4) != expected_count
        || !matches!(state.as_str(), "pending" | "finalized")
    {
        bail!("report id is reserved for a different test command");
    }
    let rows = client
        .query(
            select_test_case_mappings_sql(),
            &[&expected.tenant_id, &expected.report_id],
        )
        .await
        .context("read deterministic test case mappings")?;
    if rows.len() != expected.case_ids.len() {
        bail!("test report reservation has an incomplete case mapping");
    }
    for (ordinal, (row, case_id)) in rows.iter().zip(&expected.case_ids).enumerate() {
        let stored_ordinal: i32 = row.get(0);
        let stored_case_id: String = row.get(1);
        let stored_run_id: String = row.get(2);
        if stored_ordinal != i32::try_from(ordinal).expect("bounded ordinal fits i32")
            || stored_case_id != *case_id
            || stored_run_id
                != test_case_run_id(&expected.tenant_id, &expected.report_id, ordinal, case_id)
        {
            bail!("test report retry observed a different case mapping");
        }
    }
    Ok(())
}

/// Persist one executor-produced terminal result against its immutable mapping.
pub async fn finalize_test_case(
    client: &Client,
    finalization: &TestCaseFinalization,
) -> anyhow::Result<()> {
    if finalization.passed == finalization.failure.is_some() || !finalization.summary.is_object() {
        bail!("test case terminal outcome is contradictory or malformed");
    }
    let failure = finalization.failure.map(TestCaseFailure::as_sql);
    let updated = client
        .execute(
            "UPDATE authoring_test_case_runs \
                SET state = 'finalized', passed = $4, failure_kind = $5, \
                    summary = $6::jsonb, finalized_at = clock_timestamp() \
              WHERE tenant_id = $1 AND report_id = $2 AND ordinal = $3 \
                AND state = 'pending'",
            &[
                &finalization.tenant_id,
                &finalization.report_id,
                &finalization.ordinal,
                &finalization.passed,
                &failure,
                &finalization.summary,
            ],
        )
        .await
        .context("finalize test case mapping")?;
    if updated != 1 {
        bail!("test case mapping was absent or already finalized");
    }
    Ok(())
}

/// Reconcile deadline/effect-uncertain cases and finalize a ready report.
///
/// `effect_uncertain_run_ids` are project run identities the caller has ALREADY
/// OBSERVED as effect-uncertain, named by the producer-idempotency coordinate
/// they were reserved under ([`test_case_run_id`]).
///
/// They arrive as an argument because the reservation, case-map, and report
/// relations moved to the control database (wamn-0h0g.8.18) while `runs` stayed
/// project-local. The statement this replaced joined `runs` in the same
/// transaction; across two databases that join cannot exist, and writing one
/// would be a claim of atomicity the platform does not have. An already-observed
/// terminal run status is an immutable fact, so consuming it as an input loses
/// nothing: re-observing it inside this transaction could only produce the same
/// answer. Observing project runs in the first place stays with wamn-0h0g.8.5.
///
/// An empty slice is the ordinary case and finalizes nothing on that leg.
///
/// # There is no resolution map here (wamn-0h0g.15.170)
///
/// This used to pin the reservation's `resolution_map` from the first case run
/// carrying one. wamn-0h0g.15.7 deleted the last writer of
/// `authoring_test_case_runs.resolution_map`, so that select could only ever match
/// zero rows and the statement was a no-op that read as a source; wamn-0h0g.15.29
/// removed it, leaving columns that carried `'{}'` forever. The columns are now
/// gone. The publish gate's `tested_resolution_map` is derived from the release
/// manifest in `wamn_ctl::publish_release`, never from this report.
pub async fn reconcile_test_report(
    client: &mut Client,
    tenant_id: &str,
    report_id: &str,
    effect_uncertain_run_ids: &[String],
) -> anyhow::Result<TestReportReconciliation> {
    let transaction = client.transaction().await?;
    let Some(reservation) = transaction
        .query_opt(
            "SELECT state FROM authoring_test_run_reservations \
              WHERE tenant_id = $1 AND report_id = $2 FOR UPDATE",
            &[&tenant_id, &report_id],
        )
        .await?
    else {
        bail!("test report reservation does not exist");
    };
    if reservation.get::<_, String>(0) == "finalized" {
        let passed: bool = transaction
            .query_one(
                "SELECT passed FROM authoring_test_reports \
                  WHERE tenant_id = $1 AND report_id = $2",
                &[&tenant_id, &report_id],
            )
            .await?
            .get(0);
        transaction.commit().await?;
        return Ok(TestReportReconciliation::Finalized { passed });
    }

    transaction
        .execute(
            "UPDATE authoring_test_case_runs AS test_case \
                SET state = 'finalized', passed = false, \
                    failure_kind = 'effect-uncertain', \
                    summary = jsonb_build_object('kind', 'effect-uncertain'), \
                    finalized_at = clock_timestamp() \
              WHERE test_case.tenant_id = $1 AND test_case.report_id = $2 \
                AND test_case.state = 'pending' \
                AND test_case.run_id = ANY($3::text[])",
            &[&tenant_id, &report_id, &effect_uncertain_run_ids],
        )
        .await
        .context("reconcile effect-uncertain test cases")?;
    transaction
        .execute(
            "UPDATE authoring_test_case_runs AS test_case \
                SET state = 'finalized', passed = false, \
                    failure_kind = 'deadline-exhausted', \
                    summary = jsonb_build_object('kind', 'deadline-exhausted'), \
                    finalized_at = clock_timestamp() \
               FROM authoring_test_run_reservations AS reservation \
              WHERE test_case.tenant_id = $1 AND test_case.report_id = $2 \
                AND test_case.state = 'pending' \
                AND reservation.tenant_id = test_case.tenant_id \
                AND reservation.report_id = test_case.report_id \
                AND clock_timestamp() >= LEAST( \
                    test_case.case_deadline_at, reservation.whole_deadline_at)",
            &[&tenant_id, &report_id],
        )
        .await
        .context("reconcile deadline-exhausted test cases")?;
    let pending: i64 = transaction
        .query_one(
            "SELECT count(*) FROM authoring_test_case_runs \
              WHERE tenant_id = $1 AND report_id = $2 AND state = 'pending'",
            &[&tenant_id, &report_id],
        )
        .await?
        .get(0);
    if pending != 0 {
        transaction.commit().await?;
        return Ok(TestReportReconciliation::Pending);
    }
    let passed: bool = transaction
        .query_one(
            "WITH inserted AS ( \
                INSERT INTO authoring_test_reports \
                    (tenant_id, report_id, validated_draft_id, \
                     catalog_id, catalog_version, passed, summary) \
                SELECT reservation.tenant_id, reservation.report_id, \
                       reservation.validated_draft_id, \
                       reservation.catalog_id, reservation.catalog_version, \
                       bool_and(test_case.passed), \
                       jsonb_build_object('cases', jsonb_agg( \
                           jsonb_build_object( \
                               'ordinal', test_case.ordinal, 'case-id', test_case.case_id, \
                               'run-id', test_case.run_id, 'passed', test_case.passed, \
                               'failure-kind', test_case.failure_kind, \
                               'summary', test_case.summary) ORDER BY test_case.ordinal)) \
                  FROM authoring_test_run_reservations AS reservation \
                  JOIN authoring_test_case_runs AS test_case \
                    ON test_case.tenant_id = reservation.tenant_id \
                   AND test_case.report_id = reservation.report_id \
                 WHERE reservation.tenant_id = $1 AND reservation.report_id = $2 \
                   AND reservation.state = 'pending' \
                 GROUP BY reservation.tenant_id, reservation.report_id \
                RETURNING passed \
             ) SELECT passed FROM inserted",
            &[&tenant_id, &report_id],
        )
        .await
        .context("insert immutable finalized test report")?
        .get(0);
    transaction
        .execute(
            "UPDATE authoring_test_run_reservations \
                SET state = 'finalized', finalized_at = clock_timestamp() \
              WHERE tenant_id = $1 AND report_id = $2 AND state = 'pending'",
            &[&tenant_id, &report_id],
        )
        .await
        .context("finalize test report reservation")?;
    transaction.commit().await?;
    Ok(TestReportReconciliation::Finalized { passed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reservation() -> TestReportReservation {
        TestReportReservation {
            tenant_id: "tenant-a".into(),
            report_id: "report-a".into(),
            command_hash: format!("sha256:{}", "a".repeat(64)),
            validated_draft_id: "validated-a".into(),
            catalog_id: "catalog-a".into(),
            catalog_version: 7,
            case_ids: vec!["first".into(), "second".into()],
        }
    }

    #[test]
    fn deadlines_and_case_bound_are_explicit_and_compatible() {
        assert_eq!(TEST_CASE_DEADLINE_SECONDS, 60);
        assert_eq!(TEST_REPORT_DEADLINE_SECONDS, 18_000);
        assert!(
            TEST_REPORT_DEADLINE_SECONDS
                > TEST_CASE_DEADLINE_SECONDS
                    * i32::try_from(wamn_flow::MAX_TEST_SET_CASES).unwrap()
        );
        validate_reservation(&reservation()).unwrap();
    }

    #[test]
    fn a_report_retry_derives_the_same_run_for_each_ordinal() {
        let first = test_case_run_id("tenant-a", "report-a", 7, "case-a");
        assert_eq!(first, test_case_run_id("tenant-a", "report-a", 7, "case-a"));
        assert_ne!(first, test_case_run_id("tenant-a", "report-a", 8, "case-a"));
        assert_ne!(first, test_case_run_id("tenant-a", "report-b", 7, "case-a"));
    }

    #[test]
    fn statements_pin_normalized_idempotency() {
        assert!(
            insert_test_report_reservation_sql()
                .contains("ON CONFLICT (tenant_id, report_id) DO NOTHING")
        );
        let case = insert_test_case_run_sql();
        assert!(case.contains("authoring_test_case_runs"));
        assert!(case.contains("LEAST("));
        assert!(case.contains("(($3 + 1) * $6)::int"));
        assert!(!case.contains("FROM runs"));
        assert!(!case.contains("JOIN runs"));
    }

    /// wamn-0h0g.8.18: no statement here names a project-local relation, so none
    /// of them can be a cross-database join.
    ///
    /// wamn-0h0g.20.12: the DDL record is scanned by the SAME token list. This
    /// guard used to read only the Rust module, so when wamn-0h0g.8.18 removed
    /// the join from `reconcile_test_report` nothing noticed that the record's
    /// own `BEFORE UPDATE` trigger still rechecked an `effect-uncertain`
    /// outcome against the project run plane — a recheck no write from the
    /// control database could ever satisfy.
    #[test]
    fn no_statement_reaches_the_project_run_plane() {
        let source = include_str!("test_orchestration.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        let ddl = include_str!("../../../../deploy/sql/authoring-tests.sql");
        for (origin, scanned) in [("a statement", implementation), ("the DDL record", ddl)] {
            for project_local in [
                "FROM runs",
                "JOIN runs",
                " runs AS ",
                "run.status",
                "run.run_id",
                "wamn_run.runs",
            ] {
                assert!(
                    !scanned.contains(project_local),
                    "{origin} reaches the project run plane via {project_local}"
                );
            }
        }
        // The already-observed terminal status arrives as a bound parameter.
        assert!(implementation.contains("test_case.run_id = ANY($3::text[])"));
        assert!(implementation.contains("effect_uncertain_run_ids"));
    }

    /// wamn-0h0g.15.123: the report reconcile decides the publish gate's verdict,
    /// and it needs a live client, so its inline statements are pinned as source
    /// text the way the guard above pins the run plane.
    #[test]
    fn the_report_reconcile_cannot_finalize_green_by_omission_or_race() {
        let source = include_str!("test_orchestration.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        let reconcile = implementation
            .split("pub async fn reconcile_test_report(")
            .nth(1)
            .expect("the module defines the report reconcile");
        // Two reconcilers cannot both finalize: the reservation row is locked.
        assert!(reconcile.contains("WHERE tenant_id = $1 AND report_id = $2 FOR UPDATE"));
        // An already finalized report reports its stored verdict, never a new one.
        assert!(reconcile.contains("SELECT passed FROM authoring_test_reports"));
        // Neither unresolved leg may leave a case pending, and each fails it.
        assert!(reconcile.contains("failure_kind = 'effect-uncertain'"));
        assert!(reconcile.contains("failure_kind = 'deadline-exhausted'"));
        assert!(reconcile.contains("clock_timestamp() >= LEAST("));
        assert!(reconcile.contains("test_case.case_deadline_at, reservation.whole_deadline_at)"));
        // The report is written only once no case of it is still pending.
        assert!(reconcile.contains("SELECT count(*) FROM authoring_test_case_runs"));
        assert!(reconcile.contains("if pending != 0 {"));
        // The verdict is the conjunction of every case, never a disjunction.
        assert!(reconcile.contains("bool_and(test_case.passed)"));
        assert!(!reconcile.contains("bool_or("));
    }

    /// wamn-0h0g.15.170: the reconcile pins no resolution map, because the
    /// columns it used to pin one from are deleted.
    #[test]
    fn the_reconcile_pins_no_map_from_a_column_nothing_writes() {
        let source = include_str!("test_orchestration.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        // The dead pin, every fragment of the statement that carried it, and the
        // deleted columns themselves.
        for dead in [
            "SET resolution_map = candidate.resolution_map",
            "resolution_map IS NOT NULL",
            "reservation.resolution_map IS NULL",
            "pin first reconciled test resolution map",
            "COALESCE(reservation.resolution_map",
            "resolution_map_hash",
        ] {
            assert!(
                !implementation.contains(dead),
                "the reconcile still carries the dead resolution-map pin via {dead}"
            );
        }
        // Nothing here re-aggregates a per-case map either (wamn-0h0g.15.7).
        assert!(!implementation.contains("run_flow_resolutions"));
    }

    /// The store these statements now run against.
    #[test]
    fn the_control_store_owns_the_relations_these_statements_name() {
        let ddl = include_str!("../../../../deploy/sql/control-portable-store.sql");
        for table in [
            "authoring_test_run_reservations",
            "authoring_test_case_runs",
            "authoring_test_reports",
        ] {
            assert!(
                ddl.contains(&format!("CREATE TABLE IF NOT EXISTS wamn_run.{table}")),
                "the control store does not own {table}"
            );
        }
        // The ordinal-to-run mapping's uniqueness is what makes a retry converge.
        assert!(ddl.contains("UNIQUE (tenant_id, run_id)"));
        // The project run plane is absent here, which is exactly why the
        // effect-uncertain leg takes observed identities as a parameter.
        assert!(!ddl.contains("CREATE TABLE IF NOT EXISTS wamn_run.runs"));
        assert!(!ddl.contains("authoring_test_sets"));
    }

    #[test]
    fn canonical_ddl_owns_only_test_named_replacement_tables() {
        let ddl = include_str!("../../../../deploy/sql/authoring-tests.sql");
        for table in [
            "authoring_test_run_reservations",
            "authoring_test_case_runs",
            "authoring_test_reports",
        ] {
            assert!(ddl.contains(&format!("CREATE TABLE wamn_run.{table}")));
        }
        assert!(ddl.contains("UNIQUE (tenant_id, run_id)"));
        assert!(!ddl.contains("authoring_test_sets"));
        assert!(!ddl.contains("resolution-map-mismatch"));
        assert!(!ddl.contains("authoring-test-case-resolution-map-required"));
        // wamn-0h0g.15.170: the run-pin cross-check ran only `IF
        // NEW.resolution_map IS NOT NULL`, so it left with that column.
        assert!(!ddl.contains("resolution_map"));
        assert!(!ddl.contains("authoring-test-case-run-pin-mismatch"));
        assert!(ddl.contains("authoring_test_reports_update_immutable"));
    }
}
