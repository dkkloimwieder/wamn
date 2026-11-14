//! Reset application objects while retaining the provisioned database and CDC.

use super::super::{Platform, analyse, document::EnvironmentDocument, ensure};
use super::schema::{self, Candidate, SchemaSource};
use anyhow::{Context as _, ensure};
use serde_json::Value;
use sqlx::Connection as _;

/// Each reset phase uses its own role's credential in the same worker process.
pub struct Credentials {
    pub owner: String,
    pub migration: String,
    pub capture: String,
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").finish_non_exhaustive()
    }
}

/// Delete migration-owned objects and application state. Callers stop users first.
pub async fn reset_database(
    system_url: &str,
    database_url: &str,
    migration_url: &str,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<()> {
    let mut system = wamn_postgres::connect(system_url).await?;
    let disposable: bool = sqlx::query_scalar("SELECT disposable FROM registry.environment_projects JOIN registry.environments USING (org,env) WHERE org=$1 AND project=$2 AND env=$3")
        .bind(&triple.org).bind(&triple.project).bind(triple.env.as_str()).fetch_one(&mut system).await?;
    ensure!(disposable, "environment is not recorded as disposable");
    let project_env_key =
        wamn_registry::project_env_key(&triple.org, &triple.project, triple.env.as_str());
    let mut client = wamn_postgres::connect(database_url).await?;
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut client)
        .await?;
    let login = wamn_control_provision::migration::login(&project_env_key, &database)?;
    let mut tx = client.begin().await?;
    sqlx::raw_sql(crate::apply_package::LOCK_SQL)
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *tx)
    .await?;
    let mut migration = wamn_postgres::connect(migration_url).await?;
    wamn_control_provision::migration::require_login(&mut migration, &project_env_key).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY {}",
        postgres_protocol::escape::escape_identifier(&login)
    )))
    .execute(&mut migration)
    .await?;
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
    let role = postgres_protocol::escape::escape_identifier(&login);
    let database = postgres_protocol::escape::escape_identifier(&database);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT CONNECT ON DATABASE {database} TO {role}; {}",
        wamn_control_provision::migration::grants_sql(&login)
    )))
    .execute(&mut *tx)
    .await?;
    // Application schemas belong to the platform, so schema publications survive.
    for (schema,) in sqlx::query_as::<_, (String,)>("SELECT quote_ident(nspname) FROM pg_namespace WHERE nspowner=(SELECT oid FROM pg_roles WHERE rolname='wamn_db_owner') AND nspname NOT IN ('catalog','platform','wamn_run','wamn_history','public')").fetch_all(&mut *tx).await? {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("GRANT USAGE, CREATE ON SCHEMA {schema} TO {role}"))).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub(in crate::environment) async fn recorded_cluster(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<String> {
    let policy = crate::provision_project_env::read_project_env_policy(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    crate::provision_project_env::resolve_cluster(
        &platform.system_database_url,
        &document.org,
        &policy,
    )
    .await
}

/// Reset every application from retained sources, using explicit owning credentials.
pub async fn reset_deployed(
    platform: &Platform,
    triple: &wamn_registry::Triple,
    candidate: Option<(&str, &str)>,
    credentials: &Credentials,
) -> anyhow::Result<Value> {
    let document = EnvironmentDocument::read_serving(platform, triple).await?;
    let private = ensure::PrivateDirectory::create()?;
    let mut platform = platform.clone();
    super::super::runtime::resolve_target(&mut platform, &document, &private.path(".")).await?;
    let lock = super::super::lock::LifecycleLock::acquire_wait(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    let result = lock
        .protect(async {
            let current = EnvironmentDocument::read_serving(&platform, triple).await?;
            ensure!(
                current == document,
                "environment document changed during reset"
            );
            let state = schema::state(&platform, &document).await?;
            ensure!(
                state.disposable == Some(true),
                "environment is not recorded as disposable"
            );
            let mut sources = state
                .packages
                .iter()
                .map(|(id, p)| {
                    (
                        id.clone(),
                        SchemaSource {
                            version: p.coordinate.package_version().to_owned(),
                            artifact: p.artifact_digest.clone(),
                        },
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let mut candidates = Vec::new();
            if let Some((application, release)) = candidate {
                let (_, selected) =
                    schema::release_candidates(&platform, &document, application, Some(release))
                        .await?;
                for c in &selected {
                    let metadata = wamn_catalog::package_schema::package_metadata(&c.directory)?;
                    sources.insert(
                        metadata.coordinate.package_id().to_owned(),
                        SchemaSource {
                            version: metadata.coordinate.package_version().to_owned(),
                            artifact: c.root.artifact_digest.clone(),
                        },
                    );
                }
                candidates = selected;
            }
            // A failed reset can leave a package without a completion row. Its declared release
            // supplies the source when the same operation is retried.
            for (application, selection) in &document.applications {
                if matches!(
                    selection.current,
                    super::super::document::DeclaredRelease::None
                ) || selection.packages.iter().all(|id| sources.contains_key(id))
                {
                    continue;
                }
                let (_, selected) =
                    schema::release_candidates(&platform, &document, application, None).await?;
                for c in selected {
                    let metadata = wamn_catalog::package_schema::package_metadata(&c.directory)?;
                    let id = metadata.coordinate.package_id();
                    if !sources.contains_key(id) {
                        sources.insert(
                            id.to_owned(),
                            SchemaSource {
                                version: metadata.coordinate.package_version().to_owned(),
                                artifact: c.root.artifact_digest.clone(),
                            },
                        );
                        candidates.push(c);
                    }
                }
            }
            for (id, source) in &sources {
                if candidates.iter().any(|c: &Candidate| {
                    wamn_catalog::package_schema::package_metadata(&c.directory)
                        .is_ok_and(|m| m.coordinate.package_id() == id)
                }) {
                    continue;
                }
                let digest = source
                    .artifact
                    .as_deref()
                    .with_context(|| format!("retained package {id} has no published source"))?;
                candidates.push(schema::open(&platform, id, &source.version, Some(digest)).await?);
            }
            schema::validate_candidates(&candidates, &sources)?;
            // Resolve every file through SQLx before users stop or data changes.
            for c in &candidates {
                crate::apply_package::prepare_package(&c.root).await?;
            }
            let url = ensure::project_url(&platform, &document.triple()).await?;
            let admin = wamn_postgres::database_configuration(
                &credentials.owner,
                &wamn_postgres::identity(&url)?.1,
            )?;
            let event_inputs = platform
                .events
                .as_ref()
                .context("reset requires the environment event broker")?;
            let broker = async_nats::jetstream::new(event_inputs.nats.connect().await?);
            let mut package = wamn_postgres::connect(&credentials.migration).await?;
            wamn_control_provision::migration::require_login(
                &mut package,
                &wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env),
            )
            .await?;
            let mut capture = wamn_postgres::connect(&credentials.capture).await?;
            let replication: bool = sqlx::query_scalar(
                "SELECT rolreplication AND NOT rolsuper FROM pg_roles WHERE rolname=session_user",
            )
            .fetch_one(&mut capture)
            .await?;
            ensure!(replication, "reset requires the capture login");
            stop_users(&platform, &document).await?;
            crate::enable_cdc_project_env::discard_backlog(
                &platform.system_database_url,
                &credentials.capture,
                &document.triple(),
            )
            .await?;
            reset_database(
                &platform.system_database_url,
                &admin,
                &credentials.migration,
                &document.triple(),
            )
            .await?;
            wamn_broker::streams::discard(&broker, &document.triple()).await?;
            schema::apply_candidates(&platform, &document, candidates, &url).await?;
            resume_users(&platform, &document, &credentials.owner).await?;
            Ok(serde_json::json!({"database": wamn_postgres::identity(&url)?.1, "result":"reset"}))
        })
        .await;
    lock.release().await;
    result
}

async fn stop_users(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<()> {
    crate::enable_cdc_project_env::stop_reader(platform, &document.triple()).await?;
    let shared = ensure::environment_host_name(&document.org, &document.project, &document.env)?;
    ensure::kubectl(
        platform,
        &platform.target.namespace,
        &[
            "delete",
            "workloaddeployment",
            &wamn_registry::name::resource_suffix(&shared, "materializer"),
            "--ignore-not-found",
            "--wait=true",
            "--timeout=120s",
        ],
        None,
    )
    .await?;
    let mut groups = std::collections::BTreeSet::from([shared]);
    for application in document.applications.keys() {
        groups.insert(analyse::application_release_name(document, application)?);
    }
    for name in &groups {
        stop_group(platform, name).await?;
    }
    Ok(())
}

async fn resume_users(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
) -> anyhow::Result<()> {
    use super::super::{readiness, write};
    // Restore every selected release before readers or hosts can admit work.
    let actor = wamn_identity_protocol::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    let mut applications = Vec::new();
    for (application, selection) in &document.applications {
        if matches!(
            selection.current,
            super::super::document::DeclaredRelease::None
        ) {
            continue;
        }
        let analysis = analyse::analyse(platform, document, Some(application)).await?;
        super::super::expand::expand(platform, document, &analysis, &actor).await?;
        applications.push(analysis);
    }
    ensure::ensure_cdc_inputs(platform, document, Some(admin_url)).await?;
    for analysis in applications {
        let application = &analysis.application;
        let budget = std::time::Duration::from_secs(u64::try_from(
            analysis.authorities.policy.readiness_budget_seconds,
        )?);
        tokio::time::timeout(budget, async {
            let release = analysis
                .authorities
                .release
                .as_ref()
                .context("application release")?;
            let prepared = write::prepare(platform, document, &analysis, release).await?;
            let written = write::write(platform, &analysis, prepared, false).await?;
            readiness::wait(platform, &written).await
        })
        .await
        .with_context(|| format!("resume application {application}: readiness budget expired"))?
        .with_context(|| format!("resume application {application}"))?;
    }
    let control = crate::platform::installed::InstalledPlatform::read(&platform.target)
        .await?
        .context("installed platform")?
        .values;
    ensure::provision_materializer(platform, &control, document).await?;
    Ok(())
}

async fn stop_group(platform: &Platform, name: &str) -> anyhow::Result<()> {
    use wamn_release_control::release_chart;
    if analyse::release_present(platform, name).await? {
        let bytes = crate::platform::resources::helm(
            &platform.target,
            &[
                "get".as_ref(),
                "values".as_ref(),
                name.as_ref(),
                "-n".as_ref(),
                platform.target.namespace.as_ref(),
                "-o".as_ref(),
                "json".as_ref(),
            ],
        )
        .await?;
        let mut values: Value = serde_json::from_slice(&bytes)?;
        stop_values(&mut values)?;
        let private = ensure::PrivateDirectory::create()?;
        let path = private.path("stopped.yaml");
        std::fs::write(&path, serde_yaml::to_string(&values)?)?;
        release_chart::recover_pending(&platform.target, name).await?;
        release_chart::upgrade(
            &platform.target,
            &platform.chart,
            name,
            &path,
            std::time::Duration::from_secs(120),
            "database reset",
        )
        .await?;
    }
    ensure!(
        super::super::drain::wait(platform, name, None)
            .await?
            .is_empty(),
        "database user {name} is still running"
    );
    Ok(())
}

/// Remove chart-owned workloads and stop their hosts using existing chart values.
/// Ordinary reapply derives the roles and host replicas from the selected release again.
pub fn stop_values(values: &mut Value) -> anyhow::Result<()> {
    values["roles"] = serde_json::json!([]);
    for group in values["runtime-operator"]["runtime"]["hostGroups"]
        .as_array_mut()
        .context("host groups")?
    {
        group["replicas"] = 0.into();
    }
    Ok(())
}
