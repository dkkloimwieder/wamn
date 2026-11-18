//! Authenticated environment configuration through installer-owned resources.
use super::{Platform, document::EnvironmentDocument, ensure as resources};
use crate::provision_project_env::{self, ProvisionProjectEnvRequest, workload};
use anyhow::ensure;
use serde_json::{Value, json};
use wamn_catalog::lifecycle_lock::LifecycleLock;
use wamn_platform_install as installer;
use wamn_registry::{Triple, name};

pub async fn configure(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: Option<&wamn_catalog::ApplicationName>,
    disposable: Option<bool>,
    system_owner: &str,
) -> anyhow::Result<()> {
    if let Some(application) = application {
        ensure!(
            document.applications.contains_key(application),
            "application is not declared"
        );
    }
    let mut platform = platform.clone();
    super::runtime::resolve_platform(&mut platform)?;
    let triple = document.triple();
    let client = wamn_postgres::connect(system_owner).await?;
    LifecycleLock::hold(client, &triple, async |lock| {
        wamn_catalog::configuration::require_placement(
            &mut *lock.client().await, &document.org, &document.env, &document.policy,
        ).await?;
        let outcome = provision_project_env::provision_project_env(&ProvisionProjectEnvRequest {
            org: triple.org.clone(),
            project: triple.project.clone(),
            env: triple.env.clone(),
            policy: Some(document.policy.clone()),
            target: document.target.clone(),
            disposable,
            system_database_url: Some(system_owner.into()),
            cluster: None,
            connection_limit: None,
            cluster_namespace: triple.org.to_string(),
            namespace: triple.org.to_string(),
            secret_namespace: Some(triple.org.to_string()),
            emit_database: None,
            emit_role_sql: None,
            emit_privilege_sql: None,
            emit_secret: None,
        }).await?;
        let unconfigured = super::document::read_configuration(&mut *lock.client().await, &triple)
            .await?.is_none();
        let connection = installer::environment::connection(system_owner, &triple).await?;
        let owner = wamn_provision::migrations::MigrationDatabase::Project
            .configuration(&connection.to_json()?)?;
        let schemas = if unconfigured {
            Vec::new()
        } else {
            let mut project = wamn_postgres::connect(&owner).await?;
            let unfinished = super::drain::unfinished(&mut project, &triple.env, application).await?;
            ensure!(unfinished.is_empty(), "{}", super::drain::describe(&unfinished));
            resources::publication_schemas(&platform, document).await?
        };
        let capture = installer::environment::capture_connection(system_owner, &triple).await?;
        let publication = wamn_provision::cdc_object_name(
            &triple.org, &triple.project, &triple.env, &outcome.instance,
        );
        let connections = workload::connections(&triple, &connection)?;

        installer::org::apply(&platform.target, &platform.chart, system_owner, &triple.org).await?;
        wamn_broker::authorization::provision_project(&platform.target, &triple).await?;

        let mut values = outcome.database_values;
        installer::platform::resources::merge(&mut values,
            installer::platform::operations::values(&triple, &outcome.cluster, &connection.database)?);
        installer::platform::resources::merge(&mut values,
            installer::environment::publication_values(&triple, &outcome.instance, &outcome.cluster, &publication, &schemas));
        let capture_config: wamn_postgres::ConnectionConfig = serde_json::from_str(&capture)?;
        values["loginSecrets"] = connections.iter().map(|(_, connection)| connection)
            .chain(std::iter::once(&capture_config))
            .map(|connection| json!({"name":connection.credentials.name,"username":connection.username}))
            .collect::<Vec<_>>().into();
        values["databaseConfig"] = workload::configuration_values(&triple, &outcome.instance, &connections)?;
        let certificate = name::cdc_nats_certificate(&triple);
        values["brokerCertificates"] = json!([wamn_broker::authorization::certificate(
            &certificate, &certificate, &name::cdc_nats_user(&triple),
        )]);
        values["cdcReader"] = reader_values(&platform, &triple, &publication, &capture).await?;
        // Wait for the database first because the reader needs the slot created below.
        let release = wamn_provision::environment_names::environment_resource_name(
            &triple.org, &triple.project, &triple.env,
        )?;
        installer::environment::apply_project(&platform.target, &platform.chart, &release, &values).await?;
        wamn_kubernetes::kubectl::wait(&platform.target.kubeconfig, &platform.target.context,
            triple.org.as_str(), "database", &connection.database, "condition=Applied", "120s").await?;

        LifecycleLock::hold(wamn_postgres::connect(&owner).await?, &triple, async |project_lock| {
            let mut client = project_lock.client().await;
            sqlx::raw_sql(sqlx::AssertSqlSafe(provision_project_env::privilege_sql(&connection.database)))
                .execute(&mut *client).await?;
            drop(client);
            if unconfigured {
                crate::platform_migrations::restore_project(
                    &owner, &crate::platform_migrations::embedded_sources(
                        wamn_provision::migrations::MigrationDatabase::Project),
                ).await?;
            }
            super::fill(system_owner, &owner, &platform.identity_issuer,
                wamn_identity::application::ApplicationTarget {
                    org: &triple.org, project: &triple.project, env: &triple.env,
                },
            ).await?;
            workload::grant(&mut *project_lock.client().await, &connections).await?;
            resources::prepare_cdc(&owner, &capture, &connection.database, &publication, &schemas).await?;
            resources::host_project(&platform, document, &connection).await?;
            let shared = name::environment_nats_certificate(&triple.org, &triple.env);
            wamn_kubernetes::kubectl::wait(&platform.target.kubeconfig, &platform.target.context,
                triple.org.as_str(), "certificate", &shared, "condition=Ready", "120s").await?;
            wamn_kubernetes::kubectl::wait(&platform.target.kubeconfig, &platform.target.context,
                triple.org.as_str(), "certificate", &certificate, "condition=Ready", "120s").await?;
            resources::ensure_stream(&platform, &triple).await?;
            super::cdc_reader::wait_reader_ready(&platform.target, &triple).await?;
            let mut connection = lock.client().await;
            let mut tx = provision_project_env::provisioning_transaction(&mut connection).await?;
            document.save(&mut tx).await?;
            tx.commit().await?;
            Ok(())
        }).await
    }).await
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
