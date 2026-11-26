//! Authenticated environment configuration through installer-owned resources.
use super::database::{DatabaseConnection, LifecycleLock};
use super::{
    Platform,
    document::{EnvDocument, EnvFile},
    ensure as resources,
};
use crate::env_sql_effects::{self, PostgresSession};
use crate::provision_project_env::workload;
use crate::registry::configuration;
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_platform_install as installer;
use wamn_postgres::database::{ConnectionConfig, capture_configuration, connection_for};
use wamn_registry::{Triple, name};

pub async fn configure(
    platform: &mut Platform,
    document: EnvFile,
    app: Option<&wamn_registry::AppName>,
    disposable: Option<bool>,
) -> anyhow::Result<()> {
    super::runtime::resolve_platform(platform)?;
    let lock = env_sql_effects::lock_env(
        DatabaseConnection::Direct(&platform.system_database_url),
        &document.org,
        &document.env,
    )
    .await?;
    let result = lock
        .protect(async {
            let sizing = serde_json::to_value(&document.database)?;
            let readiness_budget_seconds = document.readiness_budget_seconds;
            let drain_bound_seconds = document.drain_bound_seconds;
            let revocation_seconds = document.revocation_seconds;
            let settings = platform.cluster.postgres_settings().await?;
            let projects: Vec<_> = match app {
                Some(app) => vec![document.into_app(app)?],
                None => document.into_projects().collect(),
            };
            let first = projects
                .first()
                .context("environment has no selected projects")?;
            let project_names: Vec<_> = projects
                .iter()
                .map(|project| project.settings.project.as_str())
                .collect();
            let reserved = lock.session.reserve(&projects, disposable).await?;
            let count = lock
                .session
                .project_count(&first.settings.org, &first.settings.env)
                .await?;
            super::broker::configure(platform, &first.settings.org, &first.settings.env, count)
                .await?;
            let (org, env) = (&first.settings.org, &first.settings.env);
            let mut host_values = resources::read_host_values(
                platform,
                &lock.session,
                org,
                env,
                &project_names,
                None,
                None,
            )
            .await?;
            // The runtime operator defaults the group to one replica.
            let replicas = if host_values["placement"]["replicas"].is_null() {
                1
            } else {
                u32::try_from(
                    host_values["placement"]["replicas"]
                        .as_u64()
                        .context("invalid host replica count")?,
                )?
            };
            for (project, instance) in projects.iter().zip(reserved) {
                let connection = connection_for(&project.triple(), &instance);
                let database = crate::platform_env::database_values(
                    &connection,
                    replicas,
                    sizing.clone(),
                    &settings,
                )?;
                configure_project(
                    platform,
                    project,
                    app,
                    &instance,
                    &connection,
                    database,
                    &lock.session,
                )
                .await
                .with_context(|| {
                    format!(
                        "Configure project {} failed. Run env apply again to continue.",
                        project.settings.project
                    )
                })?;
            }
            // Keep the lock session free for its heartbeat while Helm waits.
            let mut connection = wamn_postgres::connect(&platform.system_database_url).await?;
            let mut transaction =
                wamn_postgres::provisioning_transaction::provisioning_transaction(&mut connection)
                    .await?;
            if app.is_none() {
                let names: Vec<_> = first
                    .settings
                    .integrations
                    .keys()
                    .map(String::as_str)
                    .collect();
                configuration::retain_integrations(&mut transaction, org, env, &names)
                    .await
                    .context("remove connection pins after project configuration")?;
            }
            let drain_bound_seconds = configuration::write_env(
                &mut transaction,
                org,
                env,
                readiness_budget_seconds,
                drain_bound_seconds,
                revocation_seconds,
            )
            .await
            .context("save the applied environment settings")?;
            host_values["drainBoundSeconds"] = drain_bound_seconds.into();
            resources::upgrade_host(platform, org, env, &host_values).await?;
            transaction.commit().await?;
            Ok(())
        })
        .await;
    lock.release().await;
    result
}

async fn configure_project(
    platform: &Platform,
    document: &EnvDocument,
    app: Option<&wamn_registry::AppName>,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
    connection: &ConnectionConfig,
    mut values: Value,
    lock: &PostgresSession,
) -> anyhow::Result<()> {
    let triple = &document.triple();
    let unconfigured = lock.read_configuration(triple).await?.is_none();
    let owner = connection.to_json()?;
    let cluster = name::project_cluster(&connection.database);
    let schemas = if unconfigured {
        Vec::new()
    } else {
        platform.prepare_database(connection).await?;
        let unfinished =
            env_sql_effects::unfinished_in_database(&owner, &connection.database, &triple.env, app)
                .await?;
        ensure!(
            unfinished.is_empty(),
            "{}",
            super::drain::describe(&unfinished)
        );
        resources::publication_schemas(platform, lock, document).await?
    };
    let capture = capture_configuration(connection.clone())?;
    let publication =
        wamn_registry::name::cdc_object_name(&triple.org, &triple.project, &triple.env, instance);
    let connections = workload::connections(connection)?;

    super::broker::configure_project(platform, triple).await?;

    super::chart_values::set_operation_values(
        &mut values,
        triple,
        &cluster,
        &connection.database,
        document.apps.keys(),
    )?;
    values["publication"] =
        super::chart_values::publication_values(triple, instance, &cluster, &publication, &schemas)
            ["publication"]
            .take();
    values["databaseConfig"] = workload::configuration_values(triple, instance, &connections)?;
    let image = wamn_platform_install::deployment::host_image(&platform.chart).await?;
    values["cdcReader"] = reader_values(
        triple,
        &capture,
        &image,
        &platform.nats_url,
        platform.registry_token_metadata,
    );
    let paused = lock.has_capture_gap(triple).await?;
    values["cdcReader"]["pause"] = paused.into();
    // Wait for the database first because the reader needs the slot created below.
    installer::env::validate_project_env(&triple.org, &triple.project, &triple.env)?;
    let release = name::project_env_resource_name(&triple.org, &triple.project, &triple.env);
    wamn_platform_install::deployment::apply_project(
        &platform.cluster,
        &platform.chart,
        &release,
        &values,
    )
    .await?;
    wamn_platform_install::deployment::wait_database(&platform.cluster, connection, &capture)
        .await?;
    if unconfigured {
        platform.prepare_database(connection).await?;
    }
    LifecycleLock::hold(
        DatabaseConnection::Wait {
            input: &owner,
            database: &connection.database,
        },
        triple,
        async |project_lock| {
            project_lock
                .initialize_project(&connection.database, &owner, unconfigured)
                .await?;
            lock.fill(&owner, triple, &platform.identity_issuer).await?;
            project_lock.grant_workload(&connections).await?;
            if !schemas.is_empty() {
                platform.prepare_database(&capture).await?;
                env_sql_effects::prepare_cdc(
                    &owner,
                    &capture.to_json()?,
                    &connection.database,
                    &publication,
                    &schemas,
                )
                .await?;
            }
            resources::ensure_stream(platform, triple).await?;
            if !schemas.is_empty() && !paused {
                platform.cluster.wait_reader_ready(triple).await?;
            }
            // Stop before save deactivates the apps, so apply can retry a failed stop.
            let removed: Vec<_> = super::analyse::app_releases(platform, lock, triple)
                .await?
                .into_iter()
                .filter(|(app, _)| !document.apps.contains_key(app))
                .collect();
            // Refuse before any uninstall, so no removed application stops alone.
            let mut unfinished = Vec::new();
            for (app, _) in &removed {
                unfinished.extend(project_lock.unfinished(&triple.env, Some(app)).await?);
            }
            ensure!(
                unfinished.is_empty(),
                "{}",
                super::drain::describe(&unfinished)
            );
            for (removed, release) in removed {
                println!("stopping {removed} (release {release})");
                super::drain::stop(platform, project_lock, &triple.env, &removed, &release).await?;
            }
            lock.save_deployment(
                triple,
                &document.configuration(),
                &publication,
                &schemas,
                &capture,
            )
            .await?;
            Ok(())
        },
    )
    .await
}

/// Build CDC reader chart values from resolved connection and image settings.
pub fn reader_values(
    triple: &Triple,
    capture: &ConnectionConfig,
    image: &str,
    nats_url: &str,
    registry_token_metadata: bool,
) -> Value {
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let capture_name = wamn_registry::name::cdc_reader_name(org, project, env);
    json!({
        "name":capture_name,
        "key": name::project_env_key(org, project, env),
        "coordinate": triple.to_string(),
        "image": image,
        "registrySecret": (!registry_token_metadata).then_some(name::REGISTRY_PULL),
        "natsSecret": name::project_nats_secret(triple, "cdc"),
        "database": capture,
        "natsUrl": nats_url,
    })
}
