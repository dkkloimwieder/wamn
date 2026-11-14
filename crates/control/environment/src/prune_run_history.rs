//! The `prune-run-history` subcommand (9.6 retention, wamn-srb): the effect shell
//! for the pure `wamn_run_state::sql::prune_terminal_runs_sql` builder.
//!
//! Deletes a project-env's TERMINAL run history older than a retention window so
//! the `runs` HOT store stays bounded (platform-plan risk #5). Only runs in a
//! terminal state (completed / failed / infrastructure-failure) are eligible; a
//! `dispatched`/`running` run is never pruned.
//!
//! **Role:** connects as a targeted `wamn_run_retention` credential GENERATION
//! (`wamn-0h0g.12.69`), never as the shared `wamn_app` login. That family's
//! stable ACL role holds `DELETE` plus a three-column `SELECT` on `wamn_run.runs`
//! and nothing else anywhere in the cluster, so the credential this verb mounts
//! can express this statement and no other. The `run_queue` row goes with the run
//! through its `ON DELETE CASCADE`, which needs no privilege of its own.
//! Repeated calls are safe.
//!
//! The verb tests the project identity. `--tenant` used to be a free argument
//! that reached the statement as an `app.project_env_key` GUC, which fails in two
//! directions at once: an unset identity makes the predicate NULL, so the delete
//! removes nothing and the verb still prints a success line; and an identity naming
//! somebody else's project is honoured, because `wamn_run.runs`' one permissive
//! base policy arm is `TO wamn_platform USING (true)` and every platform-grain family
//! matches it without a project predicate. [`verify_retention_identity`] closes
//! both by DERIVING the only project this credential may prune from the connected
//! role name and refusing anything else before a statement runs. The project then
//! reaches the delete as a bound parameter.
//!
//! **v0 is age-based only:** every terminal run is independently eligible once
//! it ages beyond the configured window.

use anyhow::{Context as _, bail};
use sqlx::{Connection as _, Row as _};
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};

/// Inputs of one terminal run history prune.
#[derive(Debug)]
pub struct PruneRunHistoryRequest {
    /// Postgres URL for this project's `wamn_run_retention` login
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

    pub org: String,
    pub project: String,
    pub environment: String,

    /// Prune terminal runs whose `created_at` is older than this many days.
    pub retention_days: u32,

    /// Count what WOULD be pruned (a rolled-back delete under the same predicate)
    /// without deleting anything.
    pub dry_run: bool,
}

impl PruneRunHistoryRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
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
    if [&request.org, &request.project, &request.environment]
        .iter()
        .any(|part| part.trim().is_empty())
    {
        bail!("the project must be non-empty (it is the project the delete is bound to)");
    }

    let mut client = wamn_postgres::connect(&request.database_url)
        .await
        .context("retention credential connect")?;

    let result = async {
        verify_retention_identity(&mut client, &request.project_env_key()).await?;
        prune(
            &mut client,
            &request.schema,
            &request.project_env_key(),
            request.retention_days,
            !request.dry_run,
        )
        .await
    }
    .await;
    drop(client);
    result
}

/// Refuse unless the connected role is the logins
/// this exact `(project, database)` pair mints (`wamn-0h0g.12.69`).
///
/// # This is the negative-negative guard, and it has to be here
///
/// The statement below cannot enforce it. `wamn_run.runs` FORCEs row-level
/// security whose only arm a platform-grain family matches is
/// `TO wamn_platform USING (true)`, so a retention credential's rows are not
/// narrowed by the server at all — measured on PostgreSQL 18.6, a login
/// created for one project deletes another project's terminal runs, and their queue
/// rows cascade with them. PostgreSQL privileges are relation- and
/// column-shaped, never row-shaped, so the grant set cannot express it either.
///
/// What the server DOES supply is an unforgeable identity: the login
/// name carries the 160-bit target digest of `(project, database)` under the
/// retention family's own domain, and `current_user` cannot be rewritten by a
/// session. Deriving the expected name from the environment coordinate and `current_database()`
/// and comparing turns a free CLI argument into an identity the credential has to
/// back. A missing, foreign or mismatched project therefore ERRORS here, instead
/// of reaching a predicate that matches nothing and reporting `pruned 0 terminal
/// run(s)` with exit 0.
///
/// The database is read from the SERVER (`current_database()`), never parsed out
/// of the URL: the URL is operator input, and testing a digest against the same
/// input that chose it establishes nothing.
async fn verify_retention_identity(
    client: &mut sqlx::PgConnection,
    project_env_key: &str,
) -> anyhow::Result<()> {
    let row =
        sqlx::query("SELECT current_user::text AS role, current_database()::text AS database")
            .fetch_one(&mut *client)
            .await
            .context("read connected retention identity")?;
    let role: String = row.get("role");
    let database: String = row.get("database");

    let target = WorkloadRoleIdentity::ProjectEnvKey {
        project_env_key,
        database: &database,
    };
    let expected = workload_login(WorkloadRoleFamily::Retention, target)
        .context("derive the expected retention identity")?;
    if expected == role {
        return Ok(());
    }
    bail!(
        "refusing to prune: connected as {role:?} in database {database:?}, which is not a \
         run-retention login for project_env_key {project_env_key:?} (expected {expected:?}). \
         Pruning under any other identity either carries authority this verb does not need or \
         would delete nothing and report success."
    )
}

/// The reusable core: pin the session `search_path` to the project, then run the
/// pure prune statement with the project BOUND. When `apply`, the delete commits;
/// otherwise it runs inside a rolled-back transaction so `dry_run` reports the
/// exact affected count without mutating. Returns the number of `runs` rows
/// removed.
///
/// No `app.project_env_key` identity is injected any more. That GUC keyed the RETIRED minimum;
/// `wamn-0h0g.22.6` re-keyed `runs` onto `current_user`, and the one thing the
/// identity still did here was supply a predicate that silently matched nothing
/// when it was absent. The `run_queue` cascade does not need it: measured, a
/// referential-integrity trigger consults neither the deleter's grants nor that
/// relation's own `app.project_env_key` policy.
pub async fn prune(
    client: &mut sqlx::PgConnection,
    schema: &str,
    project_env_key: &str,
    retention_days: u32,
    apply: bool,
) -> anyhow::Result<u64> {
    // Bound as a parameter (set_config), never interpolated into SQL.
    // Session-level (`false`) so the transaction below inherits it.
    sqlx::query("SELECT set_config('search_path', $1, false)")
        .bind(schema)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .context("set search_path")?;

    if apply {
        wamn_run_state::persistence::prune_terminal_runs(client, project_env_key, retention_days)
            .await
            .context("prune terminal runs")
    } else {
        let mut tx = client.begin().await.context("begin dry-run transaction")?;
        let count = wamn_run_state::persistence::prune_terminal_runs(
            &mut tx,
            project_env_key,
            retention_days,
        )
        .await
        .context("prune terminal runs (dry-run)")?;
        tx.rollback().await.context("roll back dry-run")?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {}
