//! The `retention` subcommand: the live `prune-run-history` gate.
//!
//! Pure host-side (no wasm guest): it applies the REAL
//! `deploy/sql/run-state.sql` into a throwaway ephemeral schema, seeds aged run
//! history, then drives the REAL `prune-run-history` verb through the
//! `wamn-ctl-ops` process — the same
//! `wamn_run_state::sql::prune_terminal_runs_sql` builder production uses — and
//! asserts that only OLD TERMINAL runs are removed.
//!
//! Extracted from the retired `capturebench` harness (wamn-x1gy). The capture
//! phases this stood beside asserted subjects that no longer exist:
//! `wamn_run.node_runs` was dropped by wamn-0h0g.15.82 (see the standing
//! comment in `deploy/sql/run-state.sql`) and `catalog.execution_bundles` by
//! 4802daac. Retention is a live ops verb and this is its only watcher, so the
//! phase survives its harness — under a name that says what it proves.

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};

use crate::ctl_process;

const SCHEMA: &str = "wamn_retention";
const CATALOG_SCHEMA: &str = "wamn_retention_catalog";
const TENANT: &str = "retention-t";
const CATALOG_ID: &str = "retention-fixture";
const RETENTION_DAYS: &str = "30";

#[derive(Debug, Args)]
pub struct RetentionArgs {
    /// App (wamn_app) Postgres URL — the NOSUPERUSER/NOBYPASSRLS role the prune
    /// verb deletes as. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: applies/drops the ephemeral run-state schema.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Ephemeral schema: the REAL run-state.sql, schema-rewritten (no stand-in DDL,
// so the `runs` shape can never drift from the schema of record). Only the
// `catalog.releases` foreign key reaches outside the run plane, so
// that is the only fixture relation this gate stands up.
// ---------------------------------------------------------------------------

fn run_state_ddl() -> String {
    include_str!("../../../deploy/sql/run-state.sql")
        .replace("wamn_run", SCHEMA)
        .replace("catalog.releases", &format!("{CATALOG_SCHEMA}.releases"))
}

async fn admin_exec(admin_url: &str, sql: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    // `.context`, not `anyhow!("{e}")`: tokio_postgres::Error's Display is only
    // its kind ("db error"); the server's message — e.g. `role
    // "wamn_scenario_author" does not exist` — hangs off `source()`. Formatting
    // the error away collapsed every provisioning failure to "admin exec: db
    // error"; chaining keeps the cause in the `Caused by:` report.
    let r = client.batch_execute(sql).await.context("admin exec");
    drop(client);
    let _ = conn_task.await;
    r
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS {CATALOG_SCHEMA} CASCADE; \
             CREATE SCHEMA {CATALOG_SCHEMA}; \
             CREATE TABLE {CATALOG_SCHEMA}.releases ( \
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL, \
               PRIMARY KEY (tenant_id, catalog_id, catalog_version) \
             ); \
             INSERT INTO {CATALOG_SCHEMA}.releases \
             VALUES ('{TENANT}','{CATALOG_ID}',1);"
        ),
    )
    .await?;
    admin_exec(admin_url, &run_state_ddl()).await?;
    admin_exec(
        admin_url,
        &format!(
            "INSERT INTO {SCHEMA}.environment_policies \
               (tenant_id, expected_environment, durability_class) \
             VALUES ('{TENANT}', 'test', 'standard');"
        ),
    )
    .await
}

async fn teardown(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS {CATALOG_SCHEMA} CASCADE;"
        ),
    )
    .await
}

/// A wamn_app session pinned to the retention schema + tenant claim.
async fn connect_app(app_url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!(
            "SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("set search_path + tenant claim")?;
    Ok((client, handle))
}

/// Insert a `runs` row whose `created_at` is `now()` shifted back `age_days`
/// days, so the gate can seed aged history. Fixture-only superuser seed:
/// production run admission is available only through the private native
/// run-state adapter.
async fn seed_run(
    admin_url: &str,
    run_id: &str,
    status: &str,
    age_days: i64,
) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin seed-run connect")?;
    let conn_task = tokio::spawn(conn);
    let result = client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs ( \
                   tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
                   environment, status, created_at \
                 ) VALUES ($1, $2, 'f', 1, '{CATALOG_ID}', 1, 'test', $3, \
                           now() - ($4::bigint * interval '1 day'))"
            ),
            &[&TENANT, &run_id, &status, &age_days],
        )
        .await
        .context("seed run");
    drop(client);
    let _ = conn_task.await;
    result.map(|_| ())
}

async fn run_exists(client: &Client, run_id: &str) -> anyhow::Result<bool> {
    Ok(client
        .query_one("SELECT count(*) FROM runs WHERE run_id = $1", &[&run_id])
        .await?
        .get::<_, i64>(0)
        == 1)
}

async fn retention_gate(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!(
        "\n## retention — the real prune-run-history verb prunes old TERMINAL runs, \
         keeps recent + non-terminal"
    );
    let (app, _h) = connect_app(app_url).await?;

    // Seed: an old completed run, a recent completed run, and an OLD but RUNNING
    // run (the terminal-only guard).
    seed_run(admin_url, "old-done", "completed", 40).await?;
    seed_run(admin_url, "recent-done", "completed", 1).await?;
    seed_run(admin_url, "old-running", "running", 40).await?;

    let prune = ctl_process::run_ops_checked([
        "prune-run-history",
        "--database-url",
        app_url,
        "--schema",
        SCHEMA,
        "--tenant",
        TENANT,
        "--retention-days",
        RETENTION_DAYS,
    ])
    .await
    .context("prune through wamn-ctl-ops")?;
    let prune_stdout = String::from_utf8(prune.stdout).context("prune output is UTF-8")?;
    let reported_one = prune_stdout.contains("pruned 1 terminal run(s)");

    let old_gone = !run_exists(&app, "old-done").await?;
    let recent_kept = run_exists(&app, "recent-done").await?;
    let running_kept = run_exists(&app, "old-running").await?;
    let pass = reported_one && old_gone && recent_kept && running_kept;
    println!(
        "  reported_one={reported_one} old_gone={old_gone} recent_kept={recent_kept} \
         running_kept={running_kept}"
    );
    println!("PASS(retention: old terminal pruned, recent/running kept): {pass}");
    Ok(pass)
}

pub async fn run(args: RetentionArgs) -> anyhow::Result<()> {
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "retention needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;

    println!("# wamn-gates retention (schema {SCHEMA}, tenant {TENANT})");
    provision(&admin_url)
        .await
        .context("provision ephemeral run-state schema")?;

    let outcome = retention_gate(&app_url, &admin_url).await;

    let _ = teardown(&admin_url).await;
    let pass = outcome?;

    println!("\nretention complete — overall PASS: {pass}");
    if !pass {
        bail!("the prune-run-history retention gate failed");
    }
    Ok(())
}
