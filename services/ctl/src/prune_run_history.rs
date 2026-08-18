//! The `prune-run-history` subcommand (9.6 retention, wamn-srb): the effect shell
//! for the pure `wamn_run_state::sql::prune_terminal_runs_sql` builder.
//!
//! Deletes a project-env's TERMINAL run history older than a retention window so
//! the `runs`/`node_runs` HOT store stays bounded (node I/O snapshots are the
//! biggest storage-cost driver — platform-plan risk #5). Only runs in a terminal
//! state (completed / failed / infrastructure-failure) are eligible; a
//! `dispatched`/`running` run is never pruned.
//!
//! **Role:** connects as the APP role (`wamn_app`, NOSUPERUSER/NOBYPASSRLS) under
//! the tenant floor — unlike the schema-owning verbs, this is an ordinary
//! DELETE the app role is granted. The delete is scoped to `--tenant`'s
//! `app.tenant` claim (RLS + the explicit predicate), and `node_runs` plus any
//! stale `run_queue` row cascade via their `ON DELETE CASCADE` FK to `runs`.
//! Idempotent and safe to repeat on a cadence
//! (`deploy/platform/run-retention.example.yaml`).
//!
//! **v0 is age-based only:** every terminal run is independently eligible once
//! it ages beyond the configured window.

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

#[derive(Debug, Args)]
pub struct PruneRunHistoryArgs {
    /// App (wamn_app) Postgres URL to the project-env database. The prune is an
    /// ordinary tenant-scoped DELETE the app role is granted — no superuser
    /// needed. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// The run-plane schema the `runs`/`node_runs` tables live in (set as the
    /// session `search_path`). Bare identifier, and REQUIRED: the statement this
    /// verb drives is `DELETE FROM runs` — UNQUALIFIED — so it resolves through
    /// that session `search_path`. A default would let an invocation that omits
    /// the flag prune a relation the operator never named and still report
    /// success.
    #[arg(long)]
    pub schema: String,

    /// The tenant whose run history to prune — the `app.tenant` claim RLS scopes
    /// the delete to (a project-env's runs never cross tenants).
    #[arg(long)]
    pub tenant: String,

    /// Prune terminal runs whose `created_at` is older than this many days.
    #[arg(long)]
    pub retention_days: u32,

    /// Count what WOULD be pruned (a rolled-back delete under the same predicate)
    /// without deleting anything.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: PruneRunHistoryArgs) -> anyhow::Result<()> {
    if !crate::migrate_catalog::is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    if args.tenant.trim().is_empty() {
        bail!("--tenant must be non-empty (it is the app.tenant claim the delete is scoped to)");
    }

    let (mut client, conn) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    let conn_task = tokio::spawn(conn);
    let result = prune(
        &mut client,
        &args.schema,
        &args.tenant,
        args.retention_days,
        !args.dry_run,
    )
    .await;
    drop(client);
    let _ = conn_task.await;
    let pruned = result?;

    if args.dry_run {
        println!(
            "prune-run-history (dry-run): {pruned} terminal run(s) older than {} day(s) WOULD be \
             pruned in schema {} (tenant {})",
            args.retention_days, args.schema, args.tenant
        );
    } else {
        println!(
            "prune-run-history: pruned {pruned} terminal run(s) older than {} day(s) in schema {} \
             (tenant {}) — node_runs cascaded",
            args.retention_days, args.schema, args.tenant
        );
    }
    Ok(())
}

/// The reusable core: pin the session to the project (`search_path` + tenant
/// claim), then run the pure prune statement. When `apply`, the delete commits;
/// otherwise it runs inside a rolled-back transaction so `dry_run` reports the
/// exact affected count without mutating. Returns the number of `runs` rows
/// removed. Shared by the CLI verb and the capturebench retention gate so both
/// exercise ONE code path.
pub async fn prune(
    client: &mut tokio_postgres::Client,
    schema: &str,
    tenant: &str,
    retention_days: u32,
    apply: bool,
) -> anyhow::Result<u64> {
    // Both GUCs bound as parameters (set_config) — the tenant is arbitrary text,
    // never interpolated into SQL. Session-level (`false`) so the transaction
    // below inherits them.
    client
        .execute("SELECT set_config('search_path', $1, false)", &[&schema])
        .await
        .context("set search_path")?;
    client
        .execute("SELECT set_config('app.tenant', $1, false)", &[&tenant])
        .await
        .context("set app.tenant claim")?;

    let days = i64::from(retention_days);
    let sql = wamn_run_state::sql::prune_terminal_runs_sql();
    if apply {
        client
            .execute(&sql, &[&days])
            .await
            .context("prune terminal runs")
    } else {
        let tx = client.transaction().await.context("begin dry-run tx")?;
        let n = tx
            .execute(&sql, &[&days])
            .await
            .context("prune terminal runs (dry-run)")?;
        tx.rollback().await.context("roll back dry-run")?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    /// wamn-0h0g.12.115: `--schema` carries no default, and must not reacquire
    /// one. The prune statement is `DELETE FROM runs` — unqualified
    /// (`wamn_run_state::sql::prune_terminal_runs_sql`) — resolved through the
    /// SESSION-level `search_path` [`super::prune`] sets with
    /// `set_config(..., false)`. A default therefore does not fail closed: an
    /// invocation that omits the flag deletes some other project-env's canonical
    /// history, prints a success line naming the schema it chose for itself, and
    /// exits 0. Both in-repo callers already name the schema, so the default
    /// served nobody and only enabled that silent mistarget.
    #[test]
    fn schema_carries_no_default_and_must_be_operator_named() {
        let implementation = include_str!("prune_run_history.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        assert!(!implementation.contains("default_value"));
        assert!(implementation.contains("#[arg(long)]\n    pub schema: String,"));
    }
}
