//! Authenticated environment configuration through installer-owned resources.
use wamn_kubernetes::client::{ready, resource, wait_named};
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
) -> anyhow::Result<()> {
    let mut platform = platform.clone();
    super::runtime::resolve_platform(&mut platform)?;
    let lock = LifecycleLock::acquire_environment(
        wamn_postgres::connect(&platform.system_database_url).await?,
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
            let project_names: Vec<_> = projects
                .iter()
                .map(|project| project.project.as_str())
                .collect();
            let mut reserved = Vec::with_capacity(projects.len());
            {
                let mut client = lock.client().await;
                let mut transaction = provision_project_env::provisioning_transaction(&mut client)
                    .await?;
                sqlx::query(wamn_registry::sql::PROJECT_STREAM_CAPACITY_LOCK_SQL)
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
                .bind(&first.org)
                .bind(&first.env)
                .bind(&project_names)
                .fetch_one(&mut *transaction)
                .await
                .context("count project stream reservations")?;
                let max_projects = max_projects.context("the system SQL Job has not set the project stream limit")?;
                if additional > 0 {
                    let available: bool =
                        sqlx::query_scalar("SELECT registry.project_capacity_available($1)")
                            .bind(additional)
                            .fetch_one(&mut *transaction)
                            .await
                            .context("check platform project stream capacity")?;
                    ensure!(
                        available,
                        "project stream limit is {max_projects}, count would exceed it ({additional} new)"
                    );
                }
                for project in &projects {
                    reserved.push(
                        reserve_project(&mut transaction, project, disposable)
                            .await
                            .with_context(|| format!("reserve project {}", project.project))?,
                    );
                }
                transaction.commit().await.context("commit project reservations")?;
            }
            let count =
                super::broker::project_count(&mut *lock.client().await, &first.org, &first.env)
                    .await?;
            super::broker::configure(&platform, &first.org, &first.env, count).await?;
            let (replicas, host) = resources::prepare_host_upgrade(
                &platform,
                &lock,
                &first.org,
                &first.env,
                first.drain_bound_seconds,
                &project_names,
            )
            .await?;
            let configure_projects = async {
                for (project, instance) in projects.iter().zip(reserved) {
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
                        &lock,
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
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    document: &EnvironmentDocument,
    disposable: Option<bool>,
) -> anyhow::Result<wamn_registry::instance_suffix::InstanceSuffix> {
    wamn_provision::validate_project_env_cdc(&document.org, &document.project, &document.env)?;
    registry::record_project_env(
        transaction,
        &document.triple(),
        document.readiness_budget_seconds,
        document.drain_bound_seconds,
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
    lock: &LifecycleLock,
    applied_document: &str,
) -> anyhow::Result<()> {
    let triple = &document.triple();
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
        resources::publication_schemas(platform, lock, document).await?
    };
    let capture = installer::environment::capture_configuration(connection.clone())?;
    let publication =
        wamn_provision::cdc_object_name(&triple.org, &triple.project, &triple.env, instance);
    let connections = workload::connections(connection)?;

    super::broker::configure_project(platform, triple).await?;

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
    let target = &platform.target;
    let clusters = resource(
        target.client.client(),
        &target.namespace,
        "postgresql.cnpg.io",
        "v1",
        "Cluster",
        "clusters",
    );
    wait_named(&clusters, &cluster, Duration::from_secs(120), |object| {
        Ok(object.filter(ready).map(drop))
    })
    .await?;
    installer::environment::wait_database_applied(
        &platform.target,
        &connection.database,
        Duration::from_secs(120),
    )
    .await?;

    let certificates = resource(
        target.client.client(),
        &target.namespace,
        "cert-manager.io",
        "v1",
        "Certificate",
        "certificates",
    );
    let wait_certificate = |name| {
        wait_named(&certificates, name, Duration::from_secs(120), |object| {
            Ok(object.filter(ready).map(drop))
        })
    };
    tokio::try_join!(
        wait_certificate(&connection.credentials.name),
        wait_certificate(&capture.credentials.name),
    )?;
    if unconfigured {
        platform.prepare_database(connection).await?;
    }
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
            {
                let mut project = wamn_postgres::connect(&owner).await?;
                super::fill(
                    &mut *lock.client().await,
                    &mut project,
                    &platform.identity_issuer,
                    wamn_identity::application::ApplicationTarget {
                        org: &triple.org,
                        project: &triple.project,
                        env: &triple.env,
                    },
                )
                .await?;
            }
            workload::grant(&mut *project_lock.client().await, &connections).await?;
            if !schemas.is_empty() {
                platform.prepare_database(&capture).await?;
                resources::prepare_cdc(
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
                super::cdc_reader::wait_reader_ready(&platform.target, triple).await?;
            }
            let mut connection = lock.client().await;
            let mut tx = provision_project_env::provisioning_transaction(&mut connection).await?;
            document.save(&mut tx).await?;
            // With deployed schemas, the failover slot uses the publication name.
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
        "natsSecret": name::project_nats_secret(triple, "cdc"),
        "database": capture,
        "natsUrl": platform.nats_url,
    }))
}
