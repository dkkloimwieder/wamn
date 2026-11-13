//! Create an API service identity, grant authored roles, and issue its PAT.

use anyhow::{Context as _, ensure};
use sqlx::{Connection as _, Row as _};
use std::path::PathBuf;
use std::time::Duration;
use wamn_broker::{Publisher, auth_changes::AuthDrops};
use wamn_identity_client::{PatClient, PatIssuerConfig};
use wamn_identity_protocol::{PrincipalId, RoleName};
use wamn_platform_identity::PrincipalStatus;
use wamn_platform_identity::application::{
    ApplicationUser, GrantAuthority, GrantChange, mutate_grant, write_service,
};

/// Explicit authorities and authored grants for one API service.
/// Database credentials never appear in diagnostics.
pub struct Request {
    pub org: String,
    pub project: String,
    pub env: String,
    pub name: String,
    pub display_name: String,
    pub roles: Vec<RoleName>,
    pub lifetime: Duration,
    pub system_database_url: String,
    pub application_database_url: String,
    pub identity: PatIssuerConfig,
    pub emit_secret: PathBuf,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request").finish_non_exhaustive()
    }
}

/// Create the service and issue its PAT through the existing identity authority.
/// Retrying a partial creation reuses its identity and grants.
pub async fn create(request: &Request, changes: &Publisher) -> anyhow::Result<PrincipalId> {
    wamn_control_provision::validate_project_env(&request.org, &request.project, &request.env)?;
    ensure!(
        request.name.parse::<wamn_registry::ProjectId>().is_ok(),
        "invalid API service name"
    );
    ensure!(
        !request.roles.is_empty(),
        "an API service requires an authored role"
    );
    ensure!(
        request.lifetime > Duration::ZERO
            && request.lifetime <= wamn_platform_identity::MAX_PAT_TTL,
        "invalid PAT lifetime"
    );
    crate::provision_project_env::ensure_secret_path(&request.emit_secret, "API service PAT")?;
    let issuer = PatClient::new(&request.identity)?;
    let mut system = wamn_postgres::connect(&request.system_database_url)
        .await
        .map_err(|_| anyhow::anyhow!("connect to the system database for API service creation"))?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut system)
        .await
        .context("select the API service creation authority")?;
    let deployment = sqlx::query(sqlx::AssertSqlSafe(
        wamn_registry::sql::select_project_env_sql(),
    ))
    .bind(&request.org)
    .bind(&request.project)
    .bind(&request.env)
    .fetch_optional(&mut system)
    .await?
    .context("the project deployment does not exist")?;
    let suffix: String = deployment.try_get("instance_suffix")?;
    let expected_database = wamn_registry::name::project_env_database_name(
        &request.org,
        &request.project,
        &request.env,
        &suffix,
    );
    let (_, database) = wamn_postgres::identity(&request.application_database_url)
        .map_err(|_| anyhow::anyhow!("invalid application database configuration"))?;
    ensure!(
        database == expected_database,
        "API service database differs from the project deployment"
    );
    let mut application = wamn_postgres::connect(&request.application_database_url)
        .await
        .map_err(|_| anyhow::anyhow!("connect to the API service application database"))?;
    let subject = format!("api.{}.{}.{}", request.org, request.project, request.name);
    let principal = crate::provision_project_env::resolve_or_create_service(
        &mut system,
        &subject,
        &request.display_name,
        changes,
    )
    .await?;
    ensure!(
        principal.status() == PrincipalStatus::Active,
        "API service is disabled"
    );
    let key = wamn_registry::project_env_key(&request.org, &request.project, &request.env);
    let mut tx = application.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *tx)
    .await?;
    let mut drops = AuthDrops::default();
    // Existing service rows and grants stay. This makes a retry after an issuer failure safe.
    let principal_id = principal.id().to_string();
    let present: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM platform.\"user\" WHERE id=$1::text::uuid AND type='service')")
        .bind(&principal_id).fetch_one(&mut *tx).await?.get(0);
    if !present {
        write_service(
            &mut tx,
            &key,
            ApplicationUser {
                principal_id: principal.id(),
                email: &subject,
                display_name: principal.display_name(),
            },
            &mut drops,
        )
        .await?;
    }
    for role in &request.roles {
        mutate_grant(
            &mut tx,
            &key,
            GrantAuthority::Provisioning,
            GrantChange::UserRole {
                user_id: &principal_id,
                role: role.as_str(),
                grant: true,
            },
            &mut drops,
        )
        .await?;
    }
    tx.commit().await?;
    changes.publish(drops).await;
    let token = issuer
        .issue(principal.id(), &request.name, request.lifetime)
        .await?;
    crate::provision_project_env::write_secret_json(
        &request.emit_secret,
        &serde_json::to_value(&token)?,
    )?;
    Ok(*principal.id())
}
