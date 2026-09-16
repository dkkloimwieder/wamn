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
//! with `SUPERUSER` or explicit `BYPASSRLS`, like project schema installation
//! / `reconcile-replica-identity`.
//!
//! **Scope:** strictly the `--schema` project-env schema plus the per-database
//! `catalog` metadata schema; entity/floor tables in the schema are read for
//! the legacy-trigger survey only and never altered (application schema belongs
//! to package apply; retained content restore belongs to the ops restore verb).
//!
//! `--dry-run` is STRICTLY read-only: it neither ensures the `wamn_app` role
//! nor executes any plan action.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;

use anyhow::Context as _;
use tokio_postgres::NoTls;

use wamn_control_provision::{
    APP_SCHEMA_SQL, DISPATCH_READER_ROLE, PlatformComponent, bind_platform_principal_sql,
    platform_principals_sql, project_env_database_name, sql, validate_project_env,
};
use wamn_control_registry::{DurabilityClass, Triple};
use wamn_schema_control::{
    BareSchemaName, RowPolicyObservation, RowSecurityObservation, RunPlaneAction,
    RunPlaneActionKind, RunPlaneObservation, RunPlanePlan, ScenarioAuthorRoleObservation,
    catalog_schema_present_sql, count_retired_authored_ordering_rows_sql,
    count_stale_registration_keys_sql, plan_run_plane, select_app_run_queue_authority_sql,
    select_app_scenario_author_membership_sql, select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_dispatch_reader_schema_privileges_sql,
    select_dispatch_reader_table_privileges_sql,
    select_effect_table_effective_column_privileges_sql,
    select_effect_table_effective_privileges_sql, select_effect_table_privileges_sql,
    select_environment_policy_policies_sql, select_environment_policy_row_security_sql,
    select_outbox_function_present_sql, select_outbox_trigger_tables_sql,
    select_run_capture_privileges_sql, select_run_plane_helper_functions_sql,
    select_scenario_author_role_sql, select_scenario_author_schema_usage_sql,
    select_schema_checks_sql, select_schema_columns_sql, select_schema_foreign_keys_sql,
    select_schema_indexes_sql, select_schema_triggers_sql,
};

/// The action kinds permitted to execute BEFORE
/// the role bootstrap in [`reconcile`].
///
/// **This is a permission, not an ordering assertion.** Membership never claims
/// an action leads the plan; it says the action MAY lead it. Every member opens
/// with an `ACCESS EXCLUSIVE` lock. Most lock-taking members preflight before
/// migration. `FailureDetailCutover` is
/// the deliberate exception: its one `ALTER TABLE ... DROP COLUMN ... RESTRICT`
/// is transactional, so a dependent-object refusal rolls back both column drops.
/// `ensure_wamn_app_role` is itself a WRITE: it creates or hardens `wamn_app`
/// as a passwordless NOLOGIN ACL role, hardens `wamn_scenario_author`, and
/// `REVOKE`s the membership between them. So the property this buys is
/// **refuse before you mutate** — the reconciler must not create or re-harden
/// cluster roles on a database it is about to refuse to touch. A refusal fails
/// the batch, `reconcile` returns `Err`, and the bootstrap below never runs.
///
/// `RetireNodeRuns` and `RetireExecutionBundles` each produce a one-action
/// plan. Their RESTRICT failure or successful discard therefore precedes role
/// bootstrap and every unrelated target-database repair.
///
/// **The written order is NOT execution order.** The lookup is `contains`, which
/// is order-insensitive, so the order here is decoration. The planner emits
/// `FrameIdentityCutover` AFTER `PartitionPlaneCutover` and
/// `ChildRunCutover` — the reverse of how it is listed.
/// `pre_role_bootstrap_allowlist_is_exact` pins
/// this array by equality and so freezes the order: it certifies the SET, and
/// must not be read as certifying a sequence.
///
/// **The hazard to actually guard** lives in the planner, invisible from here:
/// 1. A NEW refusing cutover pushed into the plan without being added to this
///    array — the loop stops before it, `ensure_wamn_app_role` mutates roles,
///    and only then does the new action refuse.
/// 2. A non-allowlisted push interleaved AHEAD of allowlisted ones — the loop
///    consumes a PREFIX, so the first non-member truncates it and silently
///    strips the pre-bootstrap property from every allowlisted action behind it.
const PRE_ROLE_BOOTSTRAP_ACTIONS: [RunPlaneActionKind; 11] = [
    RunPlaneActionKind::RetireNodeRuns,
    RunPlaneActionKind::RetireExecutionBundles,
    RunPlaneActionKind::FrameIdentityCutover,
    RunPlaneActionKind::RetireLegacyAdmissionSurface,
    RunPlaneActionKind::EffectTableCutover,
    RunPlaneActionKind::PartitionPlaneCutover,
    RunPlaneActionKind::ChildRunCutover,
    RunPlaneActionKind::RerunLineageCutover,
    RunPlaneActionKind::FailureDetailCutover,
    RunPlaneActionKind::StoredSuiteCutover,
    RunPlaneActionKind::RetiredEffectDispositionCutover,
];

/// Inputs of one run-plane reconciliation.
#[derive(Debug)]
pub struct ReconcileRunPlaneRequest {
    /// Administrative Postgres URL to the system registry. The reconciler reads
    /// the project-env's stored instance suffix here before resolving policy;
    /// admission never connects to this database.
    pub system_database_url: String,

    /// Administrative Postgres URL to the exact registry-derived project
    /// database. Observation and apply require SUPERUSER or BYPASSRLS so
    /// forced-RLS legacy rows cannot be skipped.
    pub admin_database_url: String,

    /// Registry organization owning the environment policy.
    pub org: String,

    /// Registry project owning the exact provisioned database target.
    pub project: String,

    /// Tenant whose project-local policy row is converged.
    pub tenant: String,

    /// Environment policy name in the owning organization's registry set.
    pub env: String,

    /// The project-env schema the run-plane tables live in (e.g.
    /// `wamn_runner_demo`, `poc_f1`).
    pub schema: String,

    /// Plan without applying (strictly read-only).
    pub dry_run: bool,
}

/// Result of one run-plane reconciliation.
#[derive(Debug)]
pub struct ReconcileRunPlaneOutcome {
    /// The plan that was applied, or that would apply under a dry run.
    pub plan: RunPlanePlan,
    /// Whether the tenant's environment policy changed, or would change.
    pub policy_changed: bool,
    /// Durability class of the source environment policy.
    pub durability_class: DurabilityClass,
    /// What the tenant's `app_system` schema and identity rows needed.
    pub tenant_identity: TenantIdentityOutcome,
}

/// Stable class for a run-plane target-identity refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileTargetErrorKind {
    /// The trusted triple could not resolve to one recorded registry target.
    RegistryTarget,
    /// The connected database did not match the registry-derived target identity.
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

/// Reconcile one registry-verified run plane and return its plan and policy change.
pub async fn reconcile_run_plane(
    args: ReconcileRunPlaneRequest,
) -> anyhow::Result<ReconcileRunPlaneOutcome> {
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
    let (mut client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
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
        let source_policy = crate::verification_policy::read_authoritative_environment_policy(
            &args.system_database_url,
            &args.org,
            &args.env,
            !args.dry_run,
        )
        .await?;
        let durability_class = source_policy.durability_class();
        let plan = reconcile(&client, &schema, !args.dry_run).await?;
        let policy_changed = converge_environment_policy(
            &client,
            &schema,
            &args.tenant,
            &source_policy,
            !args.dry_run,
        )
        .await?;
        // After the catalog schema, because app-schema.sql's stamp triggers
        // call the record-history function that composition installs.
        let identity_source = read_tenant_identity_source(
            &args.system_database_url,
            &args.org,
            &args.project,
            &args.env,
        )
        .await?;
        let tenant_identity =
            converge_tenant_identity(&mut client, &args.tenant, &identity_source, !args.dry_run)
                .await?;
        Ok::<_, anyhow::Error>((plan, policy_changed, durability_class, tenant_identity))
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    let (plan, policy_changed, durability_class, tenant_identity) = result?;
    Ok(ReconcileRunPlaneOutcome {
        plan,
        policy_changed,
        durability_class,
        tenant_identity,
    })
}

/// One service principal that holds a role in the project being reconciled.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServicePrincipal {
    id: String,
    subject: String,
    display_name: String,
}

/// One human principal with a membership in the project environment being
/// reconciled (`wamn-0h0g.9.18`).
///
/// A person carries a real address, so this holds `identity.principals.email`
/// and never the subject, which only authenticates.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PersonPrincipal {
    id: String,
    email: String,
    display_name: String,
}

/// The deployment and registry facts a tenant's identity rows are built from.
///
/// All of them come from the system registry, never from the tenant database,
/// so a tenant cannot name its own platform rows, invent a service principal,
/// or admit a person who was never granted a membership.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TenantIdentitySource {
    /// `registry.meta.platform_domain`, absent until the bootstrap sets it.
    platform_domain: Option<String>,
    services: Vec<ServicePrincipal>,
    people: Vec<PersonPrincipal>,
}

/// Every service principal holding a role in one project, with the deployment
/// platform domain that names the platform rows.
const TENANT_IDENTITY_SOURCE_SQL: &str = "SELECT p.id::text, p.subject, p.display_name \
     FROM identity.project_roles AS r \
     JOIN identity.principals AS p ON p.id = r.principal_id \
     WHERE r.org = $1 AND r.project = $2 \
       AND p.kind = 'service' AND p.status = 'active' \
     GROUP BY p.id, p.subject, p.display_name \
     ORDER BY p.subject";

/// Every human principal that `grant-project-env-membership` admitted to ONE
/// project environment (`wamn-0h0g.9.18`).
///
/// `identity.project_env_memberships` carries `org`, `project` and `env`
/// itself, so the tenant needs no join beyond the principal that names the
/// person. The env narrows the read: one tenant database serves one project
/// environment, and a member of `dev` gets no row in the `prod` tenant.
const TENANT_IDENTITY_PEOPLE_SQL: &str = "SELECT p.id::text, p.email, p.display_name \
     FROM identity.project_env_memberships AS m \
     JOIN identity.principals AS p ON p.id = m.principal_id \
     WHERE m.org = $1 AND m.project = $2 AND m.env = $3 \
       AND p.kind = 'human' AND p.status = 'active' \
     ORDER BY p.email";

async fn read_tenant_identity_source(
    system_database_url: &str,
    org: &str,
    project: &str,
    env: &str,
) -> anyhow::Result<TenantIdentitySource> {
    let (client, connection) = tokio_postgres::connect(system_database_url, NoTls)
        .await
        .context("connect to the system registry for tenant identity")?;
    let connection_task = tokio::spawn(connection);
    let read = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("assume the system registry owner")?;
        let platform_domain: Option<String> = client
            .query_one("SELECT platform_domain FROM registry.meta", &[])
            .await
            .context("read the deployment platform domain")?
            .get(0);
        let services = client
            .query(TENANT_IDENTITY_SOURCE_SQL, &[&org, &project])
            .await
            .context("read the project's service principals")?
            .into_iter()
            .map(|row| ServicePrincipal {
                id: row.get(0),
                subject: row.get(1),
                display_name: row.get(2),
            })
            .collect();
        let people = client
            .query(TENANT_IDENTITY_PEOPLE_SQL, &[&org, &project, &env])
            .await
            .context("read the project environment's human members")?
            .into_iter()
            .map(|row| PersonPrincipal {
                id: row.get(0),
                email: row.get(1),
                display_name: row.get(2),
            })
            .collect();
        Ok::<_, anyhow::Error>(TenantIdentitySource {
            platform_domain,
            services,
            people,
        })
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    read
}

/// What one tenant-identity convergence did, or would do under a dry run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TenantIdentityOutcome {
    /// `deploy/sql/app-schema.sql` was absent and was installed.
    pub app_schema_installed: bool,
    /// Platform rows written, out of the closed component list.
    pub platform_rows_written: usize,
    /// Service rows written for principals holding a role in this project.
    pub service_rows_written: usize,
    /// Person rows written for humans with a membership in this environment.
    pub person_rows_written: usize,
}

/// Install the tenant's `app_system` schema and its identity rows
/// (`wamn-0h0g.9.15`).
///
/// This is the production application of `deploy/sql/app-schema.sql` and of the
/// platform principal rows. Both were manual `psql` steps that
/// `docs/operations/deployment.md` told an operator to run, and a tenant that
/// skipped them refused every stamped write with `actor-required`.
///
/// It runs here and not in `provision-project-env` because this verb already
/// holds an administrative connection to the exact project-env database, and
/// because the record-history functions the stamp triggers call arrive with the
/// catalog schema this same run installs.
///
/// A service row is written for every service principal holding a role in the
/// project. Its email is `<subject>@<platform-domain>`: a service is not a
/// person, so it carries the deployment's own domain, like a platform row, and
/// the subject keeps it unique inside the tenant.
///
/// A person row is written for every human that `grant-project-env-membership`
/// admitted to this environment (`wamn-0h0g.9.18`). It carries the human's own
/// `identity.principals.email` and display name. A new member gets the row at
/// the next reconcile, not at the moment of the grant, because the grant flow
/// holds no tenant connection.
///
/// `apply=false` observes only and writes nothing.
async fn converge_tenant_identity(
    client: &mut tokio_postgres::Client,
    tenant_id: &str,
    source: &TenantIdentitySource,
    apply: bool,
) -> anyhow::Result<TenantIdentityOutcome> {
    anyhow::ensure!(!tenant_id.is_empty(), "tenant must not be empty");
    let mut outcome = TenantIdentityOutcome::default();
    let app_schema_present: bool = client
        .query_one(
            "SELECT EXISTS ( SELECT FROM pg_namespace WHERE nspname = 'app_system' )",
            &[],
        )
        .await
        .context("probe the app_system schema")?
        .get(0);

    // Every relation app-schema.sql creates has a stamp trigger, so the
    // record-history function has to exist before the file runs. The catalog
    // composition carries it, and this same reconcile installs that. A tenant
    // whose catalog predates record-history reaches here without it, and the
    // failure is clearer here than inside a CREATE TRIGGER.
    let stamp_function_present: bool = client
        .query_one(
            "SELECT EXISTS ( SELECT FROM pg_proc AS p \
               JOIN pg_namespace AS n ON n.oid = p.pronamespace \
              WHERE n.nspname = 'wamn_history' AND p.proname = 'stamp_row' )",
            &[],
        )
        .await
        .context("probe the record-history stamp function")?
        .get(0);

    let missing_platform: Vec<PlatformComponent> = if app_schema_present {
        let present: BTreeSet<String> = client
            .query(
                "SELECT id::text FROM app_system.users \
                  WHERE tenant_id = $1 AND type = 'platform'",
                &[&tenant_id],
            )
            .await
            .context("read the tenant's platform rows")?
            .into_iter()
            .map(|row| row.get::<_, String>(0))
            .collect();
        PlatformComponent::ALL
            .into_iter()
            .filter(|component| !present.contains(&component.principal_id().to_string()))
            .collect()
    } else {
        PlatformComponent::ALL.into_iter().collect()
    };

    let present_users: BTreeSet<String> = if app_schema_present {
        client
            .query(
                "SELECT id::text FROM app_system.users WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await
            .context("read the tenant's users rows")?
            .into_iter()
            .map(|row| row.get::<_, String>(0))
            .collect()
    } else {
        BTreeSet::new()
    };
    let missing_services: Vec<&ServicePrincipal> = source
        .services
        .iter()
        .filter(|service| !present_users.contains(&service.id))
        .collect();
    let missing_people: Vec<&PersonPrincipal> = source
        .people
        .iter()
        .filter(|person| !present_users.contains(&person.id))
        .collect();

    outcome.app_schema_installed = !app_schema_present;
    outcome.platform_rows_written = missing_platform.len();
    outcome.service_rows_written = missing_services.len();
    outcome.person_rows_written = missing_people.len();
    if !apply
        || (app_schema_present
            && missing_platform.is_empty()
            && missing_services.is_empty()
            && missing_people.is_empty())
    {
        return Ok(outcome);
    }
    anyhow::ensure!(
        stamp_function_present,
        "record-history-absent: this database has no wamn_history.stamp_row function, which every \
         app_system stamp trigger calls. deploy/sql/record-history.sql arrives with the catalog \
         schema, so reconcile the catalog schema before the tenant identity rows"
    );
    let Some(platform_domain) = source.platform_domain.as_deref() else {
        anyhow::bail!(
            "platform-domain-unset: registry.meta.platform_domain names the domain of the \
             platform principal emails, and this deployment has not set it. Set it once against \
             the system database, for example UPDATE registry.meta SET platform_domain = \
             'example.invalid'"
        );
    };
    let platform_rows = platform_principals_sql(tenant_id, platform_domain)
        .map_err(|source| anyhow::anyhow!("{source}"))?;

    let transaction = client
        .transaction()
        .await
        .context("open the tenant identity transaction")?;
    if !app_schema_present {
        transaction
            .batch_execute(APP_SCHEMA_SQL)
            .await
            .context("install deploy/sql/app-schema.sql")?;
    }
    // Every write below stamps `wamn:provisioning`, and its own row stamps
    // itself, because the bind comes first and lasts for this transaction.
    transaction
        .batch_execute(&platform_rows)
        .await
        .context("write the tenant's platform principal rows")?;
    if !missing_services.is_empty() || !missing_people.is_empty() {
        transaction
            .batch_execute(&bind_platform_principal_sql(
                PlatformComponent::Provisioning,
            ))
            .await
            .context("bind wamn:provisioning for the service and person rows")?;
        for service in &missing_services {
            transaction
                .execute(
                    "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
                     VALUES ($1, $2::text::uuid, 'service', $3, $4)",
                    &[
                        &tenant_id,
                        &service.id,
                        &format!("{}@{platform_domain}", service.subject),
                        &service.display_name,
                    ],
                )
                .await
                .with_context(|| {
                    format!("write the service row of principal {:?}", service.subject)
                })?;
        }
        for person in &missing_people {
            transaction
                .execute(
                    "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
                     VALUES ($1, $2::text::uuid, 'person', $3, $4)",
                    &[&tenant_id, &person.id, &person.email, &person.display_name],
                )
                .await
                .with_context(|| format!("write the person row of principal {}", person.id))?;
        }
    }
    transaction
        .commit()
        .await
        .context("commit the tenant identity rows")?;
    Ok(outcome)
}

/// Converge one source-attested environment policy into the project-local
/// relation owned by this reconciler. `apply=false` observes only, including a
/// from-zero schema where the relation or additive source carriers do not yet
/// exist.
pub(crate) async fn converge_environment_policy(
    client: &tokio_postgres::Client,
    schema: &BareSchemaName,
    tenant_id: &str,
    source: &crate::verification_policy::AuthoritativeEnvironmentPolicy,
    apply: bool,
) -> anyhow::Result<bool> {
    anyhow::ensure!(!tenant_id.is_empty(), "tenant must not be empty");
    let table_present: bool = client
        .query_one(
            "SELECT pg_catalog.to_regclass(pg_catalog.format('%I.environment_policies', $1::text)) IS NOT NULL",
            &[&schema.as_str()],
        )
        .await
        .context("observe project-local environment policy relation")?
        .get(0);
    let source_carriers_present: bool = if table_present {
        client
            .query_one(
                "SELECT count(*) = 2 FROM pg_catalog.pg_attribute AS attribute \
                  WHERE attribute.attrelid = \
                          pg_catalog.to_regclass(pg_catalog.format('%I.environment_policies', $1::text)) \
                    AND attribute.attname IN ('source_policy_org', 'source_policy_hash') \
                    AND attribute.attnum > 0 AND NOT attribute.attisdropped",
                &[&schema.as_str()],
            )
            .await
            .context("observe project-local environment policy source carriers")?
            .get(0)
    } else {
        false
    };
    let current = if source_carriers_present {
        client
            .query_opt(
                &format!(
                    "SELECT expected_environment, durability_class, \
                            source_policy_org, source_policy_hash \
                       FROM {}.environment_policies WHERE tenant_id = $1",
                    schema.quoted()
                ),
                &[&tenant_id],
            )
            .await
            .context("observe project-local environment policy")?
            .map(|row| {
                (
                    row.get::<_, String>(0),
                    row.get::<_, String>(1),
                    row.get::<_, Option<String>>(2),
                    row.get::<_, Option<String>>(3),
                )
            })
    } else {
        None
    };
    let wanted_class = source.durability_class().as_sql();
    let changed = current.as_ref().is_none_or(
        |(current_environment, current_class, current_org, current_hash)| {
            current_environment != source.environment()
                || current_class != wanted_class
                || current_org.as_deref() != Some(source.source_policy_org.as_ref())
                || current_hash.as_deref() != Some(source.source_policy_hash.as_ref())
        },
    );
    if apply && changed {
        anyhow::ensure!(
            table_present && source_carriers_present,
            "run-plane reconciliation did not create the environment policy source carriers"
        );
        client
            .execute(
                &format!(
                    "INSERT INTO {}.environment_policies \
                       (tenant_id, expected_environment, durability_class, \
                        source_policy_org, source_policy_hash) \
                     VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT (tenant_id) DO UPDATE SET \
                       expected_environment = EXCLUDED.expected_environment, \
                       durability_class = EXCLUDED.durability_class, \
                       source_policy_org = EXCLUDED.source_policy_org, \
                       source_policy_hash = EXCLUDED.source_policy_hash \
                     WHERE (environment_policies.expected_environment, \
                            environment_policies.durability_class, \
                            environment_policies.source_policy_org, \
                            environment_policies.source_policy_hash) \
                           IS DISTINCT FROM \
                           (EXCLUDED.expected_environment, EXCLUDED.durability_class, \
                            EXCLUDED.source_policy_org, EXCLUDED.source_policy_hash)",
                    schema.quoted()
                ),
                &[
                    &tenant_id,
                    &source.environment(),
                    &wanted_class,
                    &source.source_policy_org.as_ref(),
                    &source.source_policy_hash.as_ref(),
                ],
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
    let retires_node_runs = matches!(
        plan.actions.as_slice(),
        [action] if action.kind == RunPlaneActionKind::RetireNodeRuns
    );
    if !retires_node_runs
        && let Some(action) = dispatch_reader_read_surface_action(schema, &obs, !plan.is_noop())
    {
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
        ensure_runtime_roles(client).await?;
        for action in &plan.actions[applied..] {
            client
                .batch_execute(&action.sql)
                .await
                .with_context(|| format!("apply {:?} {}", action.kind, action.target))?;
        }
    }
    Ok(plan)
}

async fn ensure_runtime_roles(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    client
        .batch_execute(&wamn_control_provision::sql::ensure_app_acl_role_sql())
        .await
        .context("ensure wamn_app role")?;
    client
        .batch_execute(wamn_schema_control::ensure_scenario_author_role_sql())
        .await
        .context("ensure host-only wamn_scenario_author role")?;
    client
        .batch_execute(
            "SELECT pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
             REVOKE wamn_scenario_author FROM wamn_app",
        )
        .await
        .context("separate guest and scenario-author roles")
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
        ..Default::default()
    };
    for row in client
        .query(select_effect_table_privileges_sql(), &[&schema.as_str()])
        .await
        .context("read direct effect-table privileges")?
    {
        obs.effect_table_privileges
            .entry((row.get(0), row.get(1)))
            .or_default()
            .insert(row.get(2));
    }
    for row in client
        .query(
            select_effect_table_effective_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effective effect-table privileges")?
    {
        let table: String = row.get(0);
        obs.effect_table_effective_privileges
            .entry((table.clone(), row.get(1)))
            .or_default()
            .insert(row.get(2));
        obs.effect_table_owners.insert(table, row.get(3));
    }
    for row in client
        .query(
            select_effect_table_effective_column_privileges_sql(),
            &[&schema.as_str()],
        )
        .await
        .context("read effective effect-table column privileges")?
    {
        obs.effect_table_effective_column_privileges
            .entry((row.get(0), row.get(1)))
            .or_default()
            .insert(row.get(2));
    }

    obs.app_is_scenario_author_member = client
        .query_one(select_app_scenario_author_membership_sql(), &[])
        .await
        .context("read guest scenario-author membership")?
        .get(0);
    obs.app_run_queue_authority = client
        .query_one(select_app_run_queue_authority_sql(), &[&schema.as_str()])
        .await
        .context("read guest run-queue authority")?
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
    let effect_tables: Vec<&str> = [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ]
    .into_iter()
    .filter(|table| obs.tables.contains_key(*table))
    .collect();
    if !effect_tables.is_empty() {
        let count_sql = effect_tables
            .iter()
            .map(|table| format!("SELECT count(*) FROM {}.{}", schema.quoted(), table))
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        obs.effect_record_rows = client
            .query_one(
                &format!("SELECT COALESCE(sum(n), 0)::bigint FROM ({count_sql}) AS counts(n)"),
                &[],
            )
            .await
            .context("count effect records for effect-table cutover")?
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
            obs.catalog_tables.insert(table.clone());
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

    /// The observation predates every action, and the table cutover drops and
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
    /// [`PRE_ROLE_BOOTSTRAP_ACTIONS`]: when the retired plan table is present,
    /// the plan [`reconcile`] walks is exactly the carrier cutover, and that
    /// kind is allowlisted — so the whole plan runs
    /// ahead of `ensure_wamn_app_role`, which never touches a database about to
    /// refuse.
    ///
    /// Two independent facts hold that up, and this is the only place in this
    /// crate pinning either: the kind's membership above, and
    /// `wamn_schema_control::plan_run_plane`'s early return, which is what makes
    /// the cutover the WHOLE plan rather than the head of an 89-action one.
    /// Deleting either turns this red.
    #[test]
    fn execution_bundle_retirement_leads_the_pre_bootstrap_prefix() {
        let obs = RunPlaneObservation {
            catalog_tables: BTreeSet::from(["execution_bundles".to_string()]),
            ..Default::default()
        };

        let plan = plan_run_plane(&schema(), &obs);
        assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
        assert_eq!(
            plan.actions[0].kind,
            RunPlaneActionKind::RetireExecutionBundles
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
                RunPlaneActionKind::RetireNodeRuns,
                RunPlaneActionKind::RetireExecutionBundles,
                RunPlaneActionKind::FrameIdentityCutover,
                RunPlaneActionKind::RetireLegacyAdmissionSurface,
                RunPlaneActionKind::EffectTableCutover,
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
