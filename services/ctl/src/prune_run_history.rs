//! The `prune-run-history` subcommand (9.6 retention, wamn-srb): the effect shell
//! for the pure `wamn_run_state::sql::prune_terminal_runs_sql` builder.
//!
//! Deletes a project-env's TERMINAL run history older than a retention window so
//! the `runs` HOT store stays bounded (platform-plan risk #5). Only runs in a
//! terminal state (completed / failed / infrastructure-failure) are eligible; a
//! `dispatched`/`running` run is never pruned.
//!
//! **Role:** connects as a scoped `wamn_run_retention` credential GENERATION
//! (`wamn-0h0g.12.69`), never as the shared `wamn_app` login. That family's
//! stable ACL role holds `DELETE` plus a three-column `SELECT` on `wamn_run.runs`
//! and nothing else anywhere in the cluster, so the credential this verb mounts
//! can express this statement and no other. The `run_queue` row goes with the run
//! through its `ON DELETE CASCADE`, which needs no privilege of its own.
//! Idempotent and safe to repeat on a cadence
//! (`deploy/platform/run-retention.example.yaml`).
//!
//! **The tenant is proven, not asserted.** `--tenant` used to be a free argument
//! that reached the statement as an `app.tenant` GUC, which fails in two
//! directions at once: an unset claim makes the predicate NULL, so the delete
//! removes nothing and the verb still prints a success line; and a claim naming
//! somebody else's tenant is honoured, because `wamn_run.runs`' one permissive
//! floor arm is `TO wamn_platform USING (true)` and every platform-grain family
//! matches it without a tenant predicate. [`verify_retention_identity`] closes
//! both by DERIVING the only tenant this credential may prune from the connected
//! role name and refusing anything else before a statement runs. The tenant then
//! reaches the delete as a bound parameter.
//!
//! **v0 is age-based only:** every terminal run is independently eligible once
//! it ages beyond the configured window.

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
};

#[derive(Debug, Args)]
pub struct PruneRunHistoryArgs {
    /// Postgres URL for this tenant's `wamn_run_retention` credential generation
    /// — the NOSUPERUSER/NOBYPASSRLS role whose whole authority is the terminal
    /// run delete. The verb refuses any other identity. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// The run-plane schema the `runs` table lives in (set as the session
    /// `search_path`). Bare identifier, and REQUIRED: the statement this
    /// verb drives is `DELETE FROM runs` — UNQUALIFIED — so it resolves through
    /// that session `search_path`. A default would let an invocation that omits
    /// the flag prune a relation the operator never named and still report
    /// success.
    #[arg(long)]
    pub schema: String,

    /// The tenant whose run history to prune. It must be the tenant the mounted
    /// retention credential was minted for; a mismatch refuses loudly rather
    /// than pruning nothing and reporting success.
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
        bail!("--tenant must be non-empty (it is the tenant the delete is bound to)");
    }

    let (mut client, conn) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("retention credential connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        verify_retention_identity(&client, &args.tenant).await?;
        prune(
            &mut client,
            &args.schema,
            &args.tenant,
            args.retention_days,
            !args.dry_run,
        )
        .await
    }
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
             (tenant {})",
            args.retention_days, args.schema, args.tenant
        );
    }
    Ok(())
}

/// Refuse unless the connected role is one of the two credential generations
/// this exact `(tenant, database)` pair mints (`wamn-0h0g.12.69`).
///
/// # This is the negative-negative guard, and it has to be here
///
/// The statement below cannot enforce it. `wamn_run.runs` FORCEs row-level
/// security whose only arm a platform-grain family matches is
/// `TO wamn_platform USING (true)`, so a retention credential's rows are not
/// narrowed by the server at all — measured on PostgreSQL 18.6, a generation
/// minted for one tenant deletes another tenant's terminal runs, and their queue
/// rows cascade with them. PostgreSQL privileges are relation- and
/// column-shaped, never row-shaped, so the grant set cannot express it either.
///
/// What the server DOES supply is an unforgeable identity: the generation login
/// name carries the 160-bit scope digest of `(tenant, database)` under the
/// retention family's own domain, and `current_user` cannot be rewritten by a
/// session. Deriving the expected names from `--tenant` and `current_database()`
/// and comparing turns a free CLI argument into a claim the credential has to
/// back. A missing, foreign or mismatched tenant therefore ERRORS here, instead
/// of reaching a predicate that matches nothing and reporting `pruned 0 terminal
/// run(s)` with exit 0.
///
/// The database is read from the SERVER (`current_database()`), never parsed out
/// of the URL: the URL is operator input, and checking a digest against the same
/// input that chose it would prove nothing.
async fn verify_retention_identity(
    client: &tokio_postgres::Client,
    tenant: &str,
) -> anyhow::Result<()> {
    let row = client
        .query_one(
            "SELECT current_user::text AS role, current_database()::text AS database",
            &[],
        )
        .await
        .context("read connected retention identity")?;
    let role: String = row.get("role");
    let database: String = row.get("database");

    let scope = WorkloadRoleScope::Tenant {
        tenant,
        database: &database,
    };
    let mut expected = Vec::new();
    for generation in [CredentialGeneration::A, CredentialGeneration::B] {
        expected.push(
            workload_generation_role(WorkloadRoleFamily::Retention, scope, generation)
                .context("derive the expected retention generation identity")?,
        );
    }
    if expected.contains(&role) {
        return Ok(());
    }
    bail!(
        "refusing to prune: connected as {role:?} in database {database:?}, which is not a \
         run-retention credential generation for tenant {tenant:?} (expected one of {expected:?}). \
         Pruning under any other identity either carries authority this verb does not need or \
         would delete nothing and report success."
    )
}

/// The reusable core: pin the session `search_path` to the project, then run the
/// pure prune statement with the tenant BOUND. When `apply`, the delete commits;
/// otherwise it runs inside a rolled-back transaction so `dry_run` reports the
/// exact affected count without mutating. Returns the number of `runs` rows
/// removed. The live retention gate (`tests/integration/src/retention.rs`)
/// drives this same path through the `wamn-ctl-ops` process, so verb and gate
/// exercise ONE code path.
///
/// No `app.tenant` claim is injected any more. That GUC keyed the RETIRED floor;
/// `wamn-0h0g.22.6` re-keyed `runs` onto `current_user`, and the one thing the
/// claim still did here was supply a predicate that silently matched nothing
/// when it was absent. The `run_queue` cascade does not need it: measured, a
/// referential-integrity trigger consults neither the deleter's grants nor that
/// relation's own `app.tenant` policy.
pub async fn prune(
    client: &mut tokio_postgres::Client,
    schema: &str,
    tenant: &str,
    retention_days: u32,
    apply: bool,
) -> anyhow::Result<u64> {
    // Bound as a parameter (set_config), never interpolated into SQL.
    // Session-level (`false`) so the transaction below inherits it.
    client
        .execute("SELECT set_config('search_path', $1, false)", &[&schema])
        .await
        .context("set search_path")?;

    let days = i64::from(retention_days);
    let sql = wamn_run_state::sql::prune_terminal_runs_sql();
    if apply {
        client
            .execute(&sql, &[&tenant, &days])
            .await
            .context("prune terminal runs")
    } else {
        let tx = client.transaction().await.context("begin dry-run tx")?;
        let n = tx
            .execute(&sql, &[&tenant, &days])
            .await
            .context("prune terminal runs (dry-run)")?;
        tx.rollback().await.context("roll back dry-run")?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two identities the verb accepts are the A/B generations of ONE
    /// `(tenant, database)` scope, and another tenant's digest differs — which
    /// is the whole basis of the refusal, since a login name cannot be made to
    /// claim a tenant it was not minted for.
    #[test]
    fn the_expected_retention_identities_are_the_two_generations_of_one_scope() {
        let scope = WorkloadRoleScope::Tenant {
            tenant: "acme-prod",
            database: "wamn-db-acme--billing--dev--ab12cd",
        };
        let a = workload_generation_role(
            WorkloadRoleFamily::Retention,
            scope,
            CredentialGeneration::A,
        )
        .expect("retention is a tenant-scoped family");
        let b = workload_generation_role(
            WorkloadRoleFamily::Retention,
            scope,
            CredentialGeneration::B,
        )
        .expect("retention is a tenant-scoped family");
        assert!(a.starts_with("wamn_run_retention_"), "{a}");
        assert!(a.ends_with("_a"), "{a}");
        assert!(b.ends_with("_b"), "{b}");
        assert_eq!(a[..a.len() - 1], b[..b.len() - 1]);
        let other = workload_generation_role(
            WorkloadRoleFamily::Retention,
            WorkloadRoleScope::Tenant {
                tenant: "other-tenant",
                database: "wamn-db-acme--billing--dev--ab12cd",
            },
            CredentialGeneration::A,
        )
        .expect("retention is a tenant-scoped family");
        assert_ne!(a, other);
        // The same tenant in a DIFFERENT database is a different credential too:
        // the digest frames both fields, so a retention Secret cannot be moved
        // between project-env databases.
        let elsewhere = workload_generation_role(
            WorkloadRoleFamily::Retention,
            WorkloadRoleScope::Tenant {
                tenant: "acme-prod",
                database: "wamn-db-acme--billing--prod--ef34ab",
            },
            CredentialGeneration::A,
        )
        .expect("retention is a tenant-scoped family");
        assert_ne!(a, elsewhere);
    }

    /// The statement the verb drives binds the tenant and never reads the
    /// retired `app.tenant` claim.
    #[test]
    fn the_prune_statement_binds_the_tenant_and_reads_no_guc() {
        let sql = wamn_run_state::sql::prune_terminal_runs_sql();
        assert!(sql.contains("WHERE tenant_id = $1"), "{sql}");
        assert!(!sql.contains("app.tenant"), "{sql}");
        assert!(!sql.contains("current_setting"), "{sql}");
    }
}
