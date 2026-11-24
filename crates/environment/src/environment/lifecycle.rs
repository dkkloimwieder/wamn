//! Authenticated environment configuration through installer-owned resources.
use super::database::{DatabaseConnection, DatabaseSession, LifecycleLock};
use super::{
    Platform,
    document::{EnvironmentDocument, EnvironmentFile},
    ensure as resources,
};
use crate::provision_project_env::workload;
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_platform_install as installer;
use wamn_platform_install::database::ConnectionConfig;
use wamn_registry::{Triple, name};

pub async fn configure(
    platform: &mut Platform,
    document: EnvironmentFile,
    application: Option<&wamn_registry::ApplicationName>,
    disposable: Option<bool>,
) -> anyhow::Result<()> {
    super::runtime::resolve_platform(platform)?;
    let lock = platform
        .database
        .lock(
            DatabaseConnection::Direct(&platform.system_database_url),
            &document.org,
            &document.env,
        )
        .await?;
    let result = lock
        .protect(async {
            let applied_document = document.to_yaml()?;
            let sizing = serde_json::to_value(&document.database)?;
            let settings = platform.cluster.postgres_settings().await?;
            let projects: Vec<_> = match application {
                Some(application) => vec![document.into_application(application)?],
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
            let (replicas, host) = resources::prepare_host_upgrade(
                platform,
                &*lock.session,
                &first.settings.org,
                &first.settings.env,
                first.drain_bound_seconds,
                &project_names,
            )
            .await?;
            let configure_projects = async {
                for (project, instance) in projects.iter().zip(reserved) {
                    let connection =
                        installer::environment::connection_for(&project.triple(), &instance);
                    let database = (platform.database_values)(
                        &connection,
                        replicas,
                        sizing.clone(),
                        &settings,
                    )?;
                    configure_project(
                        platform,
                        project,
                        application,
                        &instance,
                        &connection,
                        database,
                        &*lock.session,
                        &applied_document,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Configure project {} failed. Run env apply again to continue.",
                            project.settings.project
                        )
                    })?;
                }
                anyhow::Ok(())
            };
            // Start Helm before project provisioning and observe its result after the project loop.
            let (host_result, ()) =
                tokio::try_join!(biased; async { anyhow::Ok(host.await) }, configure_projects)?;
            host_result?;
            if application.is_none() {
                let names: Vec<_> = first
                    .settings
                    .integrations
                    .keys()
                    .map(String::as_str)
                    .collect();
                lock.session
                    .retain_integrations(&first.settings.org, &first.settings.env, &names)
                    .await?;
            }
            Ok(())
        })
        .await;
    lock.release().await;
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "configuration needs the project reservation, database values, and held lock"
)]
async fn configure_project(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: Option<&wamn_registry::ApplicationName>,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
    connection: &ConnectionConfig,
    mut values: Value,
    lock: &dyn DatabaseSession,
    applied_document: &str,
) -> anyhow::Result<()> {
    let triple = &document.triple();
    let unconfigured = lock.read_configuration(triple).await?.is_none();
    let owner = connection.to_json()?;
    let cluster = name::project_cluster(&connection.database);
    let schemas = if unconfigured {
        Vec::new()
    } else {
        platform.prepare_database(connection).await?;
        let unfinished = platform
            .database
            .unfinished(&owner, &connection.database, &triple.env, application)
            .await?;
        ensure!(
            unfinished.is_empty(),
            "{}",
            super::drain::describe(&unfinished)
        );
        resources::publication_schemas(platform, lock, document).await?
    };
    let capture = installer::environment::capture_configuration(connection.clone())?;
    let publication =
        wamn_registry::name::cdc_object_name(&triple.org, &triple.project, &triple.env, instance);
    let connections = workload::connections(connection)?;

    super::broker::configure_project(platform, triple).await?;

    super::chart_values::set_operation_values(
        &mut values,
        triple,
        &cluster,
        &connection.database,
        document.applications.keys(),
    )?;
    values["publication"] =
        super::chart_values::publication_values(triple, instance, &cluster, &publication, &schemas)
            ["publication"]
            .take();
    values["databaseConfig"] = workload::configuration_values(triple, instance, &connections)?;
    values["environmentDocument"] = applied_document.into();
    values["cdcReader"] = reader_values(platform, triple, &capture).await?;
    let paused = lock.has_capture_gap(triple).await?;
    values["cdcReader"]["pause"] = paused.into();
    // Wait for the database first because the reader needs the slot created below.
    installer::environment::validate_project_env(&triple.org, &triple.project, &triple.env)?;
    let release = name::environment_resource_name(&triple.org, &triple.project, &triple.env);
    platform
        .cluster
        .apply_project(&platform.chart, &release, &values)
        .await?;
    platform.cluster.wait_database(connection, &capture).await?;
    if unconfigured {
        platform.prepare_database(connection).await?;
    }
    LifecycleLock::hold(
        &*platform.database,
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
                platform
                    .database
                    .prepare_cdc(
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
            // Stop before save deletes the registry rows, so a failed stop is found again.
            let removed: Vec<_> = super::analyse::application_releases(platform, lock, triple)
                .await?
                .into_iter()
                .filter(|(application, _)| !document.applications.contains_key(application))
                .collect();
            // Refuse before any uninstall, so no removed application stops alone.
            let mut unfinished = Vec::new();
            for (application, _) in &removed {
                unfinished.extend(
                    project_lock
                        .unfinished(&triple.env, Some(application))
                        .await?,
                );
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

async fn reader_values(
    platform: &Platform,
    triple: &Triple,
    capture: &ConnectionConfig,
) -> anyhow::Result<Value> {
    let image = platform.cluster.host_image(&platform.chart).await?;
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let capture_name = wamn_registry::name::cdc_reader_name(org, project, env);
    Ok(json!({
        "name":capture_name,
        "key": name::project_env_key(org, project, env),
        "coordinate": triple.to_string(),
        "image": image,
        "registrySecret": (!platform.registry_token_metadata).then_some(name::REGISTRY_PULL),
        "natsSecret": name::project_nats_secret(triple, "cdc"),
        "database": capture,
        "natsUrl": platform.nats_url,
    }))
}
