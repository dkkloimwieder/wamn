//! Authorization reads under the project's configured database logins.

use std::sync::Arc;

use anyhow::Context as _;
use tracing::Instrument as _;
use wamn_identity_protocol::AuthorityClass;
use wamn_postgres::WamnPostgres;

#[derive(Debug)]
pub struct SessionIdentityError {
    pub type_: SessionIdentityErrorType,
    pub context: &'static str,
    pub source: anyhow::Error,
}

#[derive(Debug)]
pub enum SessionIdentityErrorType {
    Authorization,
    Infrastructure,
}

impl std::fmt::Display for SessionIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for SessionIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn principal_read_error(context: &'static str, source: sqlx::Error) -> SessionIdentityError {
    SessionIdentityError {
        type_: if source
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref()
            == Some("42501")
        {
            SessionIdentityErrorType::Authorization
        } else {
            SessionIdentityErrorType::Infrastructure
        },
        context,
        source: source.into(),
    }
}

/// Find the local principal for a verified issuer and subject.
pub(super) async fn resolve_subject(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    issuer: &str,
    subject: &str,
) -> anyhow::Result<Option<wamn_identity_protocol::PrincipalId>> {
    let (mut connection, _policy) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await?;
    let result = sqlx::query_scalar(
        "SELECT id FROM platform.\"user\" WHERE issuer = $1 AND sub = $2 AND status = 'active' AND type = 'user'",
    )
    .bind(issuer)
    .bind(subject)
    .fetch_optional(&mut *connection)
    .await;
    match result {
        Ok(principal) => Ok(principal),
        Err(error) => {
            postgres.destroy(connection);
            Err(principal_read_error("resolve the access token subject", error).into())
        }
    }
}

/// Read PAT admission directly on the project platform login.
pub(super) async fn read_project_pat(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    token: &wamn_identity_protocol::secret::PatToken,
) -> anyhow::Result<Option<Arc<super::ProjectPat>>> {
    let (mut connection, _) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await?;
    match super::project_pat(&mut connection, token).await {
        Ok(principal) => Ok(principal),
        Err(error) => {
            postgres.destroy(connection);
            Err(principal_read_error("read the project PAT", error).into())
        }
    }
}

/// Read the user row, roles and permissions after a cache miss.
pub(super) async fn read_platform_grants(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    project_env_key: &wamn_registry::ProjectEnvKey,
    principal: &wamn_identity_protocol::PrincipalId,
) -> anyhow::Result<wamn_authority::auth_cache::PrincipalGrants> {
    read_principal_grants(
        postgres,
        project,
        project_env_key,
        principal,
        AuthorityClass::Platform,
    )
    .await
}

async fn read_principal_grants(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    project_env_key: &wamn_registry::ProjectEnvKey,
    principal: &wamn_identity_protocol::PrincipalId,
    class: AuthorityClass,
) -> anyhow::Result<wamn_authority::auth_cache::PrincipalGrants> {
    let (mut connection, _policy) = match class {
        AuthorityClass::GuestSql => postgres.checkout_guest(project, project_env_key).await?,
        _ => postgres.checkout_platform(project, class).await?,
    };
    let result = super::principal_grants(&mut connection, principal)
        .instrument(tracing::info_span!("wamn.auth.perm.query"))
        .await;
    let grants = match result {
        Ok(grants) => grants,
        Err(error) => {
            postgres.destroy(connection);
            return Err(principal_read_error("read the principal grants", error).into());
        }
    };
    Ok(grants)
}

/// Whether the environment of `project_env_key` is inactive,
/// from its row of `platform.environment`, read as `wamn_app`
/// (docs/architecture/execution.md#routing-and-availability). No row means
/// active. The host keeps the answer in its [auth
/// memory](wamn_authority::auth_cache).
pub(super) async fn read_environment_inactive(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> anyhow::Result<bool> {
    let (mut connection, _pool) = postgres
        .checkout_guest(project, project_env_key)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let inactive = match super::environment_inactive(&mut connection).await {
        Ok(inactive) => inactive,
        Err(error) => {
            postgres.destroy(connection);
            return Err(error).context("read the environment status");
        }
    };
    Ok(inactive)
}

/// Read the grants used to check a workflow caller's role without consulting the cache.
pub async fn read_guest_grants(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    key: &wamn_registry::ProjectEnvKey,
    principal: &wamn_identity_protocol::PrincipalId,
) -> anyhow::Result<wamn_authority::auth_cache::PrincipalGrants> {
    read_principal_grants(postgres, project, key, principal, AuthorityClass::GuestSql).await
}

/// Check out the project's platform login for OAuth's own transaction.
pub async fn oauth_connection(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
    let (connection, _) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await?;
    Ok(connection)
}
