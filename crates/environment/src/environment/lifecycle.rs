//! Authenticated environment configuration through installer-owned resources.
use super::{Platform, document::EnvironmentDocument, ensure as resources};
use crate::provision_project_env::{self, ProvisionProjectEnvRequest};
use anyhow::ensure;
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
        let mut client = wamn_postgres::connect(&owner).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(provision_project_env::privilege_sql(
            &connection.database,
        )))
        .execute(&mut client)
        .await?;
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
        wamn_platform_install::environment::apply(
            &platform.target,
            &platform.chart.join("platform/environment"),
            &name,
            serde_json::json!({"bootstrap":null}),
        )
        .await?;
        crate::environment::fill(
            system_owner,
            &owner,
            &platform.identity_issuer,
            crate::application::ApplicationTarget {
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
        resources::host_project(&platform, document, &connection).await?;
        let capture =
            wamn_platform_install::environment::capture_connection(system_owner, &triple).await?;
        resources::prepare_cdc_with_connection(&platform, document, &capture).await?;
        let certificates = wamn_broker::authorization::update_authorization(
            &platform.target,
            &document.org,
            Some(&triple),
        )
        .await?;
        let name = wamn_provision::environment_names::environment_resource_name(
            &triple.org,
            &triple.project,
            &triple.env,
        )?;
        wamn_platform_install::environment::apply(
            &platform.target,
            &platform.chart.join("platform/environment"),
            &name,
            wamn_platform_install::environment::broker_certificates(certificates),
        )
        .await?;
        resources::ensure_streams(&platform, document, &[]).await?;
        let mut connection = lock.client().await;
        let mut tx = provision_project_env::provisioning_transaction(&mut connection).await?;
        document.save(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    })
    .await?;
    Ok(())
}
