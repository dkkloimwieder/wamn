//! suiteproof — the 11.2 "test cases as catalog data" gate (wamn-828).
//!
//! The in-cluster gate-of-record candidate for flow test suites stored as data,
//! versioned WITH the flow they test. It runs the whole arc against a throwaway
//! Postgres (`WAMN_PG_ADMIN_URL` superuser to provision, `WAMN_PG_URL` app role
//! for the RLS-scoped reads) in an EPHEMERAL schema it owns end to end:
//!
//!   1. provision the run-plane + the `deploy/sql/flow-tests.sql` tables through
//!      the SAME `ensure_*` code path production provisioning uses
//!      (`publish-catalog --runstate`);
//!   2. register a flow v1 and seed a suite + cases FROM the `wamn-scenario-catalog`
//!      envelope — proving the envelope round-trips (`to_json`/`from_json`) and
//!      that the opaque case body reaches the `test_cases.case_body` jsonb intact;
//!   3. assert VERSION BINDING (every suite/case row pins `flow_version = 1`),
//!      RLS (a second tenant's claim sees ZERO suites), and the structural FK
//!      (dropping flow v1 CASCADES its suite + cases).
//!
//! Self-contained: it provisions a fresh schema and drops it at the end.

use anyhow::{Context as _, bail};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_postgres::{Client, NoTls};

use crate::ctl_process;
use wamn_gate_harness::{check, scope_session, seed_flow_version, seed_test_case, seed_test_suite};

const FLOW_ID: &str = "escalate-holds";

#[derive(Debug, Args)]
pub struct SuiteProofArgs {
    /// App (wamn_app, NOSUPERUSER) Postgres URL. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL — provisions the ephemeral schema + run-plane/test tables.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The ephemeral schema this gate owns (dropped at the end).
    #[arg(long, default_value = "wamn_suiteproof")]
    pub schema: String,

    /// The owning tenant the suite is seeded under.
    #[arg(long, default_value = "demo-tenant")]
    pub tenant: String,

    /// A second tenant that must see ZERO suites (RLS negative).
    #[arg(long, default_value = "other-tenant")]
    pub other_tenant: String,

    /// Keep the schema at the end (default drops it).
    #[arg(long)]
    pub keep: bool,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct StoredTestEnvelope {
    schema_version: String,
    flow_id: String,
    flow_version: u32,
    suite_id: String,
    name: String,
    cases: Vec<StoredTestEntry>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct StoredTestEntry {
    case_id: String,
    ordinal: u32,
    case: serde_json::Value,
}

/// The persisted test envelope the compatibility gate seeds.
fn stored_test_envelope() -> StoredTestEnvelope {
    serde_json::from_value(json!({
        "schema-version": "0.1",
        "flow-id": FLOW_ID,
        "flow-version": 1,
        "suite-id": "smoke",
        "name": "escalate-holds smoke suite",
        "cases": [
            { "case-id": "escalates-stale", "ordinal": 0,
              "case": {
                  "schema-version": "0.1",
                  "name": "escalates-stale",
                  "flow-ref": { "flow-id": FLOW_ID, "version": 1 },
                  "input": { "age-hours": 72 },
                  "expect": [ { "run-terminal-outcome": { "status": "completed" } } ]
              } },
            { "case-id": "keeps-fresh", "ordinal": 1,
              "case": {
                  "schema-version": "0.1",
                  "name": "keeps-fresh",
                  "flow-ref": { "flow-id": FLOW_ID, "version": 1 },
                  "input": { "age-hours": 1 },
                  "expect": [ { "run-terminal-outcome": { "status": "completed" } } ]
              } },
        ],
    }))
    .expect("the gate's persisted test envelope is valid")
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .context("postgres connect")?;
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok((client, task))
}

pub async fn run(args: SuiteProofArgs) -> anyhow::Result<()> {
    if !is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    println!(
        "# wamn-gates suiteproof — 11.2 test cases as catalog data (schema {}, tenant {})",
        args.schema, args.tenant
    );

    // --- provision (superuser) through the production ensure_* path ---
    let (admin, admin_task) = connect(&admin_url).await?;
    provision(&admin, &admin_url, &args.schema).await?;

    // --- envelope round-trip (pure) ---
    let mut ok = true;
    let stored_test = stored_test_envelope();
    let encoded =
        serde_json::to_string(&stored_test).context("serialize persisted test envelope")?;
    let round_trips = serde_json::from_str::<StoredTestEnvelope>(&encoded)
        .is_ok_and(|decoded| decoded == stored_test);
    check(
        &mut ok,
        "ENVELOPE: persisted test data round-trips through JSON",
        round_trips,
    );

    // --- seed a flow v1 + the suite/cases FROM the envelope (app role) ---
    let (app, app_task) = connect(&app_url).await?;
    scope_session(&app, &args.tenant, &args.schema).await?;
    seed_flow_version(
        &app,
        &args.tenant,
        &stored_test.flow_id,
        1,
        true,
        "{}",
        true,
    )
    .await
    .context("register flow v1")?;
    seed_test_suite(
        &app,
        &args.tenant,
        &stored_test.flow_id,
        i32::try_from(stored_test.flow_version).context("stored test flow version exceeds i32")?,
        &stored_test.suite_id,
        &stored_test.name,
    )
    .await
    .context("seed suite")?;
    for case in &stored_test.cases {
        seed_test_case(
            &app,
            &args.tenant,
            &stored_test.flow_id,
            i32::try_from(stored_test.flow_version)
                .context("stored test flow version exceeds i32")?,
            &stored_test.suite_id,
            &case.case_id,
            i32::try_from(case.ordinal).context("stored test ordinal exceeds i32")?,
            &case.case.to_string(),
        )
        .await
        .context("seed case")?;
    }

    // --- counts + VERSION BINDING (app role, owning tenant) ---
    let stored_test_count: i64 = scalar(&app, "SELECT count(*) FROM test_suites").await?;
    let cases: i64 = scalar(&app, "SELECT count(*) FROM test_cases").await?;
    check(
        &mut ok,
        &format!("STORE: one persisted test set seeded (got {stored_test_count})"),
        stored_test_count == 1,
    );
    check(
        &mut ok,
        &format!("STORE: two cases seeded (got {cases})"),
        cases == 2,
    );
    let bound: i64 = scalar(
        &app,
        "SELECT count(*) FROM test_cases WHERE flow_version = 1",
    )
    .await?;
    check(
        &mut ok,
        &format!("BIND: every case pins flow_version = 1 (got {bound})"),
        bound == 2,
    );
    // The opaque case body reached jsonb intact (round-trips to the seeded body)
    // The opaque case body reaches jsonb intact.
    let stored: serde_json::Value = app
        .query_one(
            "SELECT case_body FROM test_cases WHERE case_id = 'escalates-stale'",
            &[],
        )
        .await
        .context("read stored case body")?
        .get(0);
    let seeded = &stored_test.cases[0].case;
    check(
        &mut ok,
        "STORE: opaque case body round-trips through jsonb intact",
        &stored == seeded,
    );

    // --- RLS: a second tenant's claim sees ZERO suites ---
    let (other, other_task) = connect(&app_url).await?;
    scope_session(&other, &args.other_tenant, &args.schema).await?;
    let other_sees: i64 = scalar(&other, "SELECT count(*) FROM test_suites").await?;
    check(
        &mut ok,
        &format!("RLS: a foreign tenant sees no suites (got {other_sees})"),
        other_sees == 0,
    );
    drop(other);
    let _ = other_task.await;

    // --- FK cascade (version binding is structural): drop flow v1 → suite gone ---
    app.execute(
        "DELETE FROM flows WHERE tenant_id = $1 AND flow_id = $2 AND version = 1",
        &[&args.tenant, &stored_test.flow_id],
    )
    .await
    .context("delete flow v1")?;
    let after_suites: i64 = scalar(&app, "SELECT count(*) FROM test_suites").await?;
    let after_cases: i64 = scalar(&app, "SELECT count(*) FROM test_cases").await?;
    check(
        &mut ok,
        &format!("FK: dropping flow v1 cascaded its suite (got {after_suites})"),
        after_suites == 0,
    );
    check(
        &mut ok,
        &format!("FK: and its cases (got {after_cases})"),
        after_cases == 0,
    );

    drop(app);
    let _ = app_task.await;

    // --- teardown ---
    if !args.keep {
        admin
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", args.schema))
            .await
            .context("drop ephemeral schema")?;
    }
    drop(admin);
    let _ = admin_task.await;

    println!("\nsuiteproof complete — overall PASS: {ok}");
    if !ok {
        bail!("suiteproof failed");
    }
    Ok(())
}

/// Fresh ephemeral schema reconciled through the production ctl boundary.
async fn provision(admin: &Client, admin_url: &str, schema: &str) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
             END IF; END $$;"
        ))
        .await
        .context("reset schema + ensure wamn_app role")?;
    ctl_process::run_checked([
        "reconcile-run-plane",
        "--admin-database-url",
        admin_url,
        "--schema",
        schema,
    ])
    .await
    .context("reconcile run-plane through wamn-ctl")?;
    println!("## provisioned schema {schema} (run-state + flows + test_suites/test_cases)");
    Ok(())
}

async fn scalar(c: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(c.query_one(sql, &[]).await.context("scalar count")?.get(0))
}

/// A bare lowercase SQL identifier (the ephemeral schema is interpolated).
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate's seeded suite is a valid, version-bound envelope — a broken
    /// fixture fails here, not only against a live Postgres.
    #[test]
    fn gate_stored_test_envelope_is_valid_and_bound() {
        let stored_test = stored_test_envelope();
        assert_eq!(stored_test.flow_id, FLOW_ID);
        assert_eq!(stored_test.flow_version, 1);
        assert_eq!(stored_test.cases.len(), 2);
        // Round-trips.
        let encoded = serde_json::to_string(&stored_test).unwrap();
        assert_eq!(
            serde_json::from_str::<StoredTestEnvelope>(&encoded).unwrap(),
            stored_test
        );
    }

    #[test]
    fn bare_ident_rejects_injection() {
        assert!(is_bare_ident("wamn_suiteproof"));
        assert!(!is_bare_ident("a; DROP"));
        assert!(!is_bare_ident("Cap"));
    }

    #[test]
    fn canonical_stored_test_ddl_enforces_tenant_and_version_lifetime() {
        let ddl = include_str!("../../../deploy/sql/flow-tests.sql");
        for table in ["test_suites", "test_cases"] {
            assert!(
                ddl.contains(&format!(
                    "ALTER TABLE wamn_run.{table} FORCE ROW LEVEL SECURITY"
                )),
                "{table} must remain under forced tenant RLS"
            );
        }
        assert!(
            ddl.contains(
                "REFERENCES wamn_run.flows (tenant_id, flow_id, version) ON DELETE CASCADE"
            ),
            "dropping a pinned flow version must cascade its suite"
        );
        assert!(
            ddl.contains(
                "REFERENCES wamn_run.test_suites (tenant_id, flow_id, flow_version, suite_id) ON DELETE CASCADE"
            ),
            "dropping a suite must cascade its cases"
        );
    }
}
