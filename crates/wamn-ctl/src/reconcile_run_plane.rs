//! The `reconcile-run-plane` subcommand (E4/R14-migration, wamn-1wdq): the
//! **effect shell** for the pure `wamn_migrate` run-plane schema reconciler —
//! THE durable migration path for provisioned run-plane schemas.
//!
//! `deploy/sql/run-state.sql` / `flows.sql` / `run-queue.sql` evolve, but
//! nothing migrated schemas instantiated from older revisions: the live demo
//! schemas broke on the E4 `stream_seq` column (runner 42703 warn-loops), one
//! env had NO queue tables at all, and the ephemeral fixture restart wiped
//! everything including the `catalog` metadata schema. This verb reads what ONE
//! project-env schema actually has (tables, columns, index definitions, legacy
//! outbox-era objects, the per-database `catalog` schema), asks the pure
//! planner (`wamn_migrate::plan_run_plane`) for the idempotent ADDITIVE plan,
//! and — unless `--dry-run` — executes it, in order:
//!
//! - missing tables from their record sections (from-zero restore included),
//! - `ADD COLUMN` for record columns a present table lacks,
//! - record indexes created / a stale-definition index (the pre-E4 claimable
//!   index) recreated,
//! - the pre-l5i9.19 outbox-era teardown (tables, triggers, function, the
//!   legacy registration `state` keys),
//! - the `catalog` metadata schema when absent (or its missing tables).
//!
//! **Additive:** no live column, no non-legacy table, and no data row is ever
//! dropped; live columns the record does not know are printed, not touched.
//! Constraint drift on an existing column (a legacy `fail_kind` CHECK) is the
//! wamn-fqg.16 sibling class, deliberately not covered.
//!
//! **Ownership:** CREATE/ALTER/DROP need table ownership — `wamn_app` cannot
//! run them — so this connects as a **superuser** (or the schema owner), like
//! `publish-catalog --provision` / `reconcile-replica-identity`.
//!
//! **Scope:** strictly the `--schema` project-env schema plus the per-database
//! `catalog` metadata schema; entity/floor tables in the schema are read for
//! the legacy-trigger survey only and never altered (the floor is
//! `publish-catalog --provision` / `migrate-catalog` territory, and flow/seed
//! CONTENT restore stays `publish-catalog --flow` / `--seed-dataset`).
//!
//! `--dry-run` is STRICTLY read-only: it neither ensures the `wamn_app` role
//! nor executes any plan action.

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

use wamn_migrate::{
    RunPlaneObservation, RunPlanePlan, catalog_schema_present_sql,
    count_stale_registration_state_sql, plan_run_plane, select_outbox_function_present_sql,
    select_outbox_trigger_tables_sql, select_schema_columns_sql, select_schema_indexes_sql,
};

#[derive(Debug, Args)]
pub struct ReconcileRunPlaneArgs {
    /// Superuser Postgres URL to the project database. CREATE/ALTER/DROP need
    /// table ownership, so a superuser/schema-owner is required. Env
    /// `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// The project-env schema the run-plane tables live in (e.g.
    /// `wamn_runner_demo`, `poc_f1`).
    #[arg(long)]
    pub schema: String,

    /// Print the reconcile plan without applying it (strictly read-only).
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: ReconcileRunPlaneArgs) -> anyhow::Result<()> {
    if !crate::migrate_catalog::is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    let (client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = reconcile(&client, &args.schema, !args.dry_run).await;
    drop(client);
    let _ = conn_task.await;
    let plan = result?;

    print_plan(&plan, args.dry_run);
    Ok(())
}

/// The reusable core: observe the schema, plan, and — when `apply` — ensure the
/// `wamn_app` role the sections GRANT to and execute the actions in order.
/// Returns the plan (for reporting / gate assertions). Shared by the CLI verb
/// and the live gate so both exercise one code path.
pub async fn reconcile(
    client: &tokio_postgres::Client,
    schema: &str,
    apply: bool,
) -> anyhow::Result<RunPlanePlan> {
    let obs = observe(client, schema).await?;
    let plan = plan_run_plane(schema, &obs);
    if apply {
        crate::publish_catalog::ensure_wamn_app_role(client).await?;
        for action in &plan.actions {
            client
                .batch_execute(&action.sql)
                .await
                .with_context(|| format!("apply {:?} {}", action.kind, action.target))?;
        }
    }
    Ok(plan)
}

/// Read everything the pure planner decides on. Read-only.
async fn observe(
    client: &tokio_postgres::Client,
    schema: &str,
) -> anyhow::Result<RunPlaneObservation> {
    let mut obs = RunPlaneObservation::default();

    for row in client
        .query(select_schema_columns_sql(), &[&schema])
        .await
        .context("read schema tables/columns")?
    {
        let table: String = row.get(0);
        let column: String = row.get(1);
        obs.tables.entry(table).or_default().insert(column);
    }
    for row in client
        .query(select_schema_indexes_sql(), &[&schema])
        .await
        .context("read schema indexes")?
    {
        obs.indexes.insert(row.get(0), row.get(1));
    }
    for row in client
        .query(select_outbox_trigger_tables_sql(), &[&schema])
        .await
        .context("survey legacy outbox triggers")?
    {
        obs.outbox_trigger_tables.push(row.get(0));
    }
    obs.outbox_function_present = client
        .query_one(select_outbox_function_present_sql(), &[&schema])
        .await
        .context("survey legacy outbox function")?
        .get(0);

    obs.catalog_schema_present = client
        .query_one(catalog_schema_present_sql(), &[])
        .await
        .context("probe catalog schema")?
        .get(0);
    if obs.catalog_schema_present {
        for row in client
            .query(select_schema_columns_sql(), &[&"catalog"])
            .await
            .context("read catalog tables")?
        {
            let table: String = row.get(0);
            obs.catalog_tables.insert(table);
        }
        if obs.catalog_tables.contains("event_registrations") {
            obs.stale_registration_state_rows = client
                .query_one(count_stale_registration_state_sql(), &[])
                .await
                .context("count legacy registration state keys")?
                .get(0);
        }
    }
    Ok(obs)
}

fn print_plan(plan: &RunPlanePlan, dry_run: bool) {
    let verb = if dry_run { "would apply" } else { "applied" };
    if plan.is_noop() {
        println!(
            "run plane already at the schema of record — no actions ({} tables at target)",
            plan.at_target.len()
        );
    } else {
        for a in &plan.actions {
            println!("{verb} {:?}: {}", a.kind, a.target);
        }
    }
    for (table, col) in &plan.extra_columns {
        println!("  [extra] {table}.{col} is not in the schema of record — left untouched");
    }
}
