//! Authenticated environment configuration through installer-owned resources.
use super::{Platform, document::EnvironmentDocument, ensure as resources, lock::LifecycleLock};
use crate::provision_project_env::{self, ProvisionProjectEnvRequest};
use anyhow::{Context as _, ensure};

pub async fn configure(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: Option<&str>,
    disposable: Option<bool>,
    system_owner: &str,
    actor: &str,
) -> anyhow::Result<()> {
    if let Some(application) = application {
        ensure!(
            document.applications.contains_key(application),
            "application is not declared"
        );
    }
    let mut platform = platform.clone();
    let installed = super::runtime::resolve_platform(&mut platform, &document.org).await?;
    let triple = document.triple();
    LifecycleLock::hold(system_owner, &triple, async |lock| {
        let outcome = provision_project_env::provision_project_env(&ProvisionProjectEnvRequest {
            changes: platform.auth_changes().await?,
            org: triple.org.clone(),
            project: triple.project.clone(),
            env: triple.env.as_str().into(),
            policy: Some(document.policy.clone()),
            target: document.target.clone(),
            disposable,
            system_database_url: Some(system_owner.into()),
            cluster: None,
            connection_limit: None,
            cluster_namespace: triple.org.clone(),
            namespace: triple.org.clone(),
            secret_namespace: Some(triple.org.clone()),
            emit_database: Some("-".into()),
            emit_role_sql: None,
            emit_privilege_sql: None,
            emit_secret: None,
            pat_issuer: platform.pat_issuer.clone(),
            emit_management_author_pat_secret: None,
            emit_operator_pat_secret: None,
        })
        .await?;
        let unconfigured:bool=sqlx::query_scalar("SELECT configuration IS NULL FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
            .bind(&triple.org).bind(&triple.project).bind(triple.env.as_str()).fetch_one(&mut *lock.client().await).await?;
        let connection = wamn_platform_install::environment::provision(
            &platform.target,
            &platform.chart,
            system_owner,
            &triple,
            unconfigured,
        )
        .await?;
        let owner = wamn_control_provision::migrations::MigrationDatabase::Project
            .configuration(&connection.to_json()?)?;
        let mut client = wamn_postgres::connect(&owner).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(provision_project_env::privilege_sql(
            &connection.database,
        )))
        .execute(&mut client)
        .await?;
        let image=crate::platform::artifact_plan::select(&platform.chart)?.image("control")?.to_owned();
        wamn_platform_install::environment::initialize(&platform.target,&image,&connection,unconfigured).await?;
        if unconfigured {
            crate::platform_migrations::restore_project(
                &owner,
                &crate::platform_migrations::embedded_sources(
                    wamn_control_provision::migrations::MigrationDatabase::Project,
                ),
            )
            .await?;
        }
        let name = wamn_control_provision::environment_names::environment_resource_name(
            &triple.org, &triple.project, &triple.env)?;
        wamn_platform_install::environment::apply(&platform.target,
            &platform.chart.join("platform/environment"), &name,
            serde_json::json!({"bootstrap":null})).await?;
        let key = wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env);
        wamn_platform_identity::environment::fill(
            system_owner,
            &owner,
            wamn_platform_identity::application::ApplicationTarget {
                org: &triple.org,
                project: &triple.project,
                env: &triple.env,
                project_env_key: &key,
            },
            &platform.auth_changes().await?,
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
            wamn_platform_install::environment::capture_connection(system_owner, &triple).await?;
        resources::prepare_cdc_with_connection(&platform, document, &capture).await?;
        let certificates = wamn_broker::authorization::update_authorization(
            &platform.target,
            &document.org,
            Some(&triple),
        )
        .await?;
        let name = wamn_control_provision::environment_names::environment_resource_name(
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
        resources::provision_materializer(&platform, &installed, document).await?;
        let mut connection = lock.client().await;
        let mut tx = provision_project_env::provisioning_transaction(&mut connection).await?;
        document.save(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    })
    .await?;
    if let Some(application) = application {
        let serving = EnvironmentDocument::read_serving(&platform, &triple).await?;
        if let super::document::DeclaredRelease::Digest(digest) = &serving
            .applications
            .get(application)
            .context("configured application")?
            .current
        {
            super::apply_selection(
                &platform,
                &triple,
                application,
                super::document::ReleaseChange::Deploy(digest),
                Some(actor),
            )
            .await?;
        }
    }
    Ok(())
}
