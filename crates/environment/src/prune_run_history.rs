//! The `prune-run-history` subcommand (9.6 retention, wamn-srb): the effect shell
//! for the pure `wamn_run_state::sql::prune_terminal_runs_sql` builder.
//!
//! Deletes a project-env's TERMINAL run history older than a retention window so
//! the `runs` HOT store stays bounded (platform-plan risk #5). Only runs in a
//! terminal state (completed / failed / infrastructure-failure) are eligible; a
//! `dispatched`/`running` run is never pruned.
//!
//! **Role:** connects as the project's `wamn_platform` login. The `run_queue`
//! row goes with the run through its `ON DELETE CASCADE`.
//! Repeated calls are safe.
//!
//! The targeted database credential selects the project.
//!
//! **v0 is age-based only:** every terminal run is independently eligible once
//! it ages beyond the configured window.

use anyhow::{Context as _, bail};
use sqlx::Connection as _;

/// Inputs of one terminal run history prune.
#[derive(Debug)]
pub struct PruneRunHistoryRequest {
    /// Postgres URL for this project's `wamn_platform` login
    /// — the NOSUPERUSER/NOBYPASSRLS role whose whole authority is the terminal
    /// run delete. The verb refuses any other identity.
    pub database_url: String,

    /// The run-plane schema the `runs` table lives in (set as the session
    /// `search_path`). Bare identifier, and REQUIRED: the statement this
    /// verb drives is `DELETE FROM runs` — UNQUALIFIED — so it resolves through
    /// that session `search_path`. A default would let an invocation that omits
    /// the schema prune a relation the operator never named and still report
    /// success.
    pub schema: String,

    /// Prune terminal runs whose `created_at` is older than this many days.
    pub retention_days: u32,

    /// Count what WOULD be pruned (a rolled-back delete under the same predicate)
    /// without deleting anything.
    pub dry_run: bool,
}

/// Prune one project's expired terminal run history and return the number of
/// `runs` rows the delete removed. A dry run rolls back and returns the count
/// the same delete would have removed.
pub async fn prune_terminal_run_history(request: PruneRunHistoryRequest) -> anyhow::Result<u64> {
    if !crate::ident::is_bare_ident(&request.schema) {
        bail!(
            "the schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            request.schema
        );
    }

    let mut client = wamn_postgres::connect(
        &wamn_provision::workload_role::platform_configuration(&request.database_url)?,
    )
    .await
    .context("retention credential connect")?;

    let result = async {
        prune(
            &mut client,
            &request.schema,
            request.retention_days,
            !request.dry_run,
        )
        .await
    }
    .await;
    drop(client);
    result
}

/// The reusable core: pin the session `search_path` to the project, then run the
/// pure prune statement. When `apply`, the delete commits;
/// otherwise it runs inside a rolled-back transaction so `dry_run` reports the
/// exact affected count without mutating. Returns the number of `runs` rows
/// removed.
///
/// Bind the retention component before reading or deleting protected rows.
pub async fn prune(
    client: &mut sqlx::PgConnection,
    schema: &str,
    retention_days: u32,
    apply: bool,
) -> anyhow::Result<u64> {
    let mut transaction = client
        .begin()
        .await
        .context("begin run history retention")?;
    wamn_provision::bind_platform_principal(
        &mut transaction,
        wamn_provision::PlatformComponent::AuditRetention,
    )
    .await
    .context("bind retention principal")?;
    sqlx::query("SELECT set_config('search_path', $1, true)")
        .bind(schema)
        .execute(&mut *transaction)
        .await
        .context("set run history schema")?;
    let count = wamn_run_state::persistence::prune_terminal_runs(&mut transaction, retention_days)
        .await
        .context("prune terminal runs")?;
    if apply {
        transaction
            .commit()
            .await
            .context("commit run history retention")?;
    } else {
        transaction
            .rollback()
            .await
            .context("roll back run history preview")?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {}
