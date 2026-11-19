//! Authenticated environment configuration through installer-owned resources.
use super::{
    Platform,
    document::{EnvironmentDocument, EnvironmentFile},
    ensure as resources,
};
use crate::provision_project_env::{self, registry, workload};
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::time::Duration;
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_platform_install as installer;
use wamn_registry::{Triple, name, sql::upsert_event_reader_sql};

pub async fn configure(
    platform: &Platform,
    document: EnvironmentFile,
    application: Option<&wamn_catalog::ApplicationName>,
    disposable: Option<bool>,
    system_owner: &str,
) -> anyhow::Result<()> {
    let mut platform = platform.clone();
    super::runtime::resolve_platform(&mut platform)?;
    let lock = LifecycleLock::acquire_environment(
        wamn_postgres::connect(system_owner).await?,
        &document.org,
        &document.env,
    )
    .await?;
    let result = lock
        .protect(async {
            let applied_document = document.to_yaml()?;
            let sizing = serde_json::to_value(&document.database)?;
            let settings = resources::postgres_settings(&platform).await?;
            let projects: Vec<_> = match application {
                Some(application) => vec![document.into_application(application)?],
                None => document.into_projects().collect(),
            };
            let first = projects
                .first()
                .context("environment has no selected projects")?;
            let mut reserved = Vec::with_capacity(projects.len());
            for project in &projects {
                let outcome = reserve_project(project, disposable, system_owner)
                    .await
                    .with_context(|| {
                        format!(
                            "Reserve project {} failed. Run env apply again to continue.",
                            project.project
                        )
                    })?;
                reserved.push(outcome);
            }
            let project_names: Vec<_> = projects
                .iter()
                .map(|project| project.project.as_str())
                .collect();
            let (replicas, host) = resources::prepare_host_upgrade(
                &platform,
                &first.org,
                &first.env,
                &first.policy,
                &project_names,
            )
            .await?;
            let configure_projects = async {
                for (index, (project, instance)) in projects.iter().zip(reserved).enumerate() {
                    let connection =
                        installer::environment::connection_for(&project.triple(), &instance);
                    let database = installer::environment::database_values(
                        &connection,
                        replicas,
                        sizing.clone(),
                        &settings,
                    )?;
                    configure_project(
                        &platform,
                        project,
                        application,
                        &instance,
                        &connection,
                        database,
                        system_owner,
                        &lock,
                        index == 0,
                        &applied_document,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Configure project {} failed. Run env apply again to continue.",
                            project.project
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
                let names: Vec<_> = first.integrations.keys().map(String::as_str).collect();
                let mut connection = lock.client().await;
                let mut transaction =
                    provision_project_env::provisioning_transaction(&mut connection).await?;
                wamn_catalog::configuration::retain_integrations(
                    &mut transaction,
                    &first.org,
                    &first.env,
                    &names,
                )
                .await
                .context("remove connection pins after project configuration")?;
                transaction.commit().await?;
            }
            Ok(())
        })
        .await;
    lock.release().await;
    result
}

async fn reserve_project(
    document: &EnvironmentDocument,
    disposable: Option<bool>,
    system_owner: &str,
) -> anyhow::Result<wamn_registry::instance_suffix::InstanceSuffix> {
    wamn_provision::validate_project_env_cdc(&document.org, &document.project, &document.env)?;
    registry::record_project_env(
        system_owner,
        &document.triple(),
        Some(&document.policy),
        &document.target,
        &name::project_env_secret_name(&document.org, &document.project, &document.env),
        Some(document.org.as_str()),
        &registry::mint_instance_suffix()?,
        disposable,
    )
    .await
}

async fn configure_project(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: Option<&wamn_catalog::ApplicationName>,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
    connection: &wamn_postgres::ConnectionConfig,
    mut values: Value,
    system_owner: &str,
    lock: &LifecycleLock,
    first: bool,
    applied_document: &str,
) -> anyhow::Result<()> {
    let triple = &document.triple();
    if first {
        let admin = name::environment_nats_admin(&triple.org, &triple.env);
        resources::wait_certificate(platform, &admin).await?;
    }
    let unconfigured = super::document::read_configuration(&mut *lock.client().await, triple)
        .await?
        .is_none();
    let owner = connection.to_json()?;
    let cluster = name::project_cluster(&connection.database);
    let schemas = if unconfigured {
        Vec::new()
    } else {
        platform.prepare_database(connection).await?;
        let mut project =
            installer::environment::wait_project_connection(&owner, &connection.database).await?;
        let unfinished = super::drain::unfinished(&mut project, &triple.env, application).await?;
        ensure!(
            unfinished.is_empty(),
            "{}",
            super::drain::describe(&unfinished)
        );
        resources::publication_schemas(platform, document).await?
    };
    let capture = installer::environment::capture_configuration(connection.clone())?;
    let publication =
        wamn_provision::cdc_object_name(&triple.org, &triple.project, &triple.env, instance);
    let connections = workload::connections(connection)?;

    wamn_broker::authorization::provision_project(&platform.target, triple).await?;

    installer::environment::set_operation_values(
        &mut values,
        triple,
        &cluster,
        &connection.database,
        document.applications.keys(),
    )?;
    values["publication"] = installer::environment::publication_values(
        triple,
        instance,
        &cluster,
        &publication,
        &schemas,
    )["publication"]
        .take();
    values["databaseConfig"] = workload::configuration_values(triple, instance, &connections)?;
    let certificate = name::cdc_nats_certificate(triple);
    let recovery = name::recovery_nats_user(triple);
    values["brokerCertificates"] = json!([
        wamn_broker::authorization::certificate(
            &certificate,
            &certificate,
            &name::cdc_nats_user(&triple.org, &triple.project, &triple.env),
        ),
        wamn_broker::authorization::certificate(&recovery, &recovery, &recovery),
    ]);
    values["environmentDocument"] = applied_document.into();
    values["cdcReader"] = reader_values(platform, triple, &capture).await?;
    let paused = super::capture_gap::is_open(&mut *lock.client().await, triple).await?;
    values["cdcReader"]["pause"] = paused.into();
    // Wait for the database first because the reader needs the slot created below.
    let release = wamn_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    installer::environment::apply_project(&platform.target, &platform.chart, &release, &values)
        .await?;
    wamn_kubernetes::kubectl::wait_ready(
        &platform.target,
        "cluster.postgresql.cnpg.io",
        &cluster,
        Duration::from_secs(120),
    )
    .await?;
    installer::environment::wait_database_applied(
        &platform.target,
        &connection.database,
        Duration::from_secs(120),
    )
    .await?;

    if unconfigured {
        platform.prepare_database(connection).await?;
    }
    platform.prepare_database(&capture).await?;
    let capture_json = capture.to_json()?;
    LifecycleLock::hold(
        installer::environment::wait_project_connection(&owner, &connection.database).await?,
        triple,
        async |project_lock| {
            let mut client = project_lock.client().await;
            sqlx::raw_sql(sqlx::AssertSqlSafe(provision_project_env::privilege_sql(
                &connection.database,
            )))
            .execute(&mut *client)
            .await?;
            drop(client);
            if unconfigured {
                installer::platform_migrations::install_project(&mut *project_lock.client().await)
                    .await?;
                crate::platform_migrations::restore_project(
                    &owner,
                    &crate::platform_migrations::embedded_sources(
                        wamn_provision::migrations::MigrationDatabase::Project,
                    ),
                )
                .await?;
            }
            super::fill(
                system_owner,
                &owner,
                &platform.identity_issuer,
                wamn_identity::application::ApplicationTarget {
                    org: &triple.org,
                    project: &triple.project,
                    env: &triple.env,
                },
            )
            .await?;
            workload::grant(&mut *project_lock.client().await, &connections).await?;
            resources::prepare_cdc(
                &owner,
                &capture_json,
                &connection.database,
                &publication,
                &schemas,
            )
            .await?;
            tokio::try_join!(
                resources::wait_certificate(platform, &certificate),
                resources::wait_certificate(platform, &recovery),
            )?;
            resources::ensure_stream(platform, triple).await?;
            if !paused {
                super::cdc_reader::wait_reader_ready(&platform.target, triple).await?;
            }
            let mut connection = lock.client().await;
            let mut tx = provision_project_env::provisioning_transaction(&mut connection).await?;
            document.save(&mut tx).await?;
            // prepare_cdc creates the failover slot under the publication name.
            let slot = &publication;
            let stream = wamn_event::stream_name(&triple.org, &triple.project, &triple.env);
            sqlx::query(sqlx::AssertSqlSafe(upsert_event_reader_sql()))
                .bind(&triple.org)
                .bind(&triple.project)
                .bind(&triple.env)
                .bind(&publication)
                .bind(schemas.as_slice())
                .bind(slot)
                .bind(stream)
                .bind(&capture.credentials.name)
                .bind(&capture.credentials.namespace)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("register the CDC reader for {triple}"))?;
            tx.commit().await?;
            Ok(())
        },
    )
    .await
}

async fn reader_values(
    platform: &Platform,
    triple: &Triple,
    capture: &wamn_postgres::ConnectionConfig,
) -> anyhow::Result<Value> {
    let image = installer::release_chart::image_set(&platform.chart)
        .await?
        .host;
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let capture_name = wamn_provision::cdc_reader_name(org, project, env);
    Ok(json!({
        "name":capture_name,
        "key": name::project_env_key(org, project, env),
        "coordinate": triple.to_string(),
        "image": image,
        "registrySecret": (!platform.registry_token_metadata).then_some(name::REGISTRY_PULL),
        "natsSecret": name::cdc_nats_certificate(triple),
        "database": capture,
        "natsUrl": platform.nats_url,
    }))
}
