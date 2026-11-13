//! Observe and reconcile the current run plane and its runtime identities.
//!
//! Dry runs read database facts without changing roles or schema.
//! Apply requires SUPERUSER or BYPASSRLS to observe every project row.

use sqlx::{Connection as _, Row as _};
use std::collections::BTreeSet;
use std::error::Error as StdError;

use anyhow::Context as _;

use wamn_control_provision::{
    APP_SCHEMA_SQL, PlatformComponent, bind_platform_principal_sql, platform_principals_sql,
    project_env_database_name, validate_project_env,
};
use wamn_registry::Triple;
use wamn_schema_control::{
    BareSchemaName, RowPolicyObservation, RowSecurityObservation, RunPlaneObservation,
    RunPlanePlan, catalog_schema_present_sql, plan_run_plane, select_app_run_queue_authority_sql,
    select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_environment_policy_policies_sql,
    select_environment_policy_row_security_sql, select_run_capture_privileges_sql,
    select_run_plane_helper_functions_sql, select_schema_checks_sql, select_schema_columns_sql,
    select_schema_indexes_sql, select_schema_triggers_sql,
};

/// Inputs of one run-plane reconciliation.
#[derive(Debug)]
pub struct ReconcileRunPlaneRequest {
    /// Administrative Postgres URL to the system registry. The reconciler reads
    /// the project-env's stored instance suffix here before resolving policy;
    /// admission never connects to this database.
    pub system_database_url: String,

    /// Administrative Postgres URL to the exact registry-derived project
    /// database. Observation and apply require SUPERUSER or BYPASSRLS so
    /// forced-RLS rows cannot be skipped.
    pub admin_database_url: String,

    /// Registry organization owning the environment policy.
    pub org: String,

    /// Registry project owning the exact provisioned database target.
    pub project: String,

    /// Environment name. The policy is the one the environment row names.
    pub env: String,

    /// The project-env schema the run-plane tables live in (e.g.
    /// `wamn_runner_demo`, `poc_f1`).
    pub schema: String,

    /// Plan without applying (strictly read-only).
    pub dry_run: bool,

    /// Publishes the user rows the reconciliation wrote or removed.
    pub changes: wamn_broker::Publisher,
}

impl ReconcileRunPlaneRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
        wamn_registry::project_env_key(&self.org, &self.project, &self.env)
    }
}

/// Result of one run-plane reconciliation.
#[derive(Debug)]
pub struct ReconcileRunPlaneOutcome {
    /// The plan that was applied, or that would apply under a dry run.
    pub plan: RunPlanePlan,
    /// Whether the project's environment policy changed, or would change.
    pub policy_changed: bool,
    /// What the project's `platform` schema and identity rows needed.
    pub project_env_key_identity: ProjectEnvKeyIdentityOutcome,
}

/// Stable class for a run-plane target-identity refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileTargetErrorType {
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
    type_: ReconcileTargetErrorType,
    context: String,
    expected_database: Option<String>,
    actual_database: Option<String>,
    source: Option<anyhow::Error>,
}

impl ReconcileTargetError {
    fn with_source(
        type_: ReconcileTargetErrorType,
        context: impl Into<String>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            type_,
            context: format!("{RECONCILE_TARGET_REFUSAL_PREFIX}: {}", context.into()),
            expected_database: None,
            actual_database: None,
            source: Some(source.into()),
        }
    }

    fn mismatch(triple: &Triple, expected_database: String, actual_database: String) -> Self {
        Self {
            type_: ReconcileTargetErrorType::DatabaseTarget,
            context: format!(
                "{RECONCILE_TARGET_REFUSAL_PREFIX}: database target mismatch for registry triple {triple}: expected {expected_database:?}, actual {actual_database:?}"
            ),
            expected_database: Some(expected_database),
            actual_database: Some(actual_database),
            source: None,
        }
    }

    /// Return the stable refusal class.
    pub const fn error_type(&self) -> ReconcileTargetErrorType {
        self.type_
    }

    /// Whether the trusted triple failed to resolve in the registry.
    pub const fn is_registry_target(&self) -> bool {
        matches!(self.type_, ReconcileTargetErrorType::RegistryTarget)
    }

    /// Whether the connected database failed the exact target test.
    pub const fn is_database_target(&self) -> bool {
        matches!(self.type_, ReconcileTargetErrorType::DatabaseTarget)
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
    reconcile_run_plane_with_migrations(
        args,
        &crate::platform_migrations::embedded_sources(
            wamn_control_provision::schema_migrations::MigrationDatabase::Project,
        ),
    )
    .await
}

/// Reconcile with explicit image files, using the same executor as project schema apply.
pub async fn reconcile_run_plane_with_migrations(
    args: ReconcileRunPlaneRequest,
    sources: &[wamn_schema_control::MigrationSource],
) -> anyhow::Result<ReconcileRunPlaneOutcome> {
    anyhow::ensure!(
        ![&args.org, &args.project, &args.env]
            .iter()
            .any(|part| part.is_empty()),
        "org, project and environment must not be empty"
    );
    let schema = BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid --schema {:?}", args.schema))?;
    let triple = Triple::new(&args.org, args.env.as_str(), &args.project);
    validate_project_env(&args.org, &args.project, &args.env).map_err(|source| {
        ReconcileTargetError::with_source(
            ReconcileTargetErrorType::RegistryTarget,
            format!("registry target identity {triple} is invalid"),
            source,
        )
    })?;
    let instance =
        crate::provision_project_env::read_project_env_instance(&args.system_database_url, &triple)
            .await
            .map_err(|source| {
                ReconcileTargetError::with_source(
                    ReconcileTargetErrorType::RegistryTarget,
                    format!("registry target {triple} has no usable recorded instance"),
                    source,
                )
            })?;
    let expected_database =
        project_env_database_name(&args.org, &args.project, &args.env, &instance);
    let mut client = wamn_postgres::connect(&args.admin_database_url)
        .await
        .map_err(|source| {
            ReconcileTargetError::with_source(
                ReconcileTargetErrorType::DatabaseTarget,
                format!("database target for registry triple {triple} did not connect"),
                source,
            )
        })?;

    let actual_database = sqlx::query("SELECT pg_catalog.current_database()::text")
        .fetch_one(&mut client)
        .await
        .map(|row| row.get::<String, _>(0))
        .map_err(|source| {
            ReconcileTargetError::with_source(
                ReconcileTargetErrorType::DatabaseTarget,
                format!("database target for registry triple {triple} did not identify itself"),
                source,
            )
        });
    let actual_database = match actual_database {
        Ok(actual_database) => actual_database,
        Err(error) => {
            drop(client);
            return Err(error.into());
        }
    };
    if actual_database != expected_database {
        drop(client);
        return Err(
            ReconcileTargetError::mismatch(&triple, expected_database, actual_database).into(),
        );
    }
    let result = async {
        // The environment row names the policy
        // (docs/architecture/overview.md#environment-lifecycle).
        let policy = crate::provision_project_env::read_project_env_policy(
            &args.system_database_url,
            &triple,
        )
        .await?;
        let source_policy =
            wamn_release_control::verification_policy::read_authoritative_environment_policy(
                &args.system_database_url,
                &args.org,
                &policy,
            )
            .await?;
        // Validate completed initialization before any substrate reconciliation writes.
        if sqlx::query("SELECT to_regclass('platform.schema_baseline') IS NOT NULL")
            .fetch_one(&mut client)
            .await?
            .get::<bool, _>(0)
        {
            wamn_package_control::schema_baseline::observe(
                &mut client,
                wamn_control_provision::schema_migrations::MigrationDatabase::Project,
                sources,
            )
            .await?;
        }
        let mut system = wamn_postgres::connect(&args.system_database_url).await?;
        let reset_sources =
            crate::environment::sql::reset::migration_sources(&mut system, &triple, sources)
                .await?;
        drop(system);
        let plan = reconcile(&mut client, &schema, !args.dry_run).await?;
        let policy_changed = converge_environment_policy(
            &mut client,
            &schema,
            &args.project_env_key(),
            &args.env,
            &source_policy,
            !args.dry_run,
        )
        .await?;
        // After the catalog schema, because app-schema.sql's stamp triggers
        // call the record-history function that composition installs.
        let mut system = wamn_postgres::connect(&args.system_database_url).await?;

        let identity_result = async {
            let mut authority = system.begin().await?;
            if !args.dry_run {
                wamn_platform_identity::org::lock_org_memberships(&mut authority, &args.org)
                    .await?;
            }
            let identity_source = read_project_env_key_identity_source(
                &mut authority,
                &args.org,
                &args.project,
                &args.env,
            )
            .await?;
            let project_env_key = args.project_env_key();
            let target = wamn_platform_identity::application::ApplicationTarget {
                org: &args.org,
                project: &args.project,
                env: &args.env,
                project_env_key: &project_env_key,
            };
            let outcome = converge_project_env_key_identity(
                &mut client,
                &project_env_key,
                &identity_source,
                !args.dry_run,
                &args.changes,
                &mut authority,
                target,
            )
            .await?;
            authority.commit().await?;
            anyhow::Ok(outcome)
        }
        .await;
        drop(system);
        let project_env_key_identity = identity_result?;
        if !args.dry_run
            && (project_env_key_identity.app_schema_installed || reset_sources.is_some())
        {
            crate::platform_migrations::initialize_project(
                &mut client,
                reset_sources.as_deref().unwrap_or(sources),
            )
            .await?;
        }
        Ok::<_, anyhow::Error>((plan, policy_changed, project_env_key_identity))
    }
    .await;
    drop(client);
    let (plan, policy_changed, project_env_key_identity) = result?;
    Ok(ReconcileRunPlaneOutcome {
        plan,
        policy_changed,
        project_env_key_identity,
    })
}

/// One service principal that holds a role in the project being reconciled.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServicePrincipal {
    id: String,
    subject: String,
    display_name: String,
}

/// One user principal with active org and environment memberships.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UserPrincipal {
    id: String,
}

/// The deployment and registry facts a project's identity rows are built from.
///
/// All of them come from the system registry, never from the project database,
/// so a project cannot name its own platform rows, invent a service principal,
/// or admit a user who was never granted a membership.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectEnvKeyIdentitySource {
    /// `registry.meta.platform_domain`, absent until the bootstrap sets it.
    platform_domain: Option<String>,
    services: Vec<ServicePrincipal>,
    users: Vec<UserPrincipal>,
    /// The committed user and service principals that derive application admin.
    admins: BTreeSet<String>,
}

/// Every service principal holding a role in one project, with the deployment
/// platform domain that names the platform rows.
const PROJECT_ENV_KEY_IDENTITY_SOURCE_SQL: &str = "SELECT p.id::text, p.subject, p.display_name \
     FROM identity.project_roles AS r \
     JOIN identity.principals AS p ON p.id = r.principal_id \
     WHERE r.org = $1 AND r.project = $2 \
       AND p.type = 'service' AND p.status = 'active' \
     GROUP BY p.id, p.subject, p.display_name \
     ORDER BY p.subject";

/// Every user principal that `grant-project-env-membership` admitted to ONE
/// project environment (`wamn-0h0g.9.18`).
///
/// `identity.project_env_memberships` carries `org`, `project` and `env`
/// itself, so the project needs no join beyond the principal that names the
/// user. The env narrows the read: one project database serves one project
/// environment, and a member of `dev` gets no row in the `prod` project.
const PROJECT_ENV_KEY_IDENTITY_USERS_SQL: &str = "SELECT p.id::text,p.email,p.display_name \
    FROM identity.principals p WHERE identity.has_project_access(p.id,$1,$3,$2) ORDER BY p.email";

const PROJECT_ENV_KEY_IDENTITY_ADMINS_SQL: &str = "SELECT p.id::text FROM identity.principals p \
    WHERE p.type='service' AND p.status='active' AND $3::text IS NOT NULL AND EXISTS \
    (SELECT FROM identity.project_roles r WHERE r.principal_id=p.id AND r.org=$1 AND r.project=$2 AND r.role=$4)";

async fn read_project_env_key_identity_source(
    client: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    project: &str,
    env: &str,
) -> anyhow::Result<ProjectEnvKeyIdentitySource> {
    let platform_domain: Option<String> =
        sqlx::query(wamn_registry::sql::IDENTITY_PLATFORM_SRC_APPLICATION_PROJECT_AUTHORITY)
            .fetch_one(&mut **client)
            .await
            .context("read the deployment platform domain")?
            .get(0);
    let services = sqlx::query(PROJECT_ENV_KEY_IDENTITY_SOURCE_SQL)
        .bind(org)
        .bind(project)
        .fetch_all(&mut **client)
        .await
        .context("read the project's service principals")?
        .into_iter()
        .map(|row| ServicePrincipal {
            id: row.get(0),
            subject: row.get(1),
            display_name: row.get(2),
        })
        .collect();
    let users = sqlx::query(PROJECT_ENV_KEY_IDENTITY_USERS_SQL)
        .bind(org)
        .bind(project)
        .bind(env)
        .fetch_all(&mut **client)
        .await
        .context("read the project environment's user members")?
        .into_iter()
        .map(|row| UserPrincipal { id: row.get(0) })
        .collect();
    let admins = sqlx::query(PROJECT_ENV_KEY_IDENTITY_ADMINS_SQL)
        .bind(org)
        .bind(project)
        .bind(env)
        .bind(wamn_platform_identity::control::PROJECT_ADMIN_ROLE)
        .fetch_all(&mut **client)
        .await
        .context("read the project environment's administrators")?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    Ok::<_, anyhow::Error>(ProjectEnvKeyIdentitySource {
        platform_domain,
        services,
        users,
        admins,
    })
}

/// What one project_env_keyentity convergence did, or would do under a dry run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectEnvKeyIdentityOutcome {
    /// `deploy/sql/app-schema.sql` was absent and was installed.
    pub app_schema_installed: bool,
    /// Platform rows written, out of the closed component list.
    pub platform_rows_written: usize,
    /// Service rows written for principals holding a role in this project.
    pub service_rows_written: usize,
    /// User rows written for users with a membership in this environment.
    pub user_rows_written: usize,
    /// User rows removed because their user has no membership in this
    /// environment any more, or is no longer an active user.
    pub user_rows_removed: usize,
    /// `admin` rows written for administrators that had none, which repairs
    /// an interrupted grant and the grants of `wamn-ctl invite`.
    pub admin_rows_written: usize,
}

/// Install the project's `platform` schema and its identity rows
/// (`wamn-0h0g.9.15`).
///
/// This is the production application of `deploy/sql/app-schema.sql` and of the
/// platform principal rows. Both were manual `psql` steps that
/// `docs/operations/deployment.md` told an operator to run, and a project that
/// skipped them refused every stamped write with `actor-required`.
///
/// It runs here and not in `provision-project-env` because this verb already
/// holds an administrative connection to the exact project-env database, and
/// because the record-history functions the stamp triggers call arrive with the
/// catalog schema this same run installs.
///
/// A service row is written for every service principal holding a role in the
/// project. Its email is `<subject>@<platform-domain>`: a service is not a
/// user, so it carries the deployment's own domain, like a platform row, and
/// the subject keeps it unique inside the project.
///
/// A user row is written for every user that `grant-project-env-membership`
/// admitted to this environment (`wamn-0h0g.9.18`). It carries the user's own
/// `identity.principals.email` and display name. A new member gets the row at
/// the next reconcile, not at the moment of the grant, because the grant flow
/// holds no project connection.
///
/// A user row whose user is not in that source any more is removed, and the
/// foreign keys remove its role assignments and API keys
/// (docs/architecture/overview.md#organization-membership-and-administration).
/// Service and platform rows keep their own sources, so this removes neither.
///
/// `apply=false` observes only and writes nothing.
async fn converge_project_env_key_identity(
    client: &mut sqlx::PgConnection,
    project_env_key: &str,
    source: &ProjectEnvKeyIdentitySource,
    apply: bool,
    changes: &wamn_broker::Publisher,
    system: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target: wamn_platform_identity::application::ApplicationTarget<'_>,
) -> anyhow::Result<ProjectEnvKeyIdentityOutcome> {
    anyhow::ensure!(!project_env_key.is_empty(), "project must not be empty");
    let mut outcome = ProjectEnvKeyIdentityOutcome::default();
    let app_schema_present: bool =
        sqlx::query("SELECT to_regclass('platform.\"user\"') IS NOT NULL")
            .fetch_one(&mut *client)
            .await
            .context("probe the platform schema")?
            .get(0);

    // Every relation app-schema.sql creates has a stamp trigger, so the
    // record-history function has to exist before the file runs. The catalog
    // composition carries it, and this same reconcile installs that. A project
    // whose catalog predates record-history reaches here without it, and the
    // failure is clearer here than inside a CREATE TRIGGER.
    let stamp_function_present: bool = sqlx::query(
        "SELECT EXISTS ( SELECT FROM pg_proc AS p \
               JOIN pg_namespace AS n ON n.oid = p.pronamespace \
              WHERE n.nspname = 'wamn_history' AND p.proname = 'stamp_row' )",
    )
    .fetch_one(&mut *client)
    .await
    .context("probe the record-history stamp function")?
    .get(0);

    let missing_platform: Vec<PlatformComponent> = if app_schema_present {
        let present: BTreeSet<String> =
            sqlx::query("SELECT id::text FROM platform.\"user\" WHERE type = 'platform'")
                .fetch_all(&mut *client)
                .await
                .context("read the project's platform rows")?
                .into_iter()
                .map(|row| row.get::<String, _>(0))
                .collect();
        PlatformComponent::ALL
            .into_iter()
            .filter(|component| !present.contains(&component.principal_id().to_string()))
            .collect()
    } else {
        PlatformComponent::ALL.into_iter().collect()
    };

    let (present_users, present_user_type): (BTreeSet<String>, BTreeSet<String>) =
        if app_schema_present {
            sqlx::query("SELECT id::text, type = 'user' FROM platform.\"user\"")
                .fetch_all(&mut *client)
                .await
                .context("read the project's user rows")?
                .into_iter()
                .fold(Default::default(), |(mut all, mut users), row| {
                    let id: String = row.get(0);
                    if row.get::<bool, _>(1) {
                        users.insert(id.clone());
                    }
                    all.insert(id);
                    (all, users)
                })
        } else {
            Default::default()
        };
    let missing_services: Vec<&ServicePrincipal> = source
        .services
        .iter()
        .filter(|service| !present_users.contains(&service.id))
        .collect();
    let missing_users: Vec<&UserPrincipal> = source
        .users
        .iter()
        .filter(|user| !present_users.contains(&user.id))
        .collect();

    outcome.app_schema_installed = !app_schema_present;
    outcome.platform_rows_written = missing_platform.len();
    outcome.service_rows_written = missing_services.len();
    let members: BTreeSet<&str> = source.users.iter().map(|user| user.id.as_str()).collect();
    let stale_users: Vec<&String> = present_user_type
        .iter()
        .filter(|id| !members.contains(id.as_str()))
        .collect();
    outcome.user_rows_written = missing_users.len();
    outcome.user_rows_removed = stale_users.len();
    let present_admins: BTreeSet<String> = if app_schema_present {
        sqlx::query(
            "SELECT user_id::text FROM platform.user_role \
                  WHERE role_name = $1",
        )
        .bind(wamn_identity_protocol::ADMIN_ROLE)
        .fetch_all(&mut *client)
        .await
        .context("read the project's admin rows")?
        .into_iter()
        .map(|row| row.get(0))
        .collect()
    } else {
        BTreeSet::new()
    };
    let missing_admins: Vec<&UserPrincipal> = source
        .users
        .iter()
        .filter(|user| source.admins.contains(&user.id) && !present_admins.contains(&user.id))
        .collect();
    outcome.admin_rows_written = source.admins.difference(&present_admins).count();
    let stale_admins: Vec<_> = present_admins.difference(&source.admins).collect();
    if !apply
        || (app_schema_present
            && missing_platform.is_empty()
            && present_users.contains(&wamn_identity_protocol::workflow_user_id().to_string())
            && missing_services.is_empty()
            && missing_users.is_empty()
            && stale_users.is_empty()
            && missing_admins.is_empty()
            && stale_admins.is_empty()
            && source.admins.is_subset(&present_admins))
    {
        return Ok(outcome);
    }
    anyhow::ensure!(
        stamp_function_present,
        "record-history-absent: this database has no wamn_history.stamp_row function, which every \
         platform stamp trigger calls. deploy/sql/record-history.sql arrives with the catalog \
         schema, so reconcile the catalog schema before the project identity rows"
    );
    let Some(platform_domain) = source.platform_domain.as_deref() else {
        anyhow::bail!(
            "platform-domain-unset: registry.meta.platform_domain names the domain of the \
             platform principal emails, and this deployment has not set it. Set it once against \
             the system database, for example UPDATE registry.meta SET platform_domain = \
             'example.invalid'"
        );
    };
    let platform_rows =
        platform_principals_sql(platform_domain).map_err(|source| anyhow::anyhow!("{source}"))?;

    let mut transaction = client
        .begin()
        .await
        .context("open the project identity transaction")?;
    let mut drops = wamn_broker::auth_changes::AuthDrops::default();
    if !app_schema_present {
        let owner: String = sqlx::query(
            "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()",
        )
        .fetch_one(&mut *transaction)
        .await?
        .get(0);
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SET LOCAL ROLE {}; {APP_SCHEMA_SQL} RESET ROLE;",
            postgres_protocol::escape::escape_identifier(&owner)
        )))
        .execute(&mut *transaction)
        .await
        .map(drop)
        .context("install deploy/sql/app-schema.sql")?;
        wamn_package_control::schema_baseline::record_project(&mut transaction)
            .await
            .context("record the project schema baseline")?;
        if let Some(pending) = crate::provision_project_env::registry::pending_database(
            system,
            &Triple::new(target.org, target.env, target.project),
        )
        .await?
        {
            let identity = pending["initialization_identity"]
                .as_str()
                .context("reset initialization identity")?;
            sqlx::query("UPDATE platform.schema_baseline SET installed_at=$1::text::timestamptz WHERE singleton").bind(identity).execute(&mut *transaction).await.map(|result| result.rows_affected())?;
        }
    }
    // Every write below stamps `wamn:provisioning`, and its own row stamps
    // itself, because the bind comes first and lasts for this transaction.
    sqlx::raw_sql(sqlx::AssertSqlSafe(platform_rows.clone()))
        .execute(&mut *transaction)
        .await
        .map(drop)
        .context("write the project's platform principal rows")?;
    if !missing_services.is_empty()
        || !missing_users.is_empty()
        || !stale_users.is_empty()
        || !missing_admins.is_empty()
        || !stale_admins.is_empty()
        || !source.admins.is_subset(&present_admins)
    {
        sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        )))
        .execute(&mut *transaction)
        .await
        .map(drop)
        .context("bind wamn:provisioning for the service and user rows")?;
        for service in &missing_services {
            let principal_id: wamn_identity_protocol::PrincipalId = service.id.parse()?;
            wamn_platform_identity::application::write_service(
                &mut transaction,
                project_env_key,
                wamn_platform_identity::application::ApplicationUser {
                    principal_id: &principal_id,
                    email: &format!("{}@{platform_domain}", service.subject),
                    display_name: &service.display_name,
                },
                &mut drops,
            )
            .await?;
        }
        let mut principals = present_user_type.clone();
        principals.extend(source.users.iter().map(|user| user.id.clone()));
        principals.extend(source.services.iter().map(|service| service.id.clone()));
        principals.extend(present_admins.iter().cloned());
        for id in principals {
            let principal = id.parse()?;
            wamn_platform_identity::application::project_authority(
                system,
                &mut transaction,
                target,
                &principal,
                &mut drops,
            )
            .await?;
        }
    }
    transaction
        .commit()
        .await
        .context("commit the project identity rows")?;
    changes.publish(drops).await;
    Ok(outcome)
}

/// Converge one source-attested environment policy into the project-local
/// relation owned by this reconciler. `apply=false` observes only, including a
/// from-zero schema where the relation or additive source carriers do not yet
/// exist.
///
/// `expected_environment` is the environment's name, which the run plane
/// and publish compare with the target. The policy the environment row names
/// supplies the source identity
/// (docs/architecture/overview.md#environment-lifecycle).
pub(crate) async fn converge_environment_policy(
    client: &mut sqlx::PgConnection,
    schema: &BareSchemaName,
    project_env_key: &str,
    environment: &str,
    source: &wamn_release_control::verification_policy::AuthoritativeEnvironmentPolicy,
    apply: bool,
) -> anyhow::Result<bool> {
    anyhow::ensure!(!project_env_key.is_empty(), "project must not be empty");
    let table_present: bool = sqlx::query("SELECT pg_catalog.to_regclass(pg_catalog.format('%I.environment_policies', $1::text)) IS NOT NULL").bind(schema.as_str()).fetch_one(&mut *client)
        .await
        .context("observe project-local environment policy relation")?
        .get(0);
    let source_carriers_present: bool = if table_present {
        sqlx::query("SELECT count(*) = 2 FROM pg_catalog.pg_attribute AS attribute \
                  WHERE attribute.attrelid = \
                          pg_catalog.to_regclass(pg_catalog.format('%I.environment_policies', $1::text)) \
                    AND attribute.attname IN ('source_policy_org', 'source_policy_hash') \
                    AND attribute.attnum > 0 AND NOT attribute.attisdropped").bind(schema.as_str()).fetch_one(&mut *client)
            .await
            .context("observe project-local environment policy source carriers")?
            .get(0)
    } else {
        false
    };
    let current = if source_carriers_present {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT expected_environment, \
                            source_policy_org, source_policy_hash \
                       FROM {}.environment_policies WHERE project_env_key = $1",
            schema.quoted()
        )))
        .bind(project_env_key)
        .fetch_optional(&mut *client)
        .await
        .context("observe project-local environment policy")?
        .map(|row| {
            (
                row.get::<String, _>(0),
                row.get::<Option<String>, _>(1),
                row.get::<Option<String>, _>(2),
            )
        })
    } else {
        None
    };
    let changed =
        current
            .as_ref()
            .is_none_or(|(current_environment, current_org, current_hash)| {
                current_environment != environment
                    || current_org.as_deref() != Some(source.source_policy_org.as_ref())
                    || current_hash.as_deref() != Some(source.source_policy_hash.as_ref())
            });
    if apply && changed {
        anyhow::ensure!(
            table_present && source_carriers_present,
            "run-plane reconciliation did not create the environment policy source carriers"
        );
        let mut transaction = client.begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_control_provision::bind_platform_principal_sql(PlatformComponent::Provisioning),
        ))
        .execute(&mut *transaction)
        .await
        .map(drop)?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {}.environment_policies \
                       (project_env_key, expected_environment, \
                        source_policy_org, source_policy_hash) \
                     VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (project_env_key) DO UPDATE SET \
                       expected_environment = EXCLUDED.expected_environment, \
                       source_policy_org = EXCLUDED.source_policy_org, \
                       source_policy_hash = EXCLUDED.source_policy_hash \
                     WHERE (environment_policies.expected_environment, \
                            environment_policies.source_policy_org, \
                            environment_policies.source_policy_hash) \
                           IS DISTINCT FROM \
                           (EXCLUDED.expected_environment, \
                            EXCLUDED.source_policy_org, EXCLUDED.source_policy_hash)",
            schema.quoted()
        )))
        .bind(project_env_key)
        .bind(environment)
        .bind(source.source_policy_org.as_ref())
        .bind(source.source_policy_hash.as_ref())
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected())
        .context("converge project-local environment policy")?;
        transaction.commit().await?;
    }
    Ok(changed)
}

/// The reusable core: observe the schema, plan, and — when `apply` — ensure the
/// `wamn_app` role the sections GRANT to and execute the actions in order.
/// Returns the plan (for reporting / condition assertions). Shared by the CLI verb
/// and the live acceptance test so both exercise one code path.
pub async fn reconcile(
    client: &mut sqlx::PgConnection,
    schema: &BareSchemaName,
    apply: bool,
) -> anyhow::Result<RunPlanePlan> {
    let bypasses_forced_rls: bool = sqlx::query(
        "SELECT rolsuper OR rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = CURRENT_USER",
    )
    .fetch_one(&mut *client)
    .await
    .context("verify reconcile admin bypasses forced RLS")?
    .get(0);
    anyhow::ensure!(
        bypasses_forced_rls,
        "reconcile-run-plane requires SUPERUSER or BYPASSRLS; a plain schema owner cannot completely observe forced-RLS rows"
    );

    let obs = observe(client, schema).await?;
    let plan = plan_run_plane(schema, &obs);
    if apply {
        ensure_runtime_roles(client).await?;
        let owner: String = sqlx::query(
            "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()",
        )
        .fetch_one(&mut *client)
        .await?
        .get(0);
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SET ROLE {}",
            postgres_protocol::escape::escape_identifier(&owner)
        )))
        .execute(&mut *client)
        .await
        .map(drop)?;
        for action in &plan.actions {
            sqlx::raw_sql(sqlx::AssertSqlSafe(action.sql.clone()))
                .execute(&mut *client)
                .await
                .map(drop)
                .with_context(|| format!("apply {:?} {}", action.type_, action.target))?;
        }
        sqlx::raw_sql("RESET ROLE")
            .execute(&mut *client)
            .await
            .map(drop)?;
    }
    Ok(plan)
}

pub async fn ensure_runtime_roles(client: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    for family in [
        wamn_control_provision::WorkloadRoleFamily::Retention,
        wamn_control_provision::WorkloadRoleFamily::AuditRetention,
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_control_provision::sql::ensure_workload_acl_role_sql(family),
        ))
        .execute(&mut *client)
        .await
        .map(drop)?;
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_platform_group_role_sql(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_app_acl_role_sql(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .context("ensure wamn_app role")?;
    Ok(())
}

/// Read the database facts that the planner uses.
async fn observe(
    client: &mut sqlx::PgConnection,
    schema: &BareSchemaName,
) -> anyhow::Result<RunPlaneObservation> {
    let mut obs = RunPlaneObservation {
        app_run_queue_authority: sqlx::query(sqlx::AssertSqlSafe(
            (select_app_run_queue_authority_sql()).to_string(),
        ))
        .bind(schema.as_str())
        .fetch_one(&mut *client)
        .await
        .context("read guest run-queue authority")?
        .get(0),
        ..Default::default()
    };
    let capture_privileges_sql = select_run_capture_privileges_sql();
    let capture_privileges = sqlx::query(sqlx::AssertSqlSafe(capture_privileges_sql.clone()))
        .bind(schema.as_str())
        .fetch_one(&mut *client)
        .await
        .context("read guest run-capture privileges")?;
    obs.app_run_capture_privileges = (
        capture_privileges.get(0),
        capture_privileges.get(1),
        capture_privileges.get(2),
    );
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_authoring_table_privileges_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
    .await
    .context("read authoring table privileges")?
    {
        obs.authoring_table_privileges
            .entry((row.get(0), row.get(1), row.get(2)))
            .or_default()
            .insert(row.get(3));
    }
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_authoring_effective_table_privileges_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
    .await
    .context("read effective authoring table privileges")?
    {
        obs.authoring_effective_table_privileges
            .entry((row.get(0), row.get(1), row.get(2)))
            .or_default()
            .insert(row.get(3));
    }
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_authoring_effective_column_privileges_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
    .await
    .context("read effective authoring column privileges")?
    {
        obs.authoring_effective_column_privileges
            .entry((row.get(0), row.get(1), row.get(2)))
            .or_default()
            .insert(row.get(3));
    }
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_authoring_table_owners_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
    .await
    .context("read authoring table owners")?
    {
        obs.authoring_table_owners
            .insert((row.get(0), row.get(1)), row.get(2));
    }

    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_schema_columns_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
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
    if let Some(row) = sqlx::query(sqlx::AssertSqlSafe(
        (select_environment_policy_row_security_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_optional(&mut *client)
    .await
    .context("read environment-policy row-security flags")?
    {
        let mut row_security = RowSecurityObservation {
            enabled: row.get(0),
            forced: row.get(1),
            ..Default::default()
        };
        for row in sqlx::query(sqlx::AssertSqlSafe(
            (select_environment_policy_policies_sql()).to_string(),
        ))
        .bind(schema.as_str())
        .fetch_all(&mut *client)
        .await
        .context("read environment-policy row-security policies")?
        {
            row_security.policies.insert(
                row.get(0),
                RowPolicyObservation {
                    command: row.get(1),
                    permissive: row.get(2),
                    roles: row.get::<Vec<String>, _>(3).into_iter().collect(),
                    using_expression: row.get(4),
                    check_expression: row.get(5),
                },
            );
        }
        obs.environment_policy_row_security = Some(row_security);
    }
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_schema_indexes_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
    .await
    .context("read schema indexes")?
    {
        obs.indexes.insert(row.get(0), row.get(1));
    }
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_schema_checks_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
    .await
    .context("read schema check constraints")?
    {
        obs.checks.insert((row.get(0), row.get(1)), row.get(2));
    }
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_schema_triggers_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
    .await
    .context("read schema triggers")?
    {
        obs.triggers.insert((row.get(0), row.get(1)), row.get(2));
    }
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (select_run_plane_helper_functions_sql()).to_string(),
    ))
    .bind(schema.as_str())
    .fetch_all(&mut *client)
    .await
    .context("read run-plane helper functions")?
    {
        obs.helper_functions.insert(row.get(0), row.get(1));
    }
    obs.catalog_schema_present = sqlx::query(sqlx::AssertSqlSafe(
        (catalog_schema_present_sql()).to_string(),
    ))
    .fetch_one(&mut *client)
    .await
    .context("probe catalog schema")?
    .get(0);
    if obs.catalog_schema_present {
        for row in sqlx::query(sqlx::AssertSqlSafe(
            (select_schema_columns_sql()).to_string(),
        ))
        .bind("catalog")
        .fetch_all(&mut *client)
        .await
        .context("read catalog tables")?
        {
            let table: String = row.get(0);
            obs.catalog_tables.insert(table.clone());
        }
    }
    Ok(obs)
}
