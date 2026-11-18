//! Authenticated environment configuration through installer-owned resources.
use super::{Platform, document::EnvironmentDocument, ensure as resources};
use crate::provision_project_env::{self, ProvisionProjectEnvRequest};
use anyhow::ensure;
use serde_json::json;
use wamn_registry::{Triple, name};
use wamn_catalog::lifecycle_lock::LifecycleLock;

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
    super::runtime::resolve_platform(&mut platform, &document.org).await?;
    let triple = document.triple();
    let client = wamn_postgres::connect(system_owner).await?;
    LifecycleLock::hold(client, &triple, async |lock| {
        wamn_catalog::configuration::require_placement(
            &mut *lock.client().await,
            &document.org,
            &document.env,
            &document.policy,
        )
        .await?;
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
            emit_database: Some("-".into()),
            emit_role_sql: None,
            emit_privilege_sql: None,
            emit_secret: None,
        })
        .await?;
        let unconfigured = super::document::read_configuration(&mut *lock.client().await, &triple)
            .await?
            .is_none();
        let connection = wamn_platform_install::environment::provision(
            &platform.target,
            &platform.chart,
            system_owner,
            &triple,
            unconfigured,
        )
        .await?;
        let owner = wamn_provision::migrations::MigrationDatabase::Project
            .configuration(&connection.to_json()?)?;
        LifecycleLock::hold(
            wamn_postgres::connect(&owner).await?,
            &triple,
            async |project_lock| {
                let mut client = project_lock.client().await;
                if !unconfigured {
                    let unfinished =
                        super::drain::unfinished(&mut client, &triple.env, application).await?;
                    ensure!(
                        unfinished.is_empty(),
                        "{}",
                        super::drain::describe(&unfinished)
                    );
                }
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
                let name = wamn_provision::environment_names::environment_resource_name(
                    &triple.org,
                    &triple.project,
                    &triple.env,
                )?;
                let mut values = wamn_platform_install::platform::operations::values(
                    &triple,
                    &outcome.cluster,
                    &connection.database,
                )?;
                values["bootstrap"] = serde_json::Value::Null;
                wamn_platform_install::environment::apply(
                    &platform.target,
                    &platform.chart.join("platform/environment"),
                    &name,
                    values,
                )
                .await?;
                crate::environment::fill(
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
                crate::provision_project_env::workload::provision(
                    &platform,
                    document,
                    &outcome.cluster,
                    &owner,
                    system_owner,
                )
                .await?;
                let capture =
                    wamn_platform_install::environment::capture_connection(system_owner, &triple)
                        .await?;
                let publication = resources::prepare_cdc(
                    &platform, document, &outcome, &connection.database, &owner, &capture,
                ).await?;
                provision_event_certificates(&platform, &triple, &name).await?;
                resources::ensure_stream(&platform, &triple).await?;
                deploy_reader(&platform, &triple, &name, &publication, &capture).await?;
                resources::host_project(&platform, document, &connection).await?;
                let mut connection = lock.client().await;
                let mut tx =
                    provision_project_env::provisioning_transaction(&mut connection).await?;
                document.save(&mut tx).await?;
                tx.commit().await?;
                Ok(())
            },
        )
        .await
    })
    .await?;
    Ok(())
}

/// Put the shared Certificate on the host release and the reader Certificate on the project release.
async fn provision_event_certificates(
    platform: &Platform,
    triple: &Triple,
    project_release: &str,
) -> anyhow::Result<()> {
    use wamn_broker::authorization::{certificate, provision_project};

    let chart = platform.chart.join("platform/environment");
    let (org, env) = (&triple.org, &triple.env);
    let host = name::environment_host_name(org, env);
    let shared_certificate = name::environment_nats_certificate(org, env);
    let shared_secret = name::environment_nats_secret(org, env);
    let reader_certificate = name::cdc_nats_certificate(triple);
    provision_project(&platform.target, triple).await?;
    for (release, name, specification) in [
        (host.as_str(), shared_certificate.as_str(), certificate(
            &shared_certificate, &shared_secret, &name::environment_nats_user(org, env),
        )),
        (project_release, reader_certificate.as_str(), certificate(
            &reader_certificate, &reader_certificate, &name::cdc_nats_user(triple),
        )),
    ] {
        wamn_platform_install::environment::apply(
            &platform.target, &chart, release, json!({"brokerCertificates":[specification]}),
        ).await?;
        wamn_kubernetes::kubectl::wait(
            &platform.target.kubeconfig, &platform.target.context, org.as_str(),
            "certificate", name, "condition=Ready", "120s",
        ).await?;
    }
    Ok(())
}

async fn deploy_reader(
    platform: &Platform,
    triple: &Triple,
    release: &str,
    publication: &str,
    capture: &str,
) -> anyhow::Result<()> {
    let (org, project, env) = (&triple.org, &triple.project, &triple.env);
    let image = wamn_platform_install::release_chart::image_set(&platform.chart).await?.host;
    let config = super::cdc_reader::reader_configuration(
        triple, publication, wamn_platform_install::platform::identity::NATS_URL,
    );
    let name = wamn_provision::project_env_cdc_secret_name(org, project, env);
    wamn_platform_install::environment::apply(
        &platform.target,
        &platform.chart.join("platform/environment"),
        release,
        json!({"cdcReader": {
            "name": name,
            "key": name::project_env_key(org, project, env),
            "coordinate": triple.to_string(),
            "image": image,
            "registrySecret": (!platform.registry_token_metadata).then_some(name::REGISTRY_PULL),
            "natsSecret": name::cdc_nats_certificate(triple),
            "connection": capture,
            "configuration": config,
        }}),
    ).await
}
