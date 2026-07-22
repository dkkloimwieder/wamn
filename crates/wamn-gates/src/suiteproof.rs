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
//!   2. register a flow v1 and seed a suite + cases FROM the `wamn-flow-tests`
//!      envelope — proving the envelope round-trips (`to_json`/`from_json`) and
//!      that the opaque case body reaches the `test_cases.case_body` jsonb intact;
//!   3. assert VERSION BINDING (every suite/case row pins `flow_version = 1`),
//!      RLS (a second tenant's claim sees ZERO suites), and the structural FK
//!      (dropping flow v1 CASCADES its suite + cases).
//!
//! Self-contained: it provisions a fresh schema and drops it at the end.

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::json;
use tokio_postgres::{Client, NoTls};

use wamn_ctl::publish_catalog::{ensure_flow_registry, ensure_flow_tests, ensure_runstate};
use wamn_flow_tests::TestSuite;
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

/// The suite envelope the gate seeds from — the canonical `wamn-flow-tests`
/// shape, flow-version-bound to the flow it tests.
fn suite_envelope() -> TestSuite {
    let json = json!({
        "schema-version": "0.1",
        "flow-id": FLOW_ID,
        "flow-version": 1,
        "suite-id": "smoke",
        "name": "escalate-holds smoke suite",
        "cases": [
            { "case-id": "escalates-stale", "ordinal": 0,
              "case": { "input": { "age-hours": 72 }, "expect": { "status": "escalated" } } },
            { "case-id": "keeps-fresh", "ordinal": 1,
              "case": { "input": { "age-hours": 1 }, "expect": { "status": "open" } } },
        ],
    })
    .to_string();
    TestSuite::from_json(&json).expect("the gate's suite envelope is valid")
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
    provision(&admin, &args.schema).await?;

    // --- envelope round-trip (pure) ---
    let mut ok = true;
    let suite = suite_envelope();
    let round_trips = TestSuite::from_json(&suite.to_json()).is_ok_and(|s| s == suite);
    check(
        &mut ok,
        "ENVELOPE: TestSuite round-trips to_json/from_json",
        round_trips,
    );

    // --- seed a flow v1 + the suite/cases FROM the envelope (app role) ---
    let (app, app_task) = connect(&app_url).await?;
    scope_session(&app, &args.tenant, &args.schema).await?;
    seed_flow_version(&app, &args.tenant, &suite.flow_id, 1, true, "{}", true)
        .await
        .context("register flow v1")?;
    seed_test_suite(
        &app,
        &args.tenant,
        &suite.flow_id,
        suite.flow_version as i32,
        &suite.suite_id,
        &suite.name,
    )
    .await
    .context("seed suite")?;
    for case in &suite.cases {
        seed_test_case(
            &app,
            &args.tenant,
            &suite.flow_id,
            suite.flow_version as i32,
            &suite.suite_id,
            &case.case_id,
            case.ordinal as i32,
            &case.case.to_string(),
        )
        .await
        .context("seed case")?;
    }

    // --- counts + VERSION BINDING (app role, owning tenant) ---
    let suites: i64 = scalar(&app, "SELECT count(*) FROM test_suites").await?;
    let cases: i64 = scalar(&app, "SELECT count(*) FROM test_cases").await?;
    check(
        &mut ok,
        &format!("STORE: one suite seeded (got {suites})"),
        suites == 1,
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
    // The opaque case body reached jsonb intact.
    let body: String = scalar_text(
        &app,
        "SELECT case_body->'expect'->>'status' FROM test_cases WHERE case_id = 'escalates-stale'",
    )
    .await?;
    check(
        &mut ok,
        &format!("STORE: opaque case body preserved (expect.status = {body:?})"),
        body == "escalated",
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
        &[&args.tenant, &suite.flow_id],
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

/// Fresh ephemeral schema + the run-plane / flow-test tables via the SAME
/// `ensure_*` functions `publish-catalog --runstate` uses (production path).
async fn provision(admin: &Client, schema: &str) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
             END IF; END $$;"
        ))
        .await
        .context("reset schema + ensure wamn_app role")?;
    // run-state creates the schema (rewritten header); flows FKs into nothing
    // new; flow-tests FKs into flows, so this ORDER matters.
    ensure_runstate(admin, schema)
        .await
        .context("ensure run-state")?;
    ensure_flow_registry(admin, schema)
        .await
        .context("ensure flow registry")?;
    ensure_flow_tests(admin, schema)
        .await
        .context("ensure flow-test tables")?;
    println!("## provisioned schema {schema} (run-state + flows + test_suites/test_cases)");
    Ok(())
}

async fn scalar(c: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(c.query_one(sql, &[]).await.context("scalar count")?.get(0))
}

async fn scalar_text(c: &Client, sql: &str) -> anyhow::Result<String> {
    Ok(c.query_one(sql, &[]).await.context("scalar text")?.get(0))
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
    fn gate_suite_envelope_is_valid_and_bound() {
        let suite = suite_envelope();
        assert_eq!(suite.flow_id, FLOW_ID);
        assert_eq!(suite.flow_version, 1);
        assert_eq!(suite.cases.len(), 2);
        // Round-trips.
        assert_eq!(TestSuite::from_json(&suite.to_json()).unwrap(), suite);
    }

    #[test]
    fn bare_ident_rejects_injection() {
        assert!(is_bare_ident("wamn_suiteproof"));
        assert!(!is_bare_ident("a; DROP"));
        assert!(!is_bare_ident("Cap"));
    }
}
