//! The publish gate ([11.7], wamn-12g) — `copy-project-env`'s side of §11.7.
//!
//! This module is the ADAPTER only. Both decisions it enforces are pure and live
//! in libraries, so the future authenticated management transport
//! (wamn-ftfc.33) reaches the same verdicts without depending on this binary:
//!
//! * *does a rule apply?* — `wamn_control_registry::resolve_publish_policy`
//!   over the T1 registry's org default + per-project override;
//! * *is the rule satisfied?* — `wamn_schema_control::publish_gate::evaluate`
//!   over the impact report's suite selectors and the durable suite reports.
//!
//! Here we only fetch rows, call those two, print, and append the verdict to
//! `catalog.publish_gate_audit`.
//!
//! # Where each fact is read
//!
//! The POLICY is T1 registry state, keyed by the DESTINATION `(org, project,
//! env)` — "prod deploys require green suite" is a rule about what may enter
//! prod. The EVIDENCE is read from the SOURCE project-env, because that is where
//! the suites ran; a suite cannot have been run in the destination against a
//! release that has not been promoted there yet. The VERDICT is written to the
//! destination catalog, next to the release it governs.
//!
//! # Refusals are recorded
//!
//! The ledger row is appended BEFORE the refusal propagates, so a blocked deploy
//! leaves durable evidence. Everything else about a refusal is inert: the gate
//! runs before the definition transaction opens, so nothing else is mutated.

use std::collections::BTreeMap;

use anyhow::Context as _;
use wamn_control_registry::{PublishPolicy, resolve_publish_policy};
use wamn_schema_control::Catalog;
use wamn_schema_control::publish_gate::{PublishGateReport, ReleaseLineage, SuiteReportRow};

/// The verb recorded in the ledger — this gate's caller.
const VERB: &str = "copy-project-env";

/// The identity a gate run is about: which project-env promotes into which.
pub(crate) struct GateTarget<'a> {
    pub org: &'a str,
    pub project: &'a str,
    pub source_env: &'a str,
    pub target_env: &'a str,
    pub tenant: &'a str,
}

/// Resolve the publish policy for the DESTINATION `(org, project, env)`.
///
/// An org that configures no such env is an ERROR, not an ungated pass: the
/// promotion names a destination the registry does not know, and answering
/// "no rule" for an unknown target is how a gate silently stops gating.
pub(crate) async fn read_policy(
    system: &tokio_postgres::Client,
    target: &GateTarget<'_>,
) -> anyhow::Result<PublishPolicy> {
    let row = system
        .query_opt(
            wamn_control_registry::sql::select_publish_policy_sql(),
            &[&target.org, &target.target_env, &target.project],
        )
        .await
        .context("read the publish policy from the T1 registry")?
        .with_context(|| {
            format!(
                "org {:?} has no env policy for {:?} — the publish gate cannot decide whether \
                 this promotion is allowed (stamp the org's env policies first)",
                target.org, target.target_env,
            )
        })?;
    Ok(resolve_publish_policy(row.get(0), row.get(1)))
}

/// Read the durable suite reports the SOURCE env holds for one catalog.
///
/// A report whose lineage is not a well-formed RELEASE lineage contributes a
/// row with `release: None` — the pure decision calls that `draft-only` rather
/// than silently dropping it, so "your evidence is a draft run" is a message the
/// operator actually sees.
pub(crate) async fn read_suite_reports(
    src: &tokio_postgres::Client,
    flow_schema: &str,
    tenant: &str,
    catalog_id: &str,
) -> anyhow::Result<Vec<SuiteReportRow>> {
    if !table_present(src, &format!("{flow_schema}.authoring_suite_reports")).await? {
        return Ok(Vec::new());
    }
    let rows = src
        .query(
            &wamn_schema_control::sql::select_suite_reports_for_catalog_sql(flow_schema),
            &[&tenant, &catalog_id],
        )
        .await
        .context("read durable suite reports for the publish gate")?;
    Ok(rows
        .iter()
        .map(|row| {
            let lineage_json: String = row.get(5);
            SuiteReportRow {
                suite: wamn_schema_control::impact::SuiteEdge {
                    tenant: tenant.to_string(),
                    flow_id: row.get(0),
                    flow_version: row.get(1),
                    suite_id: row.get(2),
                },
                report_id: row.get(3),
                passed: row.get(4),
                release: parse_release_lineage(&lineage_json),
            }
        })
        .collect())
}

/// Extract the release half of a stored `lineage_json`; `None` for a draft
/// lineage (or any shape this build does not recognize as a release).
fn parse_release_lineage(lineage_json: &str) -> Option<ReleaseLineage> {
    let value: serde_json::Value = serde_json::from_str(lineage_json).ok()?;
    if value.get("kind")?.as_str()? != "release" {
        return None;
    }
    Some(ReleaseLineage {
        artifact_hash: value.get("artifact-hash")?.as_str()?.to_string(),
        catalog_id: value.get("catalog-id")?.as_str()?.to_string(),
        catalog_version: i32::try_from(value.get("catalog-version")?.as_i64()?).ok()?,
        environment: value.get("environment")?.as_str()?.to_string(),
    })
}

/// The artifact hash the SOURCE's immutable release pins per flow — the
/// freshness authority the gate compares recorded lineage against.
pub(crate) async fn read_release_artifact_hashes(
    src: &tokio_postgres::Client,
    tenant: &str,
    catalog_id: &str,
    catalog_version: i32,
) -> anyhow::Result<BTreeMap<String, String>> {
    if !table_present(src, "catalog.release_flows").await? {
        return Ok(BTreeMap::new());
    }
    let rows = src
        .query(
            wamn_schema_control::sql::select_release_flow_artifact_hashes_sql(),
            &[&tenant, &catalog_id, &catalog_version],
        )
        .await
        .context("read the release's pinned flow artifact hashes")?;
    Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
}

/// Append one verdict to the destination's append-only ledger.
///
/// Best-effort is NOT acceptable here for a refusal — an unrecordable refusal is
/// reported as an error, because a change-control gate whose log silently drops
/// its blocks is worse than no log.
pub(crate) async fn record_verdict(
    dst: &tokio_postgres::Client,
    target: &GateTarget<'_>,
    policy: &PublishPolicy,
    catalog: &Catalog,
    report: &PublishGateReport,
) -> anyhow::Result<()> {
    let findings = serde_json::to_string(&report.findings).context("serialize gate findings")?;
    let catalog_version = i32::try_from(catalog.version)
        .with_context(|| format!("catalog version {} exceeds i32", catalog.version))?;
    dst.execute(
        wamn_schema_control::sql::record_publish_gate_sql(),
        &[
            &target.tenant,
            &VERB,
            &report.decision(),
            &policy.requires_green_suite,
            &policy.source.as_str(),
            &target.org,
            &target.project,
            &target.source_env,
            &target.target_env,
            &catalog.catalog_id,
            &catalog_version,
            &findings,
        ],
    )
    .await
    .context("append the publish-gate verdict to catalog.publish_gate_audit")?;
    Ok(())
}

async fn table_present(client: &tokio_postgres::Client, qualified: &str) -> anyhow::Result<bool> {
    let present: Option<String> = client
        .query_one("SELECT to_regclass($1)::text", &[&qualified])
        .await
        .with_context(|| format!("probe {qualified}"))?
        .get(0);
    Ok(present.is_some())
}

#[cfg(test)]
mod tests {
    use super::parse_release_lineage;

    /// The adapter must read the EXACT field spelling
    /// `wamn_scenario_model::ExecutionLineage` serializes (kebab-case, `kind`
    /// tag) — a mismatch would make every report look like a draft, i.e. turn
    /// the gate into a blanket refusal.
    #[test]
    fn release_lineage_parses_the_serialized_execution_lineage_shape() {
        let json = r#"{"kind":"release","artifact-hash":"h1","catalog-id":"ops",
                       "catalog-version":7,"environment":"dev"}"#;
        let release = parse_release_lineage(json).expect("release lineage parses");
        assert_eq!(release.artifact_hash, "h1");
        assert_eq!(release.catalog_id, "ops");
        assert_eq!(release.catalog_version, 7);
        assert_eq!(release.environment, "dev");
    }

    #[test]
    fn a_draft_lineage_is_not_a_release() {
        let json = r#"{"kind":"draft","draft-artifact-hash":"h1","runtime-flow-version":1,
                       "execution-bundle-hash":"b","validated-draft-hash":"v",
                       "catalog-id":"ops","catalog-version":7,"environment":"dev"}"#;
        assert!(parse_release_lineage(json).is_none());
    }

    /// A lineage missing any identity field cannot be evidence — it must not
    /// parse into a half-populated release.
    #[test]
    fn an_incomplete_release_lineage_is_refused() {
        for json in [
            r#"{"kind":"release","catalog-id":"ops","catalog-version":7,"environment":"dev"}"#,
            r#"{"kind":"release","artifact-hash":"h1","catalog-version":7,"environment":"dev"}"#,
            r#"{"kind":"release","artifact-hash":"h1","catalog-id":"ops","environment":"dev"}"#,
            r#"{"kind":"release","artifact-hash":"h1","catalog-id":"ops","catalog-version":7}"#,
            "not json",
        ] {
            assert!(
                parse_release_lineage(json).is_none(),
                "incomplete lineage must not parse: {json}"
            );
        }
    }
}
