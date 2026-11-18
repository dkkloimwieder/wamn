//! Authenticated environment configuration through installer-owned resources.
use super::{
    Platform,
    document::{EnvironmentDocument, EnvironmentFile},
    ensure as resources,
};
use crate::provision_project_env::{
    self, ProvisionProjectEnvOutcome, ProvisionProjectEnvRequest, workload,
};
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_platform_install as installer;
use wamn_registry::{Triple, name};

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
            wamn_catalog::configuration::require_placement(
                &mut *lock.client().await,
                &document.org,
                &document.env,
                &document.policy,
            )
            .await?;
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
            installer::org::apply(&platform.target, &platform.chart, system_owner, &first.org)
                .await?;
            let project_names: Vec<_> = projects
                .iter()
                .map(|project| project.project.as_str())
                .collect();
            let host = resources::submit_host(
                &platform,
                &first.org,
                &first.env,
                &first.policy,
                &project_names,
            )
            .await?;
            for (index, (project, outcome)) in projects.iter().zip(reserved).enumerate() {
                configure_project(
                    &platform,
                    project,
                    application,
                    outcome,
                    system_owner,
                    &lock,
                    index == 0,
                )
                .await
                .with_context(|| {
                    format!(
                        "Configure project {} failed. Run env apply again to continue.",
                        project.project
                    )
                })?;
            }
            resources::wait_host_ready(&platform, &host).await?;
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
) -> anyhow::Result<ProvisionProjectEnvOutcome> {
    provision_project_env::provision_project_env(&ProvisionProjectEnvRequest {
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        policy: Some(document.policy.clone()),
        target: document.target.clone(),
        disposable,
        system_database_url: Some(system_owner.into()),
        cluster: None,
        connection_limit: None,
        cluster_namespace: document.org.to_string(),
        namespace: document.org.to_string(),
        secret_namespace: Some(document.org.to_string()),
        emit_database: None,
        emit_role_sql: None,
        emit_privilege_sql: None,
        emit_secret: None,
    })
    .await
}

async fn configure_project(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: Option<&wamn_catalog::ApplicationName>,
    outcome: ProvisionProjectEnvOutcome,
    system_owner: &str,
    lock: &LifecycleLock,
    first: bool,
) -> anyhow::Result<()> {
    let triple = &outcome.triple;
    if first {
        resources::wait_broker_certificate(platform, triple).await?;
    }
    let unconfigured = super::document::read_configuration(&mut *lock.client().await, triple)
        .await?
        .is_none();
    let connection = installer::environment::connection(system_owner, triple).await?;
    let owner = wamn_provision::migrations::MigrationDatabase::Project
        .configuration(&connection.to_json()?)?;
    let schemas = if unconfigured {
        Vec::new()
    } else {
        let mut project = wamn_postgres::connect(&owner).await?;
        let unfinished = super::drain::unfinished(&mut project, &triple.env, application).await?;
        ensure!(
            unfinished.is_empty(),
            "{}",
            super::drain::describe(&unfinished)
        );
        resources::publication_schemas(platform, document).await?
    };
    let capture = installer::environment::capture_connection(system_owner, triple).await?;
    let publication = wamn_provision::cdc_object_name(
        &triple.org,
        &triple.project,
        &triple.env,
        &outcome.instance,
    );
    let connections = workload::connections(triple, &connection)?;

    wamn_broker::authorization::provision_project(&platform.target, triple).await?;

    let mut values = outcome.database_values;
    installer::platform::resources::merge(
        &mut values,
        installer::platform::operations::values(triple, &outcome.cluster, &connection.database)?,
    );
    installer::platform::resources::merge(
        &mut values,
        installer::environment::publication_values(
            triple,
            &outcome.instance,
            &outcome.cluster,
            &publication,
            &schemas,
        ),
    );
    let capture_config: wamn_postgres::ConnectionConfig = serde_json::from_str(&capture)?;
    values["loginSecrets"] = connections
        .iter()
        .map(|(_, connection)| connection)
        .chain(std::iter::once(&capture_config))
        .map(
            |connection| json!({"name":connection.credentials.name,"username":connection.username}),
        )
        .collect::<Vec<_>>()
        .into();
    values["databaseConfig"] =
        workload::configuration_values(triple, &outcome.instance, &connections)?;
    let certificate = name::cdc_nats_certificate(triple);
    values["brokerCertificates"] = json!([wamn_broker::authorization::certificate(
        &certificate,
        &certificate,
        &name::cdc_nats_user(triple),
    )]);
    values["cdcReader"] = reader_values(platform, triple, &publication, &capture).await?;
    // Wait for the database first because the reader needs the slot created below.
    let release = wamn_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    installer::environment::apply_project(&platform.target, &platform.chart, &release, &values)
        .await?;
    wamn_kubernetes::kubectl::wait(
        &platform.target.kubeconfig,
        &platform.target.context,
        triple.org.as_str(),
        "database",
        &connection.database,
        "condition=Applied",
        "120s",
    )
    .await?;

    LifecycleLock::hold(
        wamn_postgres::connect(&owner).await?,
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
                &capture,
                &connection.database,
                &publication,
                &schemas,
            )
            .await?;
            wamn_kubernetes::kubectl::wait(
                &platform.target.kubeconfig,
                &platform.target.context,
                triple.org.as_str(),
                "certificate",
                &certificate,
                "condition=Ready",
                "120s",
            )
            .await?;
            resources::ensure_stream(platform, triple).await?;
            super::cdc_reader::wait_reader_ready(&platform.target, triple).await?;
            let mut connection = lock.client().await;
            let mut tx = provision_project_env::provisioning_transaction(&mut connection).await?;
            document.save(&mut tx).await?;
            tx.commit().await?;
            Ok(())
        },
    )
    .await
}

async fn reader_values(
    platform: &Platform,
    triple: &Triple,
    publication: &str,
    capture: &str,
) -> anyhow::Result<Value> {
    let image = installer::release_chart::image_set(&platform.chart)
        .await?
        .host;
    let config = super::cdc_reader::reader_configuration(
        triple,
        publication,
        installer::platform::identity::NATS_URL,
    );
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let capture_name = wamn_provision::project_env_cdc_secret_name(org, project, env);
    Ok(json!({
        "name":capture_name,
        "key": name::project_env_key(org, project, env),
        "coordinate": triple.to_string(),
        "image": image,
        "registrySecret": (!platform.registry_token_metadata).then_some(name::REGISTRY_PULL),
        "natsSecret": name::cdc_nats_certificate(triple),
        "connection": capture,
        "configuration": config,
    }))
}
