//! Durable orchestration state for inline test-set runs.
//!
//! This module owns reservations, immutable run mappings, reconciliation, and
//! report evidence. It deliberately does not admit or execute a case; the
//! sequential executor composes those operations later against these rows.

use std::collections::BTreeSet;

use anyhow::{Context as _, bail};
use serde_json::Value;
use tokio_postgres::{Client, GenericClient};
use wamn_scenario_model::{NamedNodeTerminal, NodeFailureKind, NodeTerminalStatus};

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
    /// The validated-draft hash is the narrowed candidate-override identity.
    pub validated_draft_id: String,
    pub test_set_hash: String,
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
    ResolutionMapMismatch,
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
    pub resolution_map: Option<Value>,
}

impl TestCaseFailure {
    const fn as_sql(self) -> &'static str {
        match self {
            Self::AssertionFailed => "assertion-failed",
            Self::DeadlineExhausted => "deadline-exhausted",
            Self::EffectUncertain => "effect-uncertain",
            Self::ResolutionMapMismatch => "resolution-map-mismatch",
        }
    }
}

/// Insert one pending report reservation.
pub fn insert_test_report_reservation_sql() -> &'static str {
    "INSERT INTO authoring_test_run_reservations \
       (tenant_id, report_id, command_hash, validated_draft_id, test_set_hash, \
        catalog_id, catalog_version, case_count, whole_deadline_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, \
             clock_timestamp() + make_interval(secs => $9::int)) \
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
    "SELECT command_hash, validated_draft_id, test_set_hash, catalog_id, \
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

/// Project frame-keyed node facts through plan identity to stable named nodes.
pub fn select_named_node_observations_sql() -> &'static str {
    "SELECT resolution.flow_id, node.local_node_id, node.status, node.error_kind \
       FROM node_runs AS node \
       JOIN run_flow_resolutions AS resolution \
         ON resolution.tenant_id = node.tenant_id \
        AND resolution.run_id = node.run_id \
        AND resolution.execution_bundle_hash = node.current_plan_hash \
      WHERE node.tenant_id = $1 AND node.run_id = $2 \
        AND node.status IN ('success', 'error') \
      ORDER BY node.frame_id, node.local_node_id, node.occurrence"
}

fn validate_reservation(reservation: &TestReportReservation) -> anyhow::Result<()> {
    for (value, name) in [
        (&reservation.tenant_id, "tenant id"),
        (&reservation.report_id, "report id"),
        (&reservation.validated_draft_id, "validated draft id"),
        (&reservation.test_set_hash, "test-set hash"),
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
    if reservation.case_ids.is_empty()
        || reservation.case_ids.len() > wamn_scenario_model::MAX_TEST_SET_CASES
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
                &reservation.test_set_hash,
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
    let state: String = row.get(6);
    if row.get::<_, String>(0) != expected.command_hash
        || row.get::<_, String>(1) != expected.validated_draft_id
        || row.get::<_, String>(2) != expected.test_set_hash
        || row.get::<_, String>(3) != expected.catalog_id
        || row.get::<_, i32>(4) != expected.catalog_version
        || row.get::<_, i32>(5) != expected_count
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
                    resolution_map = $6::jsonb, \
                    resolution_map_hash = CASE WHEN $6::jsonb IS NULL THEN NULL ELSE \
                        'sha256:' || encode(sha256(convert_to($6::jsonb::text, 'UTF8')), 'hex') END, \
                    summary = $7::jsonb, finalized_at = clock_timestamp() \
              WHERE tenant_id = $1 AND report_id = $2 AND ordinal = $3 \
                AND state = 'pending'",
            &[
                &finalization.tenant_id,
                &finalization.report_id,
                &finalization.ordinal,
                &finalization.passed,
                &failure,
                &finalization.resolution_map,
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
pub async fn reconcile_test_report(
    client: &mut Client,
    tenant_id: &str,
    report_id: &str,
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
               FROM runs AS run \
              WHERE test_case.tenant_id = $1 AND test_case.report_id = $2 \
                AND test_case.state = 'pending' \
                AND run.tenant_id = test_case.tenant_id \
                AND run.run_id = test_case.run_id \
                AND run.status = 'effect-uncertain'",
            &[&tenant_id, &report_id],
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
    transaction
        .execute(
            "UPDATE authoring_test_run_reservations AS reservation \
                SET resolution_map = candidate.resolution_map, \
                    resolution_map_hash = 'sha256:' || encode(sha256( \
                        convert_to(candidate.resolution_map::text, 'UTF8')), 'hex') \
               FROM ( \
                    SELECT resolution_map \
                      FROM authoring_test_case_runs \
                     WHERE tenant_id = $1 AND report_id = $2 \
                       AND resolution_map IS NOT NULL \
                     ORDER BY ordinal LIMIT 1 \
               ) AS candidate \
              WHERE reservation.tenant_id = $1 AND reservation.report_id = $2 \
                AND reservation.state = 'pending' \
                AND reservation.resolution_map IS NULL",
            &[&tenant_id, &report_id],
        )
        .await
        .context("pin first reconciled test resolution map")?;
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
                    (tenant_id, report_id, validated_draft_id, test_set_hash, \
                     catalog_id, catalog_version, resolution_map, resolution_map_hash, \
                     passed, summary) \
                SELECT reservation.tenant_id, reservation.report_id, \
                       reservation.validated_draft_id, reservation.test_set_hash, \
                       reservation.catalog_id, reservation.catalog_version, \
                       COALESCE(reservation.resolution_map, '{}'::jsonb), \
                       'sha256:' || encode(sha256(convert_to( \
                           COALESCE(reservation.resolution_map, '{}'::jsonb)::text, 'UTF8')), 'hex'), \
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

/// Load all terminal named-node observations without collapsing frames/visits.
pub async fn named_node_observations<C>(
    client: &C,
    tenant_id: &str,
    run_id: &str,
) -> anyhow::Result<Vec<NamedNodeTerminal>>
where
    C: GenericClient + Sync,
{
    let rows = client
        .query(select_named_node_observations_sql(), &[&tenant_id, &run_id])
        .await
        .context("project named-node observations")?;
    rows.into_iter()
        .map(|row| {
            let status: String = row.get(2);
            let failure: Option<String> = row.get(3);
            let (status, failure_kind) = match (status.as_str(), failure.as_deref()) {
                ("success", None) => (NodeTerminalStatus::Success, None),
                ("error", Some("retryable")) => {
                    (NodeTerminalStatus::Error, Some(NodeFailureKind::Retryable))
                }
                ("error", Some("rate-limited")) => (
                    NodeTerminalStatus::Error,
                    Some(NodeFailureKind::RateLimited),
                ),
                ("error", Some("terminal")) => {
                    (NodeTerminalStatus::Error, Some(NodeFailureKind::Terminal))
                }
                ("error", Some("invalid-input")) => (
                    NodeTerminalStatus::Error,
                    Some(NodeFailureKind::InvalidInput),
                ),
                _ => bail!("invalid terminal node fact in test report projection"),
            };
            Ok(NamedNodeTerminal {
                flow_id: row.get(0),
                node_id: row.get(1),
                status,
                failure_kind,
            })
        })
        .collect()
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
            test_set_hash: format!("sha256:{}", "b".repeat(64)),
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
                    * i32::try_from(wamn_scenario_model::MAX_TEST_SET_CASES).unwrap()
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
    fn statements_pin_normalized_idempotency_and_named_node_projection() {
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

        let projection = select_named_node_observations_sql();
        assert!(projection.contains("resolution.execution_bundle_hash = node.current_plan_hash"));
        assert!(projection.contains("resolution.flow_id, node.local_node_id"));
        assert!(projection.contains("node.frame_id"));
        assert!(!projection.contains("DISTINCT"));
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
        assert!(ddl.contains("REFERENCES wamn_run.authoring_test_sets"));
        assert!(ddl.contains("resolution-map-mismatch"));
        assert!(ddl.contains("authoring-test-case-resolution-map-required"));
        assert!(ddl.contains("'{principal,validated-draft-hash}' = NEW.validated_draft_id"));
        assert!(ddl.contains("authoring_test_reports_update_immutable"));
    }
}
