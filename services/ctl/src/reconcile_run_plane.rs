//! The `reconcile-run-plane` subcommand (E4/R14-migration, wamn-1wdq): the
//! **effect shell** for the pure `wamn_schema_control` run-plane schema reconciler —
//! THE durable migration path for provisioned run-plane schemas.
//!
//! `deploy/sql/run-state.sql` / `flows.sql` / `run-queue.sql` evolve, but
//! nothing migrated schemas instantiated from older revisions: the live demo
//! schemas broke on the E4 `stream_seq` column (runner 42703 warn-loops), one
//! env had NO queue tables at all, and the ephemeral fixture restart wiped
//! everything including the `catalog` metadata schema. This verb reads what ONE
//! project-env schema actually has (tables, columns, indexes, CHECKs, user
//! triggers, helper functions, legacy outbox-era objects, and the per-database
//! `catalog` schema), asks the pure planner
//! (`wamn_schema_control::plan_run_plane`) for the idempotent plan,
//! and — unless `--dry-run` — executes it, in order:
//!
//! - missing tables from their record sections (from-zero restore included),
//! - `ADD COLUMN` for record columns a present table lacks,
//! - record indexes created / a stale-definition index (the pre-E4 claimable
//!   index) recreated,
//! - exact record CHECK constraints plus run-state helper functions and the
//!   event-lineage trigger (missing/drifted definitions repaired; extra record
//!   CHECKs/triggers removed),
//! - the pre-l5i9.19 outbox-era teardown (tables, triggers, function, the
//!   legacy registration `state` keys),
//! - the `catalog` metadata schema when absent (or its missing tables).
//!
//! **Data preserving:** no live column, non-legacy table, or data row is ever
//! dropped; live columns the record does not know are printed, not touched.
//! PostgreSQL validates each canonical CHECK against existing rows and the verb
//! fails loudly rather than rewriting incompatible history.
//!
//! **Ownership:** CREATE/ALTER/DROP need table ownership, and the legacy-attempt
//! backfill must see every tenant through forced RLS. `wamn_app` and a plain
//! schema owner cannot safely run it, so apply requires an administrative role
//! with `SUPERUSER` or explicit `BYPASSRLS`, like `publish-catalog --provision`
//! / `reconcile-replica-identity`.
//!
//! **Scope:** strictly the `--schema` project-env schema plus the per-database
//! `catalog` metadata schema; entity/floor tables in the schema are read for
//! the legacy-trigger survey only and never altered (the floor is
//! `publish-catalog --provision` / `migrate-catalog` territory, and flow/seed
//! CONTENT restore stays `publish-catalog --flow` / `--seed-dataset`).
//!
//! `--dry-run` is STRICTLY read-only: it neither ensures the `wamn_app` role
//! nor executes any plan action.

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

use wamn_schema_control::{
    BareSchemaName, RunPlaneObservation, RunPlanePlan, ScenarioAuthorRoleObservation,
    catalog_schema_present_sql, count_legacy_effect_attempt_rows_sql,
    count_stale_registration_state_sql, plan_run_plane, select_app_scenario_author_membership_sql,
    select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_outbox_function_present_sql,
    select_outbox_trigger_tables_sql, select_run_plane_helper_functions_sql,
    select_scenario_author_catalog_lock_privilege_sql, select_scenario_author_role_sql,
    select_scenario_author_schema_usage_sql, select_schema_checks_sql, select_schema_columns_sql,
    select_schema_foreign_keys_sql, select_schema_indexes_sql, select_schema_triggers_sql,
};

#[derive(Debug, Args)]
pub struct ReconcileRunPlaneArgs {
    /// Administrative Postgres URL to the project database. Observation and
    /// apply require SUPERUSER or BYPASSRLS so forced-RLS legacy rows cannot
    /// be skipped. Env `WAMN_PG_ADMIN_URL`.
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
    let schema = BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid --schema {:?}", args.schema))?;
    let (client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = reconcile(&client, &schema, !args.dry_run).await;
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
    schema: &BareSchemaName,
    apply: bool,
) -> anyhow::Result<RunPlanePlan> {
    let bypasses_forced_rls: bool = client
        .query_one(
            "SELECT rolsuper OR rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = CURRENT_USER",
            &[],
        )
        .await
        .context("verify reconcile admin bypasses forced RLS")?
        .get(0);
    anyhow::ensure!(
        bypasses_forced_rls,
        "reconcile-run-plane requires SUPERUSER or BYPASSRLS; a plain schema owner cannot completely observe forced-RLS legacy rows"
    );

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
    schema: &BareSchemaName,
) -> anyhow::Result<RunPlaneObservation> {
    let mut obs = RunPlaneObservation {
        scenario_author_role: client
            .query_opt(select_scenario_author_role_sql(), &[])
            .await
            .context("read scenario-author role attributes")?
            .map(|row| ScenarioAuthorRoleObservation {
                can_login: row.get(0),
                is_superuser: row.get(1),
                can_create_database: row.get(2),
                can_create_role: row.get(3),
                inherits_roles: row.get(4),
                can_replicate: row.get(5),
                bypasses_rls: row.get(6),
            }),
        ..Default::default()
    };

    obs.app_is_scenario_author_member = client
        .query_one(select_app_scenario_author_membership_sql(), &[])
        .await
        .context("read guest scenario-author membership")?
        .get(0);
    obs.scenario_author_can_lock_catalog_head = client
        .query_one(
            select_scenario_author_catalog_lock_privilege_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read scenario-author catalog-lock privilege")?
        .get(0);
    for row in client
        .query(select_authoring_table_privileges_sql(), &[&schema.as_str()])
        .await
        .context("read authoring table privileges")?
    {
        obs.authoring_table_privileges
            .entry((row.get(0), row.get(1), row.get(2)))
            .or_default()
            .insert(row.get(3));
    }
    for row in client
        .query(
            select_authoring_effective_table_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effective authoring table privileges")?
    {
        obs.authoring_effective_table_privileges
            .entry((row.get(0), row.get(1), row.get(2)))
            .or_default()
            .insert(row.get(3));
    }
    for row in client
        .query(
            select_authoring_effective_column_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effective authoring column privileges")?
    {
        obs.authoring_effective_column_privileges
            .entry((row.get(0), row.get(1), row.get(2)))
            .or_default()
            .insert(row.get(3));
    }
    for row in client
        .query(select_authoring_table_owners_sql(), &[&schema.as_str()])
        .await
        .context("read authoring table owners")?
    {
        obs.authoring_table_owners
            .insert((row.get(0), row.get(1)), row.get(2));
    }
    for row in client
        .query(
            select_scenario_author_schema_usage_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read scenario-author schema usage")?
    {
        let schema_name: String = row.get(0);
        let has_usage: bool = row.get(1);
        if has_usage {
            obs.scenario_author_schema_usage.insert(schema_name);
        }
    }

    for row in client
        .query(select_schema_columns_sql(), &[&schema.as_str()])
        .await
        .context("read schema tables/columns")?
    {
        let table: String = row.get(0);
        let column: String = row.get(1);
        obs.tables.entry(table).or_default().insert(column);
    }
    for row in client
        .query(select_schema_indexes_sql(), &[&schema.as_str()])
        .await
        .context("read schema indexes")?
    {
        obs.indexes.insert(row.get(0), row.get(1));
    }
    for row in client
        .query(select_schema_checks_sql(), &[&schema.as_str()])
        .await
        .context("read schema check constraints")?
    {
        obs.checks.insert((row.get(0), row.get(1)), row.get(2));
    }
    for row in client
        .query(select_schema_foreign_keys_sql(), &[&schema.as_str()])
        .await
        .context("read schema foreign keys")?
    {
        obs.foreign_keys
            .insert((row.get(0), row.get(1)), row.get(2));
    }
    for row in client
        .query(select_schema_triggers_sql(), &[&schema.as_str()])
        .await
        .context("read schema triggers")?
    {
        obs.triggers.insert((row.get(0), row.get(1)), row.get(2));
    }
    for row in client
        .query(select_run_plane_helper_functions_sql(), &[&schema.as_str()])
        .await
        .context("read run-plane helper functions")?
    {
        obs.helper_functions.insert(row.get(0), row.get(1));
    }
    for row in client
        .query(select_outbox_trigger_tables_sql(), &[&schema.as_str()])
        .await
        .context("survey legacy outbox triggers")?
    {
        obs.outbox_trigger_tables.push(row.get(0));
    }
    obs.outbox_function_present = client
        .query_one(select_outbox_function_present_sql(), &[&schema.as_str()])
        .await
        .context("survey legacy outbox function")?
        .get(0);

    if obs.tables.get("node_runs").is_some_and(|columns| {
        columns.contains("current_effect_attempt_id")
            && [
                "attempt",
                "selected_recovery_class",
                "recovery_class",
                "generation_fact_kind",
                "connection_generation",
                "credential_generation",
                "attempt_started_at",
                "attempt_dispatched_at",
                "attempt_deadline_at",
                "attempt_input_ref",
                "attempt_key",
            ]
            .iter()
            .all(|column| columns.contains(*column))
    }) {
        obs.legacy_effect_attempt_rows = client
            .query_one(&count_legacy_effect_attempt_rows_sql(schema), &[])
            .await
            .context("count legacy effect-attempt rows")?
            .get(0);
    }

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
            let column: String = row.get(1);
            obs.catalog_tables.insert(table.clone());
            obs.catalog_columns.entry(table).or_default().insert(column);
        }
        for row in client
            .query(select_schema_checks_sql(), &[&"catalog"])
            .await
            .context("read catalog check constraints")?
        {
            obs.catalog_checks
                .insert((row.get(0), row.get(1)), row.get(2));
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
