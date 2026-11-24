//! Statements and transaction scopes for environment deployment.
use crate::environment_sql::{
    close_capture_gap_sql, insert_capture_gap_sql, lock_newest_capture_gap_sql,
    select_event_reader_origin_sql, upsert_event_reader_sql,
};
use crate::provisioning_transaction as provision_project_env;
use chrono::{DateTime, Utc};
use std::time::Duration;
use wamn_environment::environment::capture_gap::ClosedCaptureGap;
use wamn_environment::environment::database::{
    CaptureRegistration, CaptureRepair, CaptureStatus, ConnectionBinding, IntegrationBinding,
    MISSING_CAPTURE_REASON, PendingCaptureGap,
};
use wamn_package::canonical_name::CanonicalName;
use wamn_platform_install::database::ConnectionConfig;
use wamn_registry::{configuration::EnvironmentConfiguration, instance_suffix::InstanceSuffix};

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use sqlx::{Connection as _, FromRow as _, PgConnection as Client, Row as _};
use wamn_application::application_config::ApplicationConfig;
use wamn_component_policy::PackageManifest;
use wamn_environment::{
    configuration::SequenceDefault,
    environment::{
        analyse::{Installed, ProjectState},
        document::EnvironmentDocument,
        drain::UnfinishedRun,
    },
    migrations::{MigrationFile, SchemaReport},
};
use wamn_package::{PackageName, package_schema::AppliedPackage};
use wamn_registry::configuration::ConfigurationStore as _;
use wamn_registry::platform_domain::PlatformDomain;
use wamn_registry::{
    ApplicationName, EnvName, EnvironmentRow, OrgId, ProjectEnvKey, ProjectId, Triple,
    deployment::DeploymentStore as _, name,
};
use wamn_workflow_graph::WorkflowGraph;

use crate::{LifecycleLock as PgLifecycleLock, connect};

pub(crate) async fn connect_project(url: &str) -> anyhow::Result<Client> {
    let mut client = connect(url).await?;
    let owner: String = sqlx::query(
        "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()",
    )
    .fetch_one(&mut client)
    .await?
    .get(0);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET ROLE {}",
        postgres_protocol::escape::escape_identifier(&owner)
    )))
    .execute(&mut client)
    .await
    .map(drop)?;
    Ok(client)
}

pub(crate) async fn read_row(
    system: &mut Client,
    triple: &Triple,
) -> anyhow::Result<Option<EnvironmentRow>> {
    Ok(
        sqlx::query(crate::environment_sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_READ_ROW)
            .bind(triple.org.as_str())
            .bind(triple.project.as_str())
            .bind(triple.env.as_str())
            .fetch_optional(&mut *system)
            .await
            .context("read the environment row")?
            .map(|row| {
                Ok::<_, sqlx::Error>(EnvironmentRow {
                    instance_suffix: crate::decode_text(row.try_get(0)?, 0)?,
                    disposable: row.try_get(1)?,
                })
            })
            .transpose()?,
    )
}

/// Read installed SQL as the project database owner; deploy facts live in Helm values.
/// Only an absent database returns `None`; unreadable state returns its cause.
pub async fn read_project(url: &str) -> anyhow::Result<Option<ProjectState>> {
    let mut client = match connect_project(url).await {
        Ok(client) => client,
        Err(error)
            if error
                .downcast_ref::<sqlx::Error>()
                .and_then(|error| error.as_database_error())
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref()
                == Some("3D000") =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.context("project database state is unknown")),
    };
    let mut transaction = client.begin().await.context("begin the project read")?;
    let mut installed = BTreeMap::<wamn_package::PackageName, Installed>::new();
    for row in sqlx::query("SELECT package_id, package_version FROM catalog.packages")
        .fetch_all(&mut *transaction)
        .await
        .context("read the installed packages")?
    {
        installed.insert(
            row.try_get::<String, _>(0)?
                .try_into()
                .context("decode package_id")?,
            Installed {
                current: row
                    .try_get::<String, _>(1)?
                    .try_into()
                    .context("decode package_version")?,
            },
        );
    }
    let schema_report = Some(crate::platform_migrations::observe_project(&mut transaction).await?);
    transaction.commit().await.context("end the project read")?;
    Ok(Some(ProjectState {
        installed,
        schema_report,
    }))
}

async fn application_names(
    system: &PgLifecycleLock,
    triple: &Triple,
) -> anyhow::Result<BTreeSet<ApplicationName>> {
    let mut client = system.client().await;
    let mut tx = crate::provisioning_transaction::provisioning_transaction(&mut client).await?;
    let applications: BTreeSet<wamn_registry::ApplicationName> = sqlx::query_scalar::<_, String>(
        "SELECT application FROM registry.environment_applications WHERE org=$1 AND env=$2 AND project=$3",
    )
    .bind(triple.org.as_str()).bind(triple.env.as_str()).bind(triple.project.as_str())
    .fetch_all(&mut *tx).await?.into_iter().map(|name| crate::decode_text(name, 0)).collect::<Result<_, _>>()?;
    tx.commit().await?;
    drop(client);
    Ok(applications)
}

#[derive(sqlx::FromRow)]
struct UnfinishedRunRow {
    package_id: String,
    run_id: String,
}

impl From<UnfinishedRunRow> for UnfinishedRun {
    fn from(row: UnfinishedRunRow) -> Self {
        Self {
            package_id: row.package_id,
            run_id: row.run_id,
        }
    }
}

async fn unfinished(
    client: &mut sqlx::PgConnection,
    env: &EnvName,
    application: Option<&ApplicationName>,
) -> anyhow::Result<Vec<UnfinishedRun>> {
    let mut transaction = crate::provisioning_transaction::provisioning_transaction(client).await?;
    let runs = sqlx::query(
        "SELECT package_id, run_id FROM wamn_run.runs
          WHERE environment = $1 AND ($2::text IS NULL OR package_id = $2)
            AND status IN ('dispatched', 'running')
          ORDER BY package_id, run_id",
    )
    .bind(env.as_str())
    .bind(application.map(ApplicationName::as_str))
    .try_map(|row| UnfinishedRunRow::from_row(&row).map(UnfinishedRun::from))
    .fetch_all(&mut *transaction)
    .await
    .context("read unfinished application runs")?;
    transaction.commit().await?;
    Ok(runs)
}

async fn project_count(
    db: &mut sqlx::PgConnection,
    org: &OrgId,
    env: &EnvName,
) -> anyhow::Result<u32> {
    let mut tx = crate::provisioning_transaction::provisioning_transaction(db).await?;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM registry.environment_projects WHERE org=$1 AND env=$2",
    )
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(u32::try_from(count)?)
}

/// Read protected configuration as the provisioning component.
async fn read_configuration(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<Option<wamn_registry::configuration::EnvironmentConfiguration>, sqlx::Error> {
    let principal = crate::PlatformComponent::Provisioning.principal_id();
    let mut transaction =
        crate::identity::transaction::read_connection(connection, &principal).await?;
    let configuration = crate::Registry::new(&mut transaction).read(triple).await?;
    transaction.commit().await?;
    Ok(configuration)
}

async fn host_projects(
    system: &PgLifecycleLock,
    org: &OrgId,
    env: &EnvName,
    reserved: &[&str],
    excluded: Option<&ProjectId>,
    drain_bound_seconds: Option<i32>,
) -> anyhow::Result<(Vec<ProjectId>, i32)> {
    let mut client = system.client().await;
    let mut tx = crate::provisioning_transaction::provisioning_transaction(&mut client).await?;
    let projects: Vec<ProjectId> = sqlx::query_scalar::<_, String>(
        "SELECT project FROM registry.environment_projects WHERE org=$1 AND env=$2
         AND ($3::text IS NULL OR project<>$3)
         AND (schema_mode IS NOT NULL OR project=ANY($4)) ORDER BY project",
    )
    .bind(org.as_str())
    .bind(env.as_str())
    .bind(excluded.map(wamn_registry::ProjectId::as_str))
    .bind(reserved)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|project| crate::decode_text(project, 0))
    .collect::<Result<_, _>>()?;
    let drain_bound_seconds = match drain_bound_seconds {
        Some(seconds) => seconds,
        None => crate::Registry::new(&mut tx)
            .drain_bound_seconds(org, env)
            .await
            .with_context(|| format!("read the host drain bound for {org}/{env}"))?,
    };
    tx.commit().await?;
    drop(client);
    Ok((projects, drain_bound_seconds))
}

/// Whether the environment's CDC is in place, judged by what it observes: the
/// publication `cdc_name` captures exactly `schemas` in database `project`, the
/// connection the probe runs on.
///
/// # Errors
///
/// When the probe fails.
pub async fn cdc_in_place(
    project: &mut sqlx::PgConnection,
    cdc_name: &str,
    schemas: &[wamn_package::canonical_name::CanonicalName],
) -> anyhow::Result<bool> {
    let schemas: Vec<_> = schemas
        .iter()
        .map(wamn_package::canonical_name::CanonicalName::as_str)
        .collect();
    Ok(sqlx::query("SELECT EXISTS (SELECT FROM pg_publication p WHERE p.pubname = $1 \
         AND NOT p.puballtables \
         AND coalesce((SELECT array_agg(n.nspname::text ORDER BY n.nspname) \
              FROM pg_publication_namespace pn JOIN pg_namespace n ON n.oid = pn.pnnspid \
              WHERE pn.pnpubid = p.oid), ARRAY[]::text[]) = ARRAY(SELECT DISTINCT unnest($2::text[]) ORDER BY 1))").bind(cdc_name).bind(schemas).fetch_one(&mut *project)
        .await
        .context("probe the CDC publication schema")?
        .get(0))
}

/// Wait for the chart's publication before creating the failover capture position.
async fn prepare_cdc(
    owner: &str,
    capture: &str,
    database: &str,
    publication: &str,
    schemas: &[wamn_package::canonical_name::CanonicalName],
) -> anyhow::Result<()> {
    let mut project = connect(owner).await?;
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        while !cdc_in_place(&mut project, publication, schemas).await? {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .with_context(|| format!("wait for publication {publication}"))??;
    crate::capture_sql::apply_cdc_sql(owner, capture, schemas, publication, database).await
}

/// Create each integration application principal and its declared dependency role.
/// An identical apply writes no rows.
///
/// # Errors
///
/// Returns an error if an identity is disabled or a database write fails.
pub async fn materialize_application_principals(
    domain: &PlatformDomain,
    project_url: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
    packages: &BTreeMap<wamn_package::PackageName, PackageManifest>,
) -> anyhow::Result<()> {
    use wamn_identity_protocol::PlatformComponent;

    let applications = packages
        .values()
        .filter(|package| wamn_release::publish_release::composes_integration(package))
        .collect::<Vec<_>>();
    if applications.is_empty() {
        return Ok(());
    }
    let mut project = connect_project(project_url).await?;
    let mut transaction = project.begin().await?;
    crate::operation::lock_operation_grants(&mut transaction, project_env_key).await?;
    crate::bind_platform_principal(&mut transaction, PlatformComponent::Provisioning).await?;
    for package in &applications {
        let subject = wamn_registry::resource_names::application_subject(
            project_env_key,
            &package.package.id,
        );
        crate::identity::application::service::write_service(
            &mut transaction,
            domain,
            &subject,
            &subject,
        )
        .await?;
        let role = format!(
            "application-{}",
            package.package.id.as_str().replace('_', "-")
        );
        crate::administration::create_role(&mut transaction, &role).await?;
    }
    write_runtime_grants(&mut transaction, project_env_key, &applications, packages).await?;
    transaction.commit().await?;
    Ok(())
}

/// Add the selected application's permissions while retaining grants needed by running code.
/// Principals and generated roles must already exist. Shared or authored roles refuse.
async fn write_runtime_grants(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &wamn_registry::ProjectEnvKey,
    applications: &[&PackageManifest],
    packages: &BTreeMap<wamn_package::PackageName, PackageManifest>,
) -> anyhow::Result<()> {
    let dependency_manifests = packages
        .values()
        .map(|package| (&package.package.id, package))
        .collect();
    for package in applications {
        let subject = wamn_registry::resource_names::application_subject(
            project_env_key,
            &package.package.id,
        );
        let principal =
            crate::identity::application::service::active_service(transaction, &subject)
                .await?
                .with_context(|| {
                    format!("application principal {subject} is absent or inactive")
                })?;
        let role = format!(
            "application-{}",
            package.package.id.as_str().replace('_', "-")
        );
        let owned: bool = sqlx::query(
            "SELECT EXISTS (SELECT FROM platform.role WHERE name=$1 AND created_by=$3) \
             AND NOT EXISTS (SELECT FROM platform.user_role WHERE role_name=$1 AND user_id<>$2)",
        )
        .bind(&role)
        .bind(uuid::Uuid::from(principal))
        .bind(uuid::Uuid::from(
            wamn_identity_protocol::PlatformComponent::Provisioning.principal_id(),
        ))
        .fetch_one(&mut **transaction)
        .await?
        .get(0);
        ensure!(
            owned,
            "application role {role} is missing, authored, or shared or resolve its ownership"
        );
        for permission in wamn_release::publish_release::application_dependency_permissions(
            package,
            &dependency_manifests,
        )? {
            crate::administration::mutate_grant(
                transaction,
                project_env_key,
                wamn_authentication::administration::GrantAuthority::Provisioning,
                wamn_authentication::administration::GrantChange::Permission {
                    role: &role,
                    reference: &permission,
                    grant: true,
                },
            )
            .await?;
        }
        crate::administration::mutate_grant(
            transaction,
            project_env_key,
            wamn_authentication::administration::GrantAuthority::Provisioning,
            wamn_authentication::administration::GrantChange::UserRole {
                user_id: &principal,
                role: &role,
                grant: true,
            },
        )
        .await?;
    }
    Ok(())
}

/// Create the `platform.number_sequence` row of each sequence default that has none.
/// A row that exists keeps its pattern (`docs/architecture/data-access.md#numbering`).
async fn create_number_sequences(
    project: &mut sqlx::PgConnection,
    sequences: &[SequenceDefault],
) -> anyhow::Result<()> {
    let mut transaction = sqlx::Connection::begin(project).await?;
    crate::bind_platform_principal(
        &mut transaction,
        wamn_identity_protocol::PlatformComponent::Provisioning,
    )
    .await?;
    for sequence in sequences {
        sqlx::query(
            "INSERT INTO platform.number_sequence (application, name, pattern) \
             VALUES ($1, $2, $3) ON CONFLICT (application, name) DO NOTHING",
        )
        .bind(sequence.application.as_str())
        .bind(&sequence.name)
        .bind(&sequence.pattern)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("create number sequence {}", sequence.name))?;
    }
    transaction.commit().await?;
    Ok(())
}

async fn check_workflows(
    project: &mut sqlx::PgConnection,
    app: &ApplicationConfig,
) -> anyhow::Result<()> {
    let graphs = sqlx::query_as::<_, (String, Value)>(
        "SELECT workflow_id, graph_json FROM catalog.workflows
         WHERE environment=$1 AND application=$2 AND enabled ORDER BY workflow_id",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .fetch_all(project)
    .await?;
    for (name, value) in graphs {
        let graph = WorkflowGraph::parse(&value)?;
        wamn_application::application_config::check_workflow_references(app, &graph)
            .with_context(|| format!("enabled workflow {name}"))?;
    }
    Ok(())
}

/// Delete migration-owned objects and application state. Callers stop users first.
pub async fn reset_database(
    system: &PgLifecycleLock,
    database_url: &str,
    migration_url: &str,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<()> {
    let disposable = crate::Registry::new(&mut *system.client().await)
        .disposable(triple)
        .await?;
    ensure!(disposable, "environment is not recorded as disposable");
    let mut client = crate::connect(database_url).await?;
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut client)
        .await?;
    let mut tx = client.begin().await?;
    sqlx::raw_sql(crate::apply_package::LOCK_SQL)
        .execute(&mut *tx)
        .await?;
    crate::bind_platform_principal(&mut tx, crate::PlatformComponent::Provisioning).await?;
    let mut migration = crate::connect(migration_url).await?;
    let login = crate::project_migration::require_login(&mut migration).await?;
    let migration_database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut migration)
        .await?;
    ensure!(
        migration_database == database,
        "migration and owner connections name different databases"
    );
    let schemas: Vec<String> = sqlx::query_scalar(
        "SELECT nspname::text FROM pg_namespace WHERE nspowner = (SELECT datdba FROM pg_database WHERE datname = current_database()) AND nspname NOT LIKE 'pg_%' AND nspname NOT IN ('public','information_schema','platform','wamn_authority','wamn_history','catalog','wamn_run') AND NOT EXISTS (SELECT FROM pg_depend WHERE classid='pg_namespace'::regclass AND objid=pg_namespace.oid AND deptype='e')",
    )
    .fetch_all(&mut *tx)
    .await?;
    let functions: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT p.oid::regprocedure::text FROM pg_trigger t \
         JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace \
         JOIN pg_proc p ON p.oid=t.tgfoid \
         WHERE n.nspname=ANY($1) AND p.pronamespace='wamn_history'::regnamespace \
           AND starts_with(p.proname::text,n.nspname::text||'__')",
    )
    .bind(&schemas)
    .fetch_all(&mut *tx)
    .await?;
    for schema in schemas {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            postgres_protocol::escape::escape_identifier(&schema)
        )))
        .execute(&mut *tx)
        .await?;
    }
    for function in functions {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP FUNCTION {function}")))
            .execute(&mut *tx)
            .await?;
    }
    // SQLx histories are trusted platform objects, separate from application DDL ownership.
    for (table,) in sqlx::query_as::<_, (String,)>("SELECT quote_ident(n.nspname)||'.'||quote_ident(c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='wamn_history' AND c.relname LIKE 'migrations_%' AND c.relkind='r'").fetch_all(&mut *tx).await? {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TABLE {table}"))).execute(&mut *tx).await?;
    }
    let tables: Vec<String> = sqlx::query_scalar("SELECT quote_ident(n.nspname)||'.'||quote_ident(c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('catalog','wamn_run') AND c.relkind='r'")
        .fetch_all(&mut *tx).await?;
    if !tables.is_empty() {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "TRUNCATE {} CASCADE",
            tables.join(", ")
        )))
        .execute(&mut *tx)
        .await?;
    }
    sqlx::raw_sql("TRUNCATE platform.write_log")
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{} {}",
        crate::project_setup::grant_workload_login_sql(
            wamn_platform_install::workload_role::WorkloadRoleFamily::Migration,
            &database,
            &login,
        ),
        crate::project_migration::grants_sql(&login)
    )))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn package_baselines(url: &str) -> anyhow::Result<BTreeMap<PackageName, AppliedPackage>> {
    let mut client = connect_project(url).await?;
    let mut packages = BTreeMap::new();
    for row in
        sqlx::query("SELECT package_id, package_version, artifact_digest FROM catalog.packages")
            .fetch_all(&mut client)
            .await?
    {
        let id = wamn_package::PackageName::try_from(row.try_get::<String, _>(0)?)
            .context("decode package_id")?;
        packages.insert(
            id.clone(),
            AppliedPackage {
                coordinate: wamn_package::PackageCoordinate::new(
                    id,
                    row.try_get::<String, _>(1)?
                        .try_into()
                        .context("decode package_version")?,
                )?,
                artifact_digest: row
                    .try_get::<Option<&str>, _>(2)?
                    .map(str::parse)
                    .transpose()?,
            },
        );
    }
    Ok(packages)
}

struct MigrationReader(Option<Client>);

#[async_trait::async_trait]
impl PackageMigrations for MigrationReader {
    async fn observe(
        &mut self,
        path: &Path,
        key: &ProjectEnvKey,
        id: &PackageName,
    ) -> anyhow::Result<SchemaReport> {
        let migrator = crate::build_database::migrator(path).await?;
        let migrations = if let Some(client) = &mut self.0 {
            crate::migrations::observe(
                client,
                &crate::project_migration::history_table(key, id),
                &migrator.migrations,
            )
            .await?
        } else {
            SchemaReport {
                pending: migrator
                    .iter()
                    .filter(|source| source.migration_type.is_up_migration())
                    .map(|source| MigrationFile {
                        version: source.version,
                        checksum: hex::encode(&source.checksum),
                    })
                    .collect(),
                ..Default::default()
            }
        };
        Ok(migrations)
    }
}

/// Remove the environment's registry rows last.
/// The caller holds the lifecycle lock and removes external resources first.
///
/// # Errors
///
/// Returns an error if a database operation or row removal fails.
pub async fn delete_registry_rows(
    system: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let mut transaction = crate::provisioning_transaction::provisioning_transaction(system)
        .await
        .context("begin the row delete")?;
    sqlx::query(
        crate::environment_sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_ROWS_DELETE_REGISTRY_ROWS,
    )
    .bind(org.as_str())
    .bind(project.as_str())
    .bind(env.as_str())
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .context("delete the registry.environment_projects row")?;
    transaction
        .commit()
        .await
        .context("commit the row delete")?;
    Ok(vec![format!("deleted the environment row of {triple}")])
}

async fn reserve(
    system: &PgLifecycleLock,
    projects: &[EnvironmentDocument],
    disposable: Option<bool>,
) -> anyhow::Result<Vec<InstanceSuffix>> {
    let first = projects
        .first()
        .context("environment has no selected projects")?;
    let project_names: Vec<_> = projects
        .iter()
        .map(|project| project.settings.project.as_str())
        .collect();
    let mut reserved = Vec::with_capacity(projects.len());
    let mut client = system.client().await;
    let mut transaction = provision_project_env::provisioning_transaction(&mut client).await?;
    sqlx::query(crate::platform_sql::PROJECT_STREAM_CAPACITY_LOCK_SQL)
        .execute(&mut *transaction)
        .await
        .context("lock platform project stream capacity")?;
    let (max_projects, additional): (Option<i64>, i32) = sqlx::query_as(
        "SELECT (SELECT max_projects FROM registry.meta), \
                            count(*)::int \
                     FROM unnest($3::text[]) AS selected(project) \
                     WHERE NOT EXISTS (SELECT 1 FROM registry.environment_projects AS registered \
                         WHERE registered.org = $1 AND registered.env = $2 \
                         AND registered.project = selected.project)",
    )
    .bind(first.settings.org.as_str())
    .bind(first.settings.env.as_str())
    .bind(&project_names)
    .fetch_one(&mut *transaction)
    .await
    .context("count project stream reservations")?;
    let max_projects =
        max_projects.context("the system SQL Job has not set the project stream limit")?;
    if additional > 0 {
        let available: bool = sqlx::query_scalar("SELECT registry.project_capacity_available($1)")
            .bind(additional)
            .fetch_one(&mut *transaction)
            .await
            .context("check platform project stream capacity")?;
        ensure!(
            available,
            "project stream limit is {max_projects}, count would exceed it ({additional} new)"
        );
    }
    for project in projects {
        reserved.push(
            reserve_project(&mut transaction, project, disposable)
                .await
                .with_context(|| format!("reserve project {}", project.settings.project))?,
        );
    }
    transaction
        .commit()
        .await
        .context("commit project reservations")?;
    Ok(reserved)
}

async fn reserve_project(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    document: &EnvironmentDocument,
    disposable: Option<bool>,
) -> anyhow::Result<wamn_registry::instance_suffix::InstanceSuffix> {
    wamn_platform_install::environment::validate_project_env_cdc(
        &document.settings.org,
        &document.settings.project,
        &document.settings.env,
    )?;
    let instance = wamn_environment::provision_project_env::new_instance_suffix()?;
    crate::environment_registry::record_project_env(
        transaction,
        &document.triple(),
        document.readiness_budget_seconds,
        document.drain_bound_seconds,
        &document.settings.target,
        &name::project_env_secret_name(
            &document.settings.org,
            &document.settings.project,
            &document.settings.env,
        ),
        Some(document.settings.org.as_str()),
        &instance,
        disposable,
    )
    .await
}

async fn initialize_project(
    project: &PgLifecycleLock,
    database: &str,
    owner: &str,
    unconfigured: bool,
) -> anyhow::Result<()> {
    let mut client = project.client().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(crate::project_setup::privilege_sql(
        database,
    )))
    .execute(&mut *client)
    .await?;
    drop(client);
    if unconfigured {
        crate::platform_migrations::install_project(&mut *project.client().await).await?;
        crate::environment_schema::restore_project(
            owner,
            &crate::platform_migrations::embedded_sources(
                crate::migration_scope::MigrationDatabase::Project,
            ),
        )
        .await?;
    }
    Ok(())
}

async fn save_deployment(
    system: &PgLifecycleLock,
    triple: &Triple,
    configuration: &EnvironmentConfiguration,
    publication: &str,
    schemas: &[CanonicalName],
    capture: &ConnectionConfig,
) -> anyhow::Result<()> {
    let mut connection = system.client().await;
    let mut tx = provision_project_env::provisioning_transaction(&mut connection).await?;
    ensure!(
        crate::Registry::new(&mut tx).write(configuration).await?,
        "provision the environment before applying configuration"
    );
    // With deployed schemas, the failover capture position uses the publication name.
    let capture_name = publication;
    let stream = wamn_event::stream_name(&triple.org, &triple.project, &triple.env);
    sqlx::query(sqlx::AssertSqlSafe(upsert_event_reader_sql()))
        .bind(triple.org.as_str())
        .bind(triple.project.as_str())
        .bind(triple.env.as_str())
        .bind(publication)
        .bind(
            schemas
                .iter()
                .map(wamn_package::canonical_name::CanonicalName::as_str)
                .collect::<Vec<_>>(),
        )
        .bind(capture_name)
        .bind(stream)
        .bind(&capture.credentials.name)
        .bind(&capture.credentials.namespace)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("register the CDC reader for {triple}"))?;
    tx.commit().await?;
    Ok(())
}

async fn delete_inputs(
    system: &PgLifecycleLock,
    triple: &Triple,
) -> anyhow::Result<Option<(InstanceSuffix, i32)>> {
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let row = sqlx::query_as::<_, (String, i32)>(
        crate::environment_sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_DELETE_LOCKED,
    )
    .bind(org.as_str())
    .bind(project.as_str())
    .bind(env.as_str())
    .fetch_optional(&mut *system.client().await)
    .await
    .context("read the environment row")?;
    row.map(|(instance, drain)| {
        Ok((
            crate::decode_text(instance, 0).context("read the environment row")?,
            drain,
        ))
    })
    .transpose()
}

async fn capture_exists(
    owner: &ConnectionConfig,
    capture_name: &str,
    data: bool,
) -> anyhow::Result<bool> {
    let observer = crate::database_configuration(&owner.to_json()?, "postgres")?;
    let mut observer = connect_for_delete(connect(&observer), data).await?;
    if let Some(row) = sqlx::query("SELECT active FROM pg_replication_slots WHERE slot_name=$1")
        .bind(capture_name)
        .fetch_optional(&mut observer)
        .await?
    {
        if row.get::<bool, _>(0) {
            bail!("replication slot is active");
        }
        Ok(true)
    } else {
        Ok(false)
    }
}
async fn drop_capture(
    capture: &ConnectionConfig,
    capture_name: &str,
    data: bool,
) -> anyhow::Result<()> {
    let mut connection = connect_for_delete(connect(&capture.to_json()?), data).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(crate::capture_sql::drop_capture_sql(
        capture_name,
    )))
    .execute(&mut connection)
    .await?;
    Ok(())
}

async fn disable_connection_principals(project: &PgLifecycleLock) -> anyhow::Result<()> {
    let mut project = project.client().await;
    let mut transaction = project.begin().await?;
    crate::bind_platform_principal(&mut transaction, crate::PlatformComponent::Provisioning)
        .await?;
    crate::identity::application::disable_connection_services(&mut transaction)
        .await
        .context("disable the retained database's connection principals")?;
    transaction.commit().await?;
    Ok(())
}

async fn connect_for_delete(
    connection: impl Future<Output = anyhow::Result<sqlx::PgConnection>>,
    data: bool,
) -> anyhow::Result<sqlx::PgConnection> {
    if data {
        tokio::time::timeout(Duration::from_secs(10), connection)
            .await
            .context("connect to project database")?
    } else {
        connection.await
    }
}

fn database_unreachable(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::Io(_)) => true,
        Some(sqlx::Error::Database(error)) => error.code().as_deref() == Some("3D000"),
        _ => error.is::<tokio::time::error::Elapsed>(),
    }
}

/// Whether the project has any capture gap that still needs resynchronization.
async fn capture_gap_open(
    client: &mut sqlx::PgConnection,
    triple: &Triple,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT FROM registry.capture_gap
         WHERE org = $1 AND project = $2 AND env = $3 AND resync_at IS NULL)",
    )
    .bind(triple.org.as_str())
    .bind(triple.project.as_str())
    .bind(triple.env.as_str())
    .fetch_one(client)
    .await?)
}

async fn capture_registration(
    system: &PgLifecycleLock,
    triple: &Triple,
) -> anyhow::Result<CaptureRegistration> {
    let env = &triple.env;
    let registration = sqlx::query(sqlx::AssertSqlSafe(
        (select_event_reader_origin_sql()).to_string(),
    ))
    .bind(triple.org.as_str())
    .bind(triple.project.as_str())
    .bind(env.as_str())
    .fetch_optional(&mut *system.client().await)
    .await
    .context("read the registry.event_readers row")?;
    let Some(registration) = registration else {
        bail!("{triple} has no CDC reader registration");
    };
    let registered_name: String = registration.get(0);
    let stream: String = registration.get(1);
    let registered_at: DateTime<Utc> = registration.get(2);
    Ok(CaptureRegistration {
        name: registered_name,
        stream,
        created_at: registered_at,
    })
}

async fn inspect_capture(
    url: &str,
    database: &str,
    replication_name: &str,
    triple: &Triple,
) -> anyhow::Result<(CaptureStatus, Box<dyn CaptureRepair>)> {
    let mut admin = crate::connect(url)
        .await
        .context("connect to the project database for capture gap recovery")?;
    let current: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await
        .context("read the database of --admin-database-url")?
        .get(0);
    ensure!(
        current == database,
        "--admin-database-url reaches database {current}, not the project-env database {database}"
    );
    let state = sqlx::query(
        "SELECT active, confirmed_flush_lsn::text, wal_status::text, \
                    invalidation_reason::text \
             FROM pg_replication_slots WHERE slot_name = $1",
    )
    .bind(replication_name)
    .fetch_optional(&mut admin)
    .await
    .context("read the slot")?;
    let (reason, confirmed) = match state {
        None => (MISSING_CAPTURE_REASON.to_owned(), None),
        Some(row) => {
            let active: bool = row.get(0);
            let confirmed: Option<String> = row.get(1);
            let wal_status: Option<String> = row.get(2);
            let invalidation: Option<String> = row.get(3);
            if active {
                bail!("slot {replication_name} is active: a reader still streams from it");
            }
            if invalidation.is_none() && wal_status.as_deref() != Some("lost") {
                bail!("slot {replication_name} is healthy: {triple} has no capture gap");
            }
            (
                invalidation.unwrap_or_else(|| "wal_status lost".to_owned()),
                confirmed,
            )
        }
    };

    Ok((
        CaptureStatus { reason, confirmed },
        Box::new(RepairConnection(admin)),
    ))
}

struct RepairConnection(Client);

#[async_trait::async_trait]
impl CaptureRepair for RepairConnection {
    async fn repair(&mut self, replication_name: &str) -> anyhow::Result<String> {
        sqlx::raw_sql(sqlx::AssertSqlSafe(crate::capture_sql::drop_capture_sql(
            replication_name,
        )))
        .execute(&mut self.0)
        .await
        .map(drop)
        .context("drop the lost slot")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            crate::capture_sql::create_failover_capture_sql(replication_name),
        ))
        .execute(&mut self.0)
        .await
        .map(drop)
        .context("create the slot again")?;
        let end_lsn: String = sqlx::query(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
        )
        .bind(replication_name)
        .fetch_one(&mut self.0)
        .await
        .context("read the new slot's first position")?
        .get(0);

        Ok(end_lsn)
    }
}

async fn record_capture_gap(
    system: &PgLifecycleLock,
    gap: &PendingCaptureGap<'_>,
) -> anyhow::Result<DateTime<Utc>> {
    let triple = gap.triple;
    let env = &triple.env;
    let mut client = system.client().await;
    let mut transaction = capture_transaction(&mut client).await?;
    let created_at: DateTime<Utc> =
        sqlx::query(sqlx::AssertSqlSafe((insert_capture_gap_sql()).to_string()))
            .bind(triple.org.as_str())
            .bind(triple.project.as_str())
            .bind(env.as_str())
            .bind(gap.name)
            .bind(gap.start_lsn)
            .bind(gap.start_at)
            .bind(gap.reason)
            .bind(gap.end_lsn)
            .fetch_one(&mut *transaction)
            .await
            .context("write the registry.capture_gap row")?
            .get(0);

    transaction.commit().await?;

    Ok(created_at)
}

/// Close the newest gap, retaining its original resynchronization time on retry.
async fn close_capture_gap(
    triple: &Triple,
    system: &mut Client,
) -> anyhow::Result<ClosedCaptureGap> {
    let env = &triple.env;
    let mut transaction = capture_transaction(system).await?;
    let newest = sqlx::query(sqlx::AssertSqlSafe(
        (lock_newest_capture_gap_sql()).to_string(),
    ))
    .bind(triple.org.as_str())
    .bind(triple.project.as_str())
    .bind(env.as_str())
    .fetch_optional(&mut *transaction)
    .await
    .context("read the newest registry.capture_gap row")?;
    let Some(newest) = newest else {
        bail!("{triple} has no capture gap row");
    };
    let created_at: DateTime<Utc> = newest.get(0);
    let replication_name: String = newest.get(1);
    let resync_at = match newest.get::<Option<DateTime<Utc>>, _>(2) {
        Some(recorded) => recorded,
        None => sqlx::query(sqlx::AssertSqlSafe(close_capture_gap_sql().to_string()))
            .bind(triple.org.as_str())
            .bind(triple.project.as_str())
            .bind(env.as_str())
            .bind(created_at)
            .fetch_one(&mut *transaction)
            .await
            .context("set resync_at")?
            .get(0),
    };
    transaction
        .commit()
        .await
        .context("commit the close transaction")?;
    Ok(ClosedCaptureGap {
        triple: triple.clone(),
        replication_name,
        created_at,
        resync_at,
    })
}

async fn capture_transaction(
    client: &mut Client,
) -> anyhow::Result<sqlx::Transaction<'_, sqlx::Postgres>> {
    use sqlx::Connection as _;
    let mut transaction = client.begin().await?;
    crate::bind_platform_principal(&mut transaction, crate::PlatformComponent::Provisioning)
        .await?;
    Ok(transaction)
}

async fn environment_target(
    system: &PgLifecycleLock,
    triple: &Triple,
) -> anyhow::Result<Option<Option<String>>> {
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT target_id::text FROM registry.environments WHERE org=$1 AND env=$2",
    )
    .bind(triple.org.as_str())
    .bind(triple.env.as_str())
    .fetch_optional(&mut *system.client().await)
    .await?)
}

async fn connection_binding(
    project: &PgLifecycleLock,
    name: &str,
) -> anyhow::Result<Option<ConnectionBinding>> {
    let connection=sqlx::query("SELECT c.id::text,c.integration_type,g.parameters,c.active_generation,c.target_id::text,c.authority FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) WHERE c.name=$1 AND c.status='active'")
            .bind(name).fetch_optional(&mut *project.client().await).await?;
    connection
        .map(|row| {
            Ok(ConnectionBinding {
                id: row.try_get(0)?,
                integration_type: row.try_get(1)?,
                parameters: row.try_get(2)?,
                generation: row.try_get(3)?,
                target: row.try_get(4)?,
                authority: row.try_get(5)?,
            })
        })
        .transpose()
}

async fn integration_binding(
    system: &PgLifecycleLock,
    triple: &Triple,
    target: &str,
    version: i32,
    connection: &ConnectionBinding,
) -> anyhow::Result<Option<IntegrationBinding>> {
    let row=sqlx::query("SELECT $1::text AS id,t.code,t.runtime_type,$3::jsonb AS parameters,v.parameter_schema,v.digest,v.operations,$6::bigint AS generation,t.exclusive,v.exports,v.write_interfaces,destination.type FROM registry.integration_type t JOIN registry.target destination ON destination.id=$4::text::uuid AND destination.name=$5 AND destination.status='active' JOIN registry.environments e ON e.org=$7 AND e.env=$8 LEFT JOIN registry.integration_type_version v ON v.integration_type=t.code AND v.version=$9 WHERE t.code=$2")
            .bind(&connection.id).bind(&connection.integration_type).bind(&connection.parameters).bind(&connection.target).bind(target).bind(connection.generation).bind(triple.org.as_str()).bind(triple.env.as_str()).bind(version)
            .fetch_optional(&mut *system.client().await).await?;
    row.map(|row| {
        Ok(IntegrationBinding {
            integration_type: row.try_get(1)?,
            runtime: row.try_get(2)?,
            schema: row.try_get(4)?,
            digest: row.try_get(5)?,
            exclusive: row.try_get(8)?,
            exports: row.try_get(9)?,
            write_interfaces: row.try_get(10)?,
            placement: row.try_get(11)?,
        })
    })
    .transpose()
}

use wamn_digest::Digest;
use wamn_environment::apply_package::{ApplyOutcome, ApplyPackageRequest};
use wamn_environment::environment::database::{
    Database, DatabaseConnection, DatabaseSession, LifecycleLock, PackageMigrations,
};
use wamn_package::package_schema::PackageDirectory;
use wamn_platform_install::workload_role::WorkloadRoleFamily;
use wamn_registry::lifecycle_lock::LifecycleLocks as _;

/// Postgres environment deployment adapter.
#[derive(Debug)]
pub struct PostgresDeployment;

#[async_trait::async_trait]
impl Database for PostgresDeployment {
    async fn lock(
        &self,
        connection: DatabaseConnection<'_>,
        org: &OrgId,
        env: &EnvName,
    ) -> anyhow::Result<LifecycleLock> {
        let lock = match connection {
            DatabaseConnection::Direct(input) => {
                PgLifecycleLock::acquire_environment(connect(input).await?, org, env).await?
            }
            DatabaseConnection::WaitForEnvironment { input, triple } => {
                PgLifecycleLock::acquire_wait(connect(input).await?, triple).await?
            }
            DatabaseConnection::Wait { input, database } => {
                let client =
                    crate::platform_database::wait_project_connection(input, database).await?;
                PgLifecycleLock::acquire_environment(client, org, env).await?
            }
            DatabaseConnection::Delete { input, data } => {
                let client = connect_for_delete(connect_project(input), data).await?;
                PgLifecycleLock::acquire_environment(client, org, env).await?
            }
        };
        let lost = lock.loss_signal();
        Ok(LifecycleLock {
            session: Box::new(lock),
            lost,
        })
    }

    fn unreachable(&self, error: &anyhow::Error) -> bool {
        database_unreachable(error)
    }

    async fn read_configuration(
        &self,
        input: &str,
        triple: &Triple,
    ) -> anyhow::Result<Option<EnvironmentConfiguration>> {
        Ok(read_configuration(&mut connect(input).await?, triple).await?)
    }

    async fn environment_row(
        &self,
        input: &str,
        triple: &Triple,
    ) -> anyhow::Result<Option<EnvironmentRow>> {
        let mut connection = connect(input).await?;
        let mut transaction =
            provision_project_env::provisioning_transaction(&mut connection).await?;
        let row = read_row(&mut transaction, triple).await?;
        if row.is_some() {
            transaction.commit().await?;
        }
        Ok(row)
    }

    async fn read_project(&self, input: &str) -> anyhow::Result<Option<ProjectState>> {
        read_project(input).await
    }

    async fn unfinished(
        &self,
        url: &str,
        database: &str,
        env: &EnvName,
        app: Option<&ApplicationName>,
    ) -> anyhow::Result<Vec<UnfinishedRun>> {
        let mut connection =
            crate::platform_database::wait_project_connection(url, database).await?;
        unfinished(&mut connection, env, app).await
    }

    async fn installed_packages(
        &self,
        input: &str,
    ) -> anyhow::Result<Vec<(PackageName, Option<Digest>)>> {
        Ok(crate::installed_package_artifacts(&mut connect(input).await?).await?)
    }

    async fn package_baselines(
        &self,
        input: &str,
    ) -> anyhow::Result<BTreeMap<PackageName, AppliedPackage>> {
        package_baselines(input).await
    }

    async fn package_migrations(
        &self,
        input: Option<&str>,
    ) -> anyhow::Result<Box<dyn PackageMigrations>> {
        let connection = match input {
            Some(input) => Some(connect_project(input).await?),
            None => None,
        };
        Ok(Box::new(MigrationReader(connection)))
    }

    async fn apply_package(
        &self,
        request: ApplyPackageRequest,
        directory: PackageDirectory,
        path: &Path,
    ) -> anyhow::Result<ApplyOutcome> {
        crate::apply_package::apply_package(request, directory, path).await
    }

    async fn materialize_application_principals(
        &self,
        domain: &PlatformDomain,
        url: &str,
        key: &ProjectEnvKey,
        packages: &BTreeMap<PackageName, PackageManifest>,
    ) -> anyhow::Result<()> {
        materialize_application_principals(domain, url, key, packages).await
    }

    fn migration_configuration(&self, owner: &str) -> anyhow::Result<String> {
        crate::migration_scope::MigrationDatabase::Project.configuration(owner)
    }

    async fn prepare_cdc(
        &self,
        owner: &str,
        capture: &str,
        database: &str,
        publication: &str,
        schemas: &[CanonicalName],
    ) -> anyhow::Result<()> {
        prepare_cdc(owner, capture, database, publication, schemas).await
    }

    async fn capture_exists(
        &self,
        owner: &ConnectionConfig,
        name: &str,
        data: bool,
    ) -> anyhow::Result<bool> {
        capture_exists(owner, name, data).await
    }

    async fn drop_capture(
        &self,
        capture: &ConnectionConfig,
        name: &str,
        data: bool,
    ) -> anyhow::Result<()> {
        drop_capture(capture, name, data).await
    }

    async fn inspect_capture(
        &self,
        url: &str,
        database: &str,
        name: &str,
        triple: &Triple,
    ) -> anyhow::Result<(CaptureStatus, Box<dyn CaptureRepair>)> {
        inspect_capture(url, database, name, triple).await
    }
}

#[async_trait::async_trait]
impl DatabaseSession for PgLifecycleLock {
    async fn release(self: Box<Self>) {
        (*self).release().await;
    }

    async fn reserve(
        &self,
        projects: &[EnvironmentDocument],
        disposable: Option<bool>,
    ) -> anyhow::Result<Vec<InstanceSuffix>> {
        reserve(self, projects, disposable).await
    }

    async fn retain_integrations(
        &self,
        org: &OrgId,
        env: &EnvName,
        names: &[&str],
    ) -> anyhow::Result<()> {
        let mut connection = self.client().await;
        let mut transaction =
            provision_project_env::provisioning_transaction(&mut connection).await?;
        crate::Registry::new(&mut transaction)
            .retain_integrations(org, env, names)
            .await
            .context("remove connection pins after project configuration")?;
        transaction.commit().await?;
        Ok(())
    }

    async fn project_count(&self, org: &OrgId, env: &EnvName) -> anyhow::Result<u32> {
        project_count(&mut *self.client().await, org, env).await
    }

    async fn host_projects(
        &self,
        org: &OrgId,
        env: &EnvName,
        reserved: &[&str],
        excluded: Option<&ProjectId>,
        drain: Option<i32>,
    ) -> anyhow::Result<(Vec<ProjectId>, i32)> {
        host_projects(self, org, env, reserved, excluded, drain).await
    }

    async fn connection(&self, triple: &Triple) -> anyhow::Result<ConnectionConfig> {
        crate::platform_database::connection(&mut *self.client().await, triple).await
    }

    async fn project_schema(
        &self,
        triple: &Triple,
        migration: &str,
        apply: bool,
        output: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        let sources = crate::platform_migrations::embedded_sources(
            crate::migration_scope::MigrationDatabase::Project,
        );
        crate::environment_schema::project_schema(self, triple, migration, &sources, apply, output)
            .await
    }

    async fn read_configuration(
        &self,
        triple: &Triple,
    ) -> anyhow::Result<Option<EnvironmentConfiguration>> {
        Ok(read_configuration(&mut *self.client().await, triple).await?)
    }

    async fn fill(&self, owner: &str, triple: &Triple, issuer: &str) -> anyhow::Result<()> {
        let mut project = connect(owner).await?;
        crate::environment_fill::fill(
            &mut *self.client().await,
            &mut project,
            issuer,
            wamn_authentication::administration::ApplicationTarget {
                org: &triple.org,
                project: &triple.project,
                env: &triple.env,
            },
        )
        .await
    }

    async fn save_deployment(
        &self,
        triple: &Triple,
        configuration: &EnvironmentConfiguration,
        publication: &str,
        schemas: &[CanonicalName],
        capture: &ConnectionConfig,
    ) -> anyhow::Result<()> {
        save_deployment(self, triple, configuration, publication, schemas, capture).await
    }

    async fn environment_row(&self, triple: &Triple) -> anyhow::Result<Option<EnvironmentRow>> {
        let mut connection = self.client().await;
        let mut transaction =
            provision_project_env::provisioning_transaction(&mut connection).await?;
        let row = read_row(&mut transaction, triple)
            .await?
            .context("the environment row is missing after step 4")?;
        transaction.commit().await?;
        Ok(Some(row))
    }

    async fn application_names(
        &self,
        triple: &Triple,
    ) -> anyhow::Result<BTreeSet<ApplicationName>> {
        application_names(self, triple).await
    }

    async fn capture_instance(&self, triple: &Triple) -> anyhow::Result<InstanceSuffix> {
        crate::environment_registry::read_project_env_instance(&mut *self.client().await, triple)
            .await
    }

    async fn instance(&self, triple: &Triple) -> anyhow::Result<InstanceSuffix> {
        crate::Registry::new(&mut *self.client().await)
            .instance_suffix(triple)
            .await?
            .context("project deployment instance is absent")
    }

    async fn disposable(&self, triple: &Triple) -> anyhow::Result<bool> {
        Ok(crate::Registry::new(&mut *self.client().await)
            .disposable(triple)
            .await?)
    }

    async fn delete_inputs(
        &self,
        triple: &Triple,
    ) -> anyhow::Result<Option<(InstanceSuffix, i32)>> {
        delete_inputs(self, triple).await
    }

    async fn delete_rows(&self, triple: &Triple) -> anyhow::Result<Vec<String>> {
        delete_registry_rows(&mut *self.client().await, triple).await
    }

    async fn reset(&self, owner: &str, migration: &str, triple: &Triple) -> anyhow::Result<()> {
        reset_database(self, owner, migration, triple).await
    }

    async fn initialize_project(
        &self,
        database: &str,
        owner: &str,
        unconfigured: bool,
    ) -> anyhow::Result<()> {
        initialize_project(self, database, owner, unconfigured).await
    }

    async fn grant_workload(
        &self,
        connections: &[(WorkloadRoleFamily, ConnectionConfig)],
    ) -> anyhow::Result<()> {
        crate::project_setup::grant(&mut *self.client().await, connections).await
    }

    async fn unfinished(
        &self,
        env: &EnvName,
        app: Option<&ApplicationName>,
    ) -> anyhow::Result<Vec<UnfinishedRun>> {
        unfinished(&mut *self.client().await, env, app).await
    }

    async fn installed_packages(&self) -> anyhow::Result<Vec<(PackageName, Option<Digest>)>> {
        Ok(crate::installed_package_artifacts(&mut *self.client().await).await?)
    }

    async fn disable_connection_principals(&self) -> anyhow::Result<()> {
        disable_connection_principals(self).await
    }

    async fn platform_domain(&self) -> anyhow::Result<PlatformDomain> {
        Ok(
            crate::identity::application::service::platform_domain(&mut *self.client().await)
                .await?,
        )
    }

    async fn check_workflows(&self, application: &ApplicationConfig) -> anyhow::Result<()> {
        check_workflows(&mut *self.client().await, application).await
    }

    async fn create_number_sequences(&self, sequences: &[SequenceDefault]) -> anyhow::Result<()> {
        create_number_sequences(&mut *self.client().await, sequences).await
    }

    async fn environment_target(&self, triple: &Triple) -> anyhow::Result<Option<Option<String>>> {
        environment_target(self, triple).await
    }

    async fn connection_binding(&self, name: &str) -> anyhow::Result<Option<ConnectionBinding>> {
        connection_binding(self, name).await
    }

    async fn integration_binding(
        &self,
        triple: &Triple,
        target: &str,
        version: i32,
        connection: &ConnectionBinding,
    ) -> anyhow::Result<Option<IntegrationBinding>> {
        integration_binding(self, triple, target, version, connection).await
    }

    async fn has_capture_gap(&self, triple: &Triple) -> anyhow::Result<bool> {
        capture_gap_open(&mut *self.client().await, triple).await
    }

    async fn capture_registration(&self, triple: &Triple) -> anyhow::Result<CaptureRegistration> {
        capture_registration(self, triple).await
    }

    async fn record_capture_gap(
        &self,
        gap: &PendingCaptureGap<'_>,
    ) -> anyhow::Result<DateTime<Utc>> {
        record_capture_gap(self, gap).await
    }

    async fn close_capture_gap(&self, triple: &Triple) -> anyhow::Result<ClosedCaptureGap> {
        close_capture_gap(triple, &mut *self.client().await).await
    }
}
