//! The `reconcile-run-plane` subcommand (E4/R14-migration, wamn-1wdq): the
//! **effect shell** for the pure `wamn_schema_control` run-plane schema reconciler —
//! THE durable migration path for provisioned run-plane schemas.
//!
//! `deploy/sql/run-state.sql` / `run-queue.sql` evolve, but
//! nothing migrated schemas instantiated from older revisions: the live demo
//! schemas broke on the E4 `stream_seq` column (runner 42703 warn-loops), one
//! env had NO queue table at all, and the ephemeral fixture restart wiped
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
//! - the pre-l5i9.19 outbox-era teardown (tables, triggers, function, and
//!   retired registration `state` keys) plus retired `partition-key` cleanup,
//! - the `catalog` metadata schema when absent (or its missing tables).
//!
//! **Retained-data preserving:** no retained table or row is rewritten or
//! deleted, and unknown live columns are printed rather than touched. Explicit
//! cutovers physically remove only named retired state after locked safety
//! preflights. The partition-plane cutover requires drained leases and refuses
//! nonempty dead-letter history with an archive-or-environment-reprovision
//! diagnostic. PostgreSQL validates each canonical CHECK against existing rows
//! and fails loudly rather than fabricating incompatible history.
//!
//! **Ownership:** CREATE/ALTER/DROP need table ownership, and attempt-history
//! retirement must see every tenant through forced RLS. `wamn_app` and a plain
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

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

use wamn_control_provision::{
    DISPATCH_READER_ROLE, project_env_database_name, sql, validate_project_env,
};
use wamn_control_registry::{DurabilityClass, Triple};
use wamn_schema_control::{
    BareSchemaName, EffectWriterRoleObservation, RowPolicyObservation, RowSecurityObservation,
    RunPlaneAction, RunPlaneActionKind, RunPlaneObservation, RunPlanePlan,
    ScenarioAuthorRoleObservation, catalog_schema_present_sql, count_release_flow_rows_sql,
    count_retired_authored_ordering_rows_sql, count_run_rows_sql,
    count_stale_registration_keys_sql, plan_run_plane, select_app_scenario_author_membership_sql,
    select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_dispatch_reader_schema_privileges_sql,
    select_dispatch_reader_table_privileges_sql,
    select_effect_ledger_effective_column_privileges_sql,
    select_effect_ledger_effective_privileges_sql, select_effect_ledger_table_privileges_sql,
    select_effect_writer_role_sql, select_effect_writer_run_column_privileges_sql,
    select_effect_writer_run_table_privileges_sql, select_effect_writer_schema_privileges_sql,
    select_environment_policy_policies_sql, select_environment_policy_row_security_sql,
    select_node_runs_column_privileges_sql, select_node_runs_effective_column_privileges_sql,
    select_node_runs_effective_privileges_sql, select_node_runs_table_privileges_sql,
    select_outbox_function_present_sql, select_outbox_trigger_tables_sql,
    select_run_capture_privileges_sql, select_run_plane_helper_functions_sql,
    select_run_projection_schema_privileges_sql, select_run_projection_writer_role_sql,
    select_scenario_author_catalog_lock_privilege_sql, select_scenario_author_role_sql,
    select_scenario_author_schema_usage_sql, select_schema_checks_sql, select_schema_columns_sql,
    select_schema_foreign_keys_sql, select_schema_indexes_sql, select_schema_triggers_sql,
};

/// The action kinds permitted to execute BEFORE
/// [`crate::publish_catalog::ensure_wamn_app_role`] in [`reconcile`].
///
/// **This is a permission, not an ordering assertion.** Membership never claims
/// an action leads the plan; it says the action MAY lead it. Every member either
/// refuses outright — the two `Verify*` role boundaries are pure `DO` blocks
/// that `RAISE` or do nothing — or opens with an `ACCESS EXCLUSIVE` lock. Most
/// lock-taking members preflight before migration. `FailureDetailCutover` is
/// the deliberate exception: its one `ALTER TABLE ... DROP COLUMN ... RESTRICT`
/// is transactional, so a dependent-object refusal rolls back both column drops.
/// `ensure_wamn_app_role` is
/// itself a WRITE: it `CREATE ROLE`s `wamn_app`, hardens `wamn_scenario_author`,
/// and `REVOKE`s the membership between them. So the property this buys is
/// **refuse before you mutate** — the reconciler must not create or re-harden
/// cluster roles on a database it is about to refuse to touch. A refusal fails
/// the batch, `reconcile` returns `Err`, and the bootstrap below never runs.
///
/// **`ExecutionPinCutover` earns membership on ONE path.**
/// `wamn_schema_control::plan_run_plane` pushes that kind twice, and only the
/// first push can lead: on the populated-refusal early return — pin contract
/// incomplete AND `runs`/`catalog.release_flows` holding rows — it is pushed
/// into an empty plan and the planner RETURNS, so the action is the whole plan
/// and the loop in [`reconcile`] finds it at index 0. The second push, on the
/// empty-database path that actually performs the migration, is deliberately
/// mid-plan and can never lead. Reading only that second push makes this member
/// look dead; it is not (wamn-0h0g.20.13 was filed on exactly that misreading).
/// `EffectWriterCutover` has the identical two-push shape — a populated-ledger
/// early return plus a mid-plan push — and invites the identical misreading.
///
/// **The written order is NOT execution order.** The lookup is `contains`, which
/// is order-insensitive, so the order here is decoration. The planner emits both
/// `Verify*` kinds AFTER `PartitionPlaneCutover` and `ChildRunCutover` — the
/// reverse of how they are listed. `pre_role_bootstrap_allowlist_is_exact` pins
/// this array by equality and so freezes the order: it certifies the SET, and
/// must not be read as certifying a sequence.
///
/// **One wrinkle, unreachable in practice.** `execution_pin_cutover_sql` is the
/// only allowlisted builder whose SQL names `wamn_app` (`GRANT INSERT
/// (execution_bundle_hash), UPDATE (release_version, manifest_digest) ON
/// {target}.runs TO wamn_app`) — the one member carrying a dependency on the
/// bootstrap it is permitted to precede. It cannot fire: that builder leads only
/// via the populated early return, and its own preflight raises `55000` on
/// exactly the nonempty tables that put it there, so the `GRANT` is never
/// reached. Recorded, not coded around.
///
/// **The hazard to actually guard** lives in the planner, invisible from here:
/// 1. A NEW refusing cutover pushed into the plan without being added to this
///    array — the loop stops before it, `ensure_wamn_app_role` mutates roles,
///    and only then does the new action refuse.
/// 2. A non-allowlisted push interleaved AHEAD of allowlisted ones — the loop
///    consumes a PREFIX, so the first non-member truncates it and silently
///    strips the pre-bootstrap property from every allowlisted action behind it.
const PRE_ROLE_BOOTSTRAP_ACTIONS: [RunPlaneActionKind; 11] = [
    RunPlaneActionKind::VerifyEffectWriterRole,
    RunPlaneActionKind::VerifyRunProjectionWriterRole,
    RunPlaneActionKind::ExecutionPinCutover,
    RunPlaneActionKind::FrameIdentityCutover,
    RunPlaneActionKind::EffectWriterCutover,
    RunPlaneActionKind::PartitionPlaneCutover,
    RunPlaneActionKind::ChildRunCutover,
    RunPlaneActionKind::RerunLineageCutover,
    RunPlaneActionKind::FailureDetailCutover,
    RunPlaneActionKind::StoredSuiteCutover,
    RunPlaneActionKind::RetiredEffectDispositionCutover,
];

#[derive(Debug, Args)]
pub struct ReconcileRunPlaneArgs {
    /// Administrative Postgres URL to the system registry. The reconciler reads
    /// the project-env's stored instance suffix here before resolving policy;
    /// admission never connects to this database. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Administrative Postgres URL to the exact registry-derived project
    /// database. Observation and apply require SUPERUSER or BYPASSRLS so
    /// forced-RLS legacy rows cannot be skipped. Env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Registry organization owning the environment policy.
    #[arg(long)]
    pub org: String,

    /// Registry project owning the exact provisioned database target.
    #[arg(long)]
    pub project: String,

    /// Tenant whose project-local policy row is converged.
    #[arg(long)]
    pub tenant: String,

    /// Environment policy name in the owning organization's registry set.
    #[arg(long)]
    pub env: String,

    /// The project-env schema the run-plane tables live in (e.g.
    /// `wamn_runner_demo`, `poc_f1`).
    #[arg(long)]
    pub schema: String,

    /// Print the reconcile plan without applying it (strictly read-only).
    #[arg(long)]
    pub dry_run: bool,
}

/// Stable class for a run-plane target-identity refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileTargetErrorKind {
    /// The trusted triple could not resolve to one recorded registry target.
    RegistryTarget,
    /// The connected database did not prove the registry-derived target identity.
    DatabaseTarget,
}

/// Prefix shared by every typed run-plane target refusal.
pub const RECONCILE_TARGET_REFUSAL_PREFIX: &str = "reconcile-run-plane target refusal";

/// Contextual refusal raised before any run-plane or policy mutation.
#[derive(Debug)]
pub struct ReconcileTargetError {
    kind: ReconcileTargetErrorKind,
    context: String,
    expected_database: Option<String>,
    actual_database: Option<String>,
    source: Option<anyhow::Error>,
}

impl ReconcileTargetError {
    fn with_source(
        kind: ReconcileTargetErrorKind,
        context: impl Into<String>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            kind,
            context: format!("{RECONCILE_TARGET_REFUSAL_PREFIX}: {}", context.into()),
            expected_database: None,
            actual_database: None,
            source: Some(source.into()),
        }
    }

    fn mismatch(triple: &Triple, expected_database: String, actual_database: String) -> Self {
        Self {
            kind: ReconcileTargetErrorKind::DatabaseTarget,
            context: format!(
                "{RECONCILE_TARGET_REFUSAL_PREFIX}: database target mismatch for registry triple {triple}: expected {expected_database:?}, actual {actual_database:?}"
            ),
            expected_database: Some(expected_database),
            actual_database: Some(actual_database),
            source: None,
        }
    }

    /// Return the stable refusal class.
    pub const fn kind(&self) -> ReconcileTargetErrorKind {
        self.kind
    }

    /// Whether the trusted triple failed to resolve in the registry.
    pub const fn is_registry_target(&self) -> bool {
        matches!(self.kind, ReconcileTargetErrorKind::RegistryTarget)
    }

    /// Whether the connected database failed the exact target check.
    pub const fn is_database_target(&self) -> bool {
        matches!(self.kind, ReconcileTargetErrorKind::DatabaseTarget)
    }

    /// The exact registry-derived database name, when target comparison ran.
    pub fn expected_database(&self) -> Option<&str> {
        self.expected_database.as_deref()
    }

    /// The exact database-reported name, when target comparison ran.
    pub fn actual_database(&self) -> Option<&str> {
        self.actual_database.as_deref()
    }
}

impl std::fmt::Display for ReconcileTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.context)
    }
}

impl StdError for ReconcileTargetError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn StdError + 'static))
    }
}

pub async fn run(args: ReconcileRunPlaneArgs) -> anyhow::Result<()> {
    anyhow::ensure!(!args.tenant.is_empty(), "--tenant must not be empty");
    let schema = BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid --schema {:?}", args.schema))?;
    let triple = Triple::new(&args.org, &args.project, args.env.as_str());
    validate_project_env(&args.org, &args.project, &args.env).map_err(|source| {
        ReconcileTargetError::with_source(
            ReconcileTargetErrorKind::RegistryTarget,
            format!("registry target identity {triple} is invalid"),
            source,
        )
    })?;
    let instance =
        crate::provision_project_env::read_project_env_instance(&args.system_database_url, &triple)
            .await
            .map_err(|source| {
                ReconcileTargetError::with_source(
                    ReconcileTargetErrorKind::RegistryTarget,
                    format!("registry target {triple} has no usable recorded instance"),
                    source,
                )
            })?;
    let expected_database =
        project_env_database_name(&args.org, &args.project, &args.env, &instance);
    let (client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .map_err(|source| {
            ReconcileTargetError::with_source(
                ReconcileTargetErrorKind::DatabaseTarget,
                format!("database target for registry triple {triple} did not connect"),
                source,
            )
        })?;
    let conn_task = tokio::spawn(conn);
    let actual_database = client
        .query_one("SELECT pg_catalog.current_database()::text", &[])
        .await
        .map(|row| row.get::<_, String>(0))
        .map_err(|source| {
            ReconcileTargetError::with_source(
                ReconcileTargetErrorKind::DatabaseTarget,
                format!("database target for registry triple {triple} did not identify itself"),
                source,
            )
        });
    let actual_database = match actual_database {
        Ok(actual_database) => actual_database,
        Err(error) => {
            drop(client);
            let _ = conn_task.await;
            return Err(error.into());
        }
    };
    if actual_database != expected_database {
        drop(client);
        let _ = conn_task.await;
        return Err(
            ReconcileTargetError::mismatch(&triple, expected_database, actual_database).into(),
        );
    }
    let result = async {
        let durability_class = resolve_durability_class(
            &args.system_database_url,
            &args.org,
            &args.env,
            !args.dry_run,
        )
        .await?;
        let plan = reconcile(&client, &schema, !args.dry_run).await?;
        let policy_changed = converge_environment_policy(
            &client,
            &schema,
            &args.tenant,
            &args.env,
            durability_class,
            !args.dry_run,
        )
        .await?;
        Ok::<_, anyhow::Error>((plan, policy_changed, durability_class))
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    let (plan, policy_changed, durability_class) = result?;

    print_plan(&plan, args.dry_run);
    if policy_changed {
        let mode = if args.dry_run {
            "would converge"
        } else {
            "converged"
        };
        println!(
            "  {mode} environment policy tenant={:?} environment={:?} durability_class={}",
            args.tenant,
            args.env,
            durability_class.as_sql(),
        );
    }
    Ok(())
}

async fn resolve_durability_class(
    system_database_url: &str,
    org: &str,
    env: &str,
    apply: bool,
) -> anyhow::Result<DurabilityClass> {
    let (client, conn) = tokio_postgres::connect(system_database_url, NoTls)
        .await
        .context("system registry connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system")?;
        let durability_class = if apply {
            crate::env_policies::ensure_env_policy_durability_schema(&client).await?;
            crate::env_policies::read_env_policy(&client, org, env)
                .await?
                .map(|policy| policy.durability_class)
        } else {
            crate::env_policies::observe_env_policy_durability_class(&client, org, env).await?
        }
        .with_context(|| {
            format!(
                "env {env:?} names none of org {org:?}'s env policies; refusing to project an invented durability class"
            )
        })?;
        Ok::<_, anyhow::Error>(durability_class)
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

/// Converge the system policy's resolved class into the project-local relation.
/// The lookup and write are absent from admission and claim paths. `apply=false`
/// observes only, including the from-zero case where the relation is not yet
/// present, and returns whether a write would be required.
pub async fn converge_environment_policy(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
    tenant: &str,
    environment: &str,
    durability_class: DurabilityClass,
    apply: bool,
) -> anyhow::Result<bool> {
    anyhow::ensure!(!tenant.is_empty(), "tenant must not be empty");
    anyhow::ensure!(!environment.is_empty(), "environment must not be empty");
    let table_present: bool = client
        .query_one(
            "SELECT pg_catalog.to_regclass(pg_catalog.format('%I.environment_policies', $1::text)) IS NOT NULL",
            &[&schema.as_str()],
        )
        .await
        .context("observe project-local environment policy relation")?
        .get(0);
    let current = if table_present {
        client
            .query_opt(
                &format!(
                    "SELECT expected_environment, durability_class \
                       FROM {}.environment_policies WHERE tenant_id = $1",
                    schema.quoted()
                ),
                &[&tenant],
            )
            .await
            .context("observe project-local environment policy")?
            .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
    } else {
        None
    };
    let wanted = durability_class.as_sql();
    let changed = current
        .as_ref()
        .is_none_or(|(current_environment, current_class)| {
            current_environment != environment || current_class != wanted
        });
    if apply && changed {
        anyhow::ensure!(
            table_present,
            "run-plane reconciliation did not create the environment policy relation"
        );
        client
            .execute(
                &format!(
                    "INSERT INTO {}.environment_policies \
                       (tenant_id, expected_environment, durability_class) \
                     VALUES ($1, $2, $3) \
                     ON CONFLICT (tenant_id) DO UPDATE SET \
                       expected_environment = EXCLUDED.expected_environment, \
                       durability_class = EXCLUDED.durability_class \
                     WHERE environment_policies.expected_environment \
                               IS DISTINCT FROM EXCLUDED.expected_environment \
                        OR environment_policies.durability_class \
                               IS DISTINCT FROM EXCLUDED.durability_class",
                    schema.quoted()
                ),
                &[&tenant, &environment, &wanted],
            )
            .await
            .context("converge project-local environment policy")?;
    }
    Ok(changed)
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
    let mut plan = plan_run_plane(schema, &obs);
    if let Some(action) = dispatch_reader_read_surface_action(schema, &obs, !plan.is_noop()) {
        plan.actions.push(action);
    }
    if apply {
        let mut applied = 0;
        while plan
            .actions
            .get(applied)
            .is_some_and(|action| PRE_ROLE_BOOTSTRAP_ACTIONS.contains(&action.kind))
        {
            let action = &plan.actions[applied];
            client
                .batch_execute(&action.sql)
                .await
                .with_context(|| format!("apply {:?} {}", action.kind, action.target))?;
            applied += 1;
        }
        crate::publish_catalog::ensure_wamn_app_role(client).await?;
        for action in &plan.actions[applied..] {
            client
                .batch_execute(&action.sql)
                .await
                .with_context(|| format!("apply {:?} {}", action.kind, action.target))?;
        }
    }
    Ok(plan)
}

/// The dispatcher read principal's in-database surface, converged from the
/// EFFECT SHELL rather than from the pure planner (wamn-0h0g.12.123).
///
/// **Why here.** The grant text is
/// [`sql::grant_dispatch_reader_read_surface_sql`], which lives in
/// `wamn-control-provision`; `wamn-schema-control` does not depend on that crate
/// and should not. Re-encoding the surface inside the pure planner would make it
/// the SECOND encoding of one grant shape, which is the failure that
/// wamn-0h0g.12.37/.12.40 found SIX copies of. `RunPlanePlan::is_noop` is
/// `actions.is_empty()`, so appending here keeps the plan the shell returns,
/// prints, and gates on exactly truthful.
///
/// **Why `plan_already_acts` widens the trigger.** The observation is taken
/// BEFORE any action runs, and several actions create the schema or drop and
/// recreate `effect_attempts` — after which the reader's observed acl entries no
/// longer describe the database the repair will land in. Enumerating "the
/// actions that recreate my relations" would be one more encoding that rots, so
/// the rule is the sound one: if the plan is doing anything at all, re-apply the
/// (idempotent, narrowing) surface behind it. On a CONVERGED database the plan
/// is empty and the surface matches, so nothing is planned — the repair never
/// repeats, which is the whole acceptance.
///
/// **An absent role is not drift.** `provision-project-env` mints
/// `wamn_dispatch_reader` with a password this verb does not hold, so the
/// reconciler owns the role's in-database surface and never the role itself.
fn dispatch_reader_read_surface_action(
    schema: &BareSchemaName,
    obs: &RunPlaneObservation,
    plan_already_acts: bool,
) -> Option<RunPlaneAction> {
    if !obs.dispatch_reader_role_present {
        return None;
    }
    let converged = !plan_already_acts
        && obs.dispatch_reader_schema_privileges == BTreeSet::from(["USAGE".to_string()])
        && obs.dispatch_reader_table_privileges
            == sql::DISPATCH_READER_RELATIONS
                .into_iter()
                .map(|relation| (relation.to_string(), BTreeSet::from(["SELECT".to_string()])))
                .collect::<BTreeMap<_, _>>();
    if converged {
        return None;
    }
    Some(RunPlaneAction {
        kind: RunPlaneActionKind::RepairDispatchReaderPrivilege,
        target: format!("{}.dispatch-reader-read-surface", schema.as_str()),
        sql: sql::grant_dispatch_reader_read_surface_sql(schema.as_str()),
    })
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
        effect_writer_role: client
            .query_opt(&select_effect_writer_role_sql(), &[])
            .await
            .context("read effect-writer role boundary")?
            .map(|row| EffectWriterRoleObservation {
                can_login: row.get(0),
                is_superuser: row.get(1),
                can_create_database: row.get(2),
                can_create_role: row.get(3),
                inherits_roles: row.get(4),
                can_replicate: row.get(5),
                bypasses_rls: row.get(6),
                can_connect: row.get(7),
                owns_objects: row.get(8),
                membership_out_of_bounds: row.get(9),
            }),
        run_projection_writer_role: client
            .query_opt(&select_run_projection_writer_role_sql(), &[])
            .await
            .context("read run-projection-writer role boundary")?
            .map(|row| EffectWriterRoleObservation {
                can_login: row.get(0),
                is_superuser: row.get(1),
                can_create_database: row.get(2),
                can_create_role: row.get(3),
                inherits_roles: row.get(4),
                can_replicate: row.get(5),
                bypasses_rls: row.get(6),
                can_connect: row.get(7),
                owns_objects: row.get(8),
                membership_out_of_bounds: row.get(9),
            }),
        ..Default::default()
    };
    let writer_schema = client
        .query_one(
            select_effect_writer_schema_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effect-writer schema privileges")?;
    obs.effect_writer_schema_privileges = (writer_schema.get(0), writer_schema.get(1));
    let projection_schema = client
        .query_one(
            select_run_projection_schema_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read run-projection-writer schema privileges")?;
    obs.run_projection_schema_privileges = (projection_schema.get(0), projection_schema.get(1));
    for row in client
        .query(select_node_runs_table_privileges_sql(), &[&schema.as_str()])
        .await
        .context("read direct node-runs privileges")?
    {
        obs.node_runs_table_privileges
            .entry(row.get(0))
            .or_default()
            .insert(row.get(1));
    }
    for row in client
        .query(
            select_node_runs_column_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read direct node-runs column privileges")?
    {
        obs.node_runs_column_privileges
            .entry(row.get(0))
            .or_default()
            .insert(row.get(1));
    }
    for row in client
        .query(
            select_node_runs_effective_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effective node-runs privileges")?
    {
        obs.node_runs_effective_privileges
            .entry(row.get(0))
            .or_default()
            .insert(row.get(1));
        obs.node_runs_owner = Some(row.get(2));
        if row.get(3) {
            obs.node_runs_app_members.insert(row.get(0));
        }
    }
    for row in client
        .query(
            select_node_runs_effective_column_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effective node-runs column privileges")?
    {
        obs.node_runs_effective_column_privileges
            .entry(row.get(0))
            .or_default()
            .insert(row.get(1));
    }
    for row in client
        .query(
            select_effect_ledger_table_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read direct effect-ledger privileges")?
    {
        obs.effect_ledger_table_privileges
            .entry((row.get(0), row.get(1)))
            .or_default()
            .insert(row.get(2));
    }
    for row in client
        .query(
            select_effect_ledger_effective_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effective effect-ledger privileges")?
    {
        let table: String = row.get(0);
        obs.effect_ledger_effective_privileges
            .entry((table.clone(), row.get(1)))
            .or_default()
            .insert(row.get(2));
        obs.effect_ledger_owners.insert(table, row.get(3));
    }
    for row in client
        .query(
            select_effect_ledger_effective_column_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effective effect-ledger column privileges")?
    {
        obs.effect_ledger_effective_column_privileges
            .entry((row.get(0), row.get(1)))
            .or_default()
            .insert(row.get(2));
    }
    for row in client
        .query(
            select_effect_writer_run_table_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effect-writer run table privileges")?
    {
        obs.effect_writer_run_table_privileges
            .entry(row.get(0))
            .or_default()
            .insert(row.get(1));
    }
    for row in client
        .query(
            select_effect_writer_run_column_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effect-writer run column privileges")?
    {
        obs.effect_writer_run_column_privileges
            .entry((row.get(0), row.get(1)))
            .or_default()
            .insert(row.get(2));
    }

    obs.app_is_scenario_author_member = client
        .query_one(select_app_scenario_author_membership_sql(), &[])
        .await
        .context("read guest scenario-author membership")?
        .get(0);
    let capture_privileges_sql = select_run_capture_privileges_sql();
    let capture_privileges = client
        .query_one(&capture_privileges_sql, &[&schema.as_str()])
        .await
        .context("read guest run-capture privileges")?;
    obs.app_run_capture_privileges = (
        capture_privileges.get(0),
        capture_privileges.get(1),
        capture_privileges.get(2),
    );
    obs.scenario_author_can_lock_catalog_head = client
        .query_one(
            select_scenario_author_catalog_lock_privilege_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read scenario-author catalog-lock privilege")?
        .get(0);
    let dispatch_reader_schema = client
        .query_one(
            select_dispatch_reader_schema_privileges_sql(),
            &[&schema.as_str(), &DISPATCH_READER_ROLE],
        )
        .await
        .context("read dispatch-reader schema privileges")?;
    obs.dispatch_reader_role_present = dispatch_reader_schema.get(0);
    obs.dispatch_reader_schema_privileges = dispatch_reader_schema
        .get::<_, Vec<String>>(1)
        .into_iter()
        .collect();
    for row in client
        .query(
            select_dispatch_reader_table_privileges_sql(),
            &[&schema.as_str(), &DISPATCH_READER_ROLE],
        )
        .await
        .context("read dispatch-reader table privileges")?
    {
        obs.dispatch_reader_table_privileges
            .entry(row.get(0))
            .or_default()
            .insert(row.get(1));
    }
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
        if row.get(2) {
            obs.non_nullable_columns
                .insert((table.clone(), column.clone()));
        }
        if row.get(3) {
            obs.defaulted_columns
                .insert((table.clone(), column.clone()));
        }
        obs.column_types
            .insert((table.clone(), column.clone()), row.get(4));
        obs.tables.entry(table).or_default().insert(column);
    }
    if let Some(row) = client
        .query_opt(
            select_environment_policy_row_security_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read environment-policy row-security flags")?
    {
        let mut row_security = RowSecurityObservation {
            enabled: row.get(0),
            forced: row.get(1),
            ..Default::default()
        };
        for row in client
            .query(
                select_environment_policy_policies_sql(),
                &[&schema.as_str()],
            )
            .await
            .context("read environment-policy row-security policies")?
        {
            row_security.policies.insert(
                row.get(0),
                RowPolicyObservation {
                    command: row.get(1),
                    permissive: row.get(2),
                    roles: row.get::<_, Vec<String>>(3).into_iter().collect(),
                    using_expression: row.get(4),
                    check_expression: row.get(5),
                },
            );
        }
        obs.environment_policy_row_security = Some(row_security);
    }
    let effect_ledgers: Vec<&str> = [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ]
    .into_iter()
    .filter(|table| obs.tables.contains_key(*table))
    .collect();
    if !effect_ledgers.is_empty() {
        let count_sql = effect_ledgers
            .iter()
            .map(|table| format!("SELECT count(*) FROM {}.{}", schema.quoted(), table))
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        obs.effect_ledger_rows = client
            .query_one(
                &format!("SELECT COALESCE(sum(n), 0)::bigint FROM ({count_sql}) AS counts(n)"),
                &[],
            )
            .await
            .context("count effect ledger rows for writer cutover")?
            .get(0);
    }
    if obs
        .tables
        .get("flows")
        .is_some_and(|columns| columns.contains("graph_json"))
    {
        obs.retired_authored_ordering_rows = client
            .query_one(&count_retired_authored_ordering_rows_sql(schema), &[])
            .await
            .context("count persisted retired flow-ordering keys")?
            .get(0);
    }
    if obs.tables.contains_key("runs") {
        obs.run_rows = client
            .query_one(&count_run_rows_sql(schema), &[])
            .await
            .context("count run rows for execution-pin cutover")?
            .get(0);
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
            if row.get(2) {
                obs.catalog_non_nullable_columns
                    .insert((table.clone(), column.clone()));
            }
            obs.catalog_column_types
                .insert((table.clone(), column.clone()), row.get(4));
            obs.catalog_tables.insert(table.clone());
            obs.catalog_columns.entry(table).or_default().insert(column);
        }
        for row in client
            .query(select_schema_indexes_sql(), &[&"catalog"])
            .await
            .context("read catalog indexes")?
        {
            obs.catalog_indexes.insert(row.get(0), row.get(1));
        }
        for row in client
            .query(select_schema_checks_sql(), &[&"catalog"])
            .await
            .context("read catalog check constraints")?
        {
            obs.catalog_checks
                .insert((row.get(0), row.get(1)), row.get(2));
        }
        for row in client
            .query(select_schema_foreign_keys_sql(), &[&"catalog"])
            .await
            .context("read catalog foreign keys")?
        {
            obs.catalog_foreign_keys
                .insert((row.get(0), row.get(1)), row.get(2));
        }
        for row in client
            .query(select_schema_triggers_sql(), &[&"catalog"])
            .await
            .context("read catalog triggers")?
        {
            obs.catalog_triggers
                .insert((row.get(0), row.get(1)), row.get(2));
        }
        if obs.catalog_tables.contains("release_flows") {
            obs.release_flow_rows = client
                .query_one(count_release_flow_rows_sql(), &[])
                .await
                .context("count release-flow rows for execution-pin cutover")?
                .get(0);
        }
        if obs.catalog_tables.contains("event_registrations") {
            obs.stale_registration_key_rows = client
                .query_one(count_stale_registration_keys_sql(), &[])
                .await
                .context("count retired registration keys")?
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

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> BareSchemaName {
        BareSchemaName::new("demo").expect("test schema is valid")
    }

    /// The exact surface `sql::grant_dispatch_reader_read_surface_sql` produces.
    fn converged_observation() -> RunPlaneObservation {
        RunPlaneObservation {
            dispatch_reader_role_present: true,
            dispatch_reader_schema_privileges: BTreeSet::from(["USAGE".to_string()]),
            dispatch_reader_table_privileges: BTreeMap::from([
                (
                    "run_queue".to_string(),
                    BTreeSet::from(["SELECT".to_string()]),
                ),
                (
                    "effect_attempts".to_string(),
                    BTreeSet::from(["SELECT".to_string()]),
                ),
            ]),
            ..Default::default()
        }
    }

    /// wamn-0h0g.12.40's failure mode, guarded purely: a converged database must
    /// plan NOTHING. An observation the grant can never satisfy makes drift
    /// permanently true and the reconciler never converges.
    #[test]
    fn a_converged_dispatch_reader_plans_no_repair() {
        assert_eq!(
            dispatch_reader_read_surface_action(&schema(), &converged_observation(), false),
            None
        );
    }

    /// `provision-project-env` owns the role; an environment provisioned before
    /// wamn-0h0g.12.122 simply has no reader, and that is not drift.
    #[test]
    fn an_absent_dispatch_reader_role_plans_no_repair() {
        let mut obs = converged_observation();
        obs.dispatch_reader_role_present = false;
        obs.dispatch_reader_schema_privileges.clear();
        obs.dispatch_reader_table_privileges.clear();
        assert_eq!(
            dispatch_reader_read_surface_action(&schema(), &obs, false),
            None
        );
        // …and an absent role stays silent even when the plan is already acting,
        // because the repair would fail on a role that does not exist.
        assert_eq!(
            dispatch_reader_read_surface_action(&schema(), &obs, true),
            None
        );
    }

    #[test]
    fn a_widened_or_missing_dispatch_reader_surface_plans_the_repair() {
        // Never granted at all.
        let mut fresh = converged_observation();
        fresh.dispatch_reader_schema_privileges.clear();
        fresh.dispatch_reader_table_privileges.clear();
        assert!(dispatch_reader_read_surface_action(&schema(), &fresh, false).is_some());

        // Widened by one privilege on a relation it may read.
        let mut widened = converged_observation();
        widened
            .dispatch_reader_table_privileges
            .get_mut("run_queue")
            .expect("run_queue is in the expected surface")
            .insert("UPDATE".to_string());
        assert!(dispatch_reader_read_surface_action(&schema(), &widened, false).is_some());

        // Widened onto a relation it may not read at all.
        let mut extra_relation = converged_observation();
        extra_relation
            .dispatch_reader_table_privileges
            .insert("runs".to_string(), BTreeSet::from(["SELECT".to_string()]));
        assert!(dispatch_reader_read_surface_action(&schema(), &extra_relation, false).is_some());

        // Widened at the schema level.
        let mut creator = converged_observation();
        creator
            .dispatch_reader_schema_privileges
            .insert("CREATE".to_string());
        assert!(dispatch_reader_read_surface_action(&schema(), &creator, false).is_some());
    }

    /// The observation predates every action, and the ledger cutover drops and
    /// recreates `effect_attempts`. A converged reader observed BEFORE that
    /// still needs its grants re-applied behind it.
    #[test]
    fn an_acting_plan_carries_the_reader_repair_along() {
        assert!(
            dispatch_reader_read_surface_action(&schema(), &converged_observation(), true)
                .is_some()
        );
    }

    /// The pinned repair. A runtime gate is insensitive to a builder swapped for
    /// a wider one whose end state happens to include the narrow grants; the
    /// frozen string is what catches it. The leading REVOKEs are what make the
    /// action NARROW as well as grant.
    #[test]
    fn the_planned_repair_sql_is_exact() {
        let action = dispatch_reader_read_surface_action(
            &schema(),
            &RunPlaneObservation {
                dispatch_reader_role_present: true,
                ..Default::default()
            },
            false,
        )
        .expect("an ungranted reader plans the repair");
        assert_eq!(
            action.kind,
            RunPlaneActionKind::RepairDispatchReaderPrivilege
        );
        assert_eq!(action.target, "demo.dispatch-reader-read-surface");
        assert_eq!(
            action.sql,
            "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA \"demo\" \
             FROM \"wamn_dispatch_reader\"; \
             REVOKE ALL PRIVILEGES ON SCHEMA \"demo\" FROM \"wamn_dispatch_reader\"; \
             GRANT USAGE ON SCHEMA \"demo\" TO \"wamn_dispatch_reader\"; \
             GRANT SELECT ON \"demo\".\"run_queue\" TO \"wamn_dispatch_reader\"; \
             GRANT SELECT ON \"demo\".\"effect_attempts\" TO \"wamn_dispatch_reader\";"
        );
    }

    /// The one POSITIVE reachability assertion behind
    /// [`PRE_ROLE_BOOTSTRAP_ACTIONS`]: on a populated schema whose execution-pin
    /// contract is incomplete, the plan [`reconcile`] walks is EXACTLY the
    /// refusing cutover, and that kind is allowlisted — so the whole plan runs
    /// ahead of `ensure_wamn_app_role`, which never touches a database about to
    /// refuse.
    ///
    /// Two independent facts hold that up, and this is the only place in this
    /// crate pinning either: the kind's membership above, and
    /// `wamn_schema_control::plan_run_plane`'s early return, which is what makes
    /// the cutover the WHOLE plan rather than the head of an 89-action one.
    /// Deleting either turns this red.
    #[test]
    fn a_populated_execution_pin_refusal_leads_the_pre_bootstrap_prefix() {
        let obs = RunPlaneObservation {
            tables: BTreeMap::from([("runs".to_string(), BTreeSet::new())]),
            run_rows: 1,
            ..Default::default()
        };

        let plan = plan_run_plane(&schema(), &obs);
        assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
        assert_eq!(
            plan.actions[0].kind,
            RunPlaneActionKind::ExecutionPinCutover
        );
        assert!(PRE_ROLE_BOOTSTRAP_ACTIONS.contains(&plan.actions[0].kind));
        // …and the shell appends nothing behind it, so the plan `reconcile`
        // walks IS this plan: all of it runs before the role bootstrap.
        assert_eq!(
            dispatch_reader_read_surface_action(&schema(), &obs, true),
            None
        );
    }

    /// The repair is NOT a pre-bootstrap action: it must run after the creates,
    /// never before.
    #[test]
    fn the_reader_repair_is_not_a_pre_role_bootstrap_action() {
        assert!(
            !PRE_ROLE_BOOTSTRAP_ACTIONS
                .contains(&RunPlaneActionKind::RepairDispatchReaderPrivilege)
        );
    }

    #[test]
    fn pre_role_bootstrap_allowlist_is_exact() {
        assert_eq!(
            PRE_ROLE_BOOTSTRAP_ACTIONS,
            [
                RunPlaneActionKind::VerifyEffectWriterRole,
                RunPlaneActionKind::VerifyRunProjectionWriterRole,
                RunPlaneActionKind::ExecutionPinCutover,
                RunPlaneActionKind::FrameIdentityCutover,
                RunPlaneActionKind::EffectWriterCutover,
                RunPlaneActionKind::PartitionPlaneCutover,
                RunPlaneActionKind::ChildRunCutover,
                RunPlaneActionKind::RerunLineageCutover,
                RunPlaneActionKind::FailureDetailCutover,
                RunPlaneActionKind::StoredSuiteCutover,
                RunPlaneActionKind::RetiredEffectDispositionCutover,
            ]
        );
        assert!(!PRE_ROLE_BOOTSTRAP_ACTIONS.contains(&RunPlaneActionKind::EnsureSchema));
    }
}
