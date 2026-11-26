//! Authorization reads under the project's configured database logins.

use std::sync::Arc;

use super::{ProjectPat, SessionIdentityError, SessionIdentityErrorType};
use anyhow::Context as _;
use sqlx::Row as _;
use tracing::Instrument as _;
use wamn_application::database::SqlError;
use wamn_authority::auth_cache::PrincipalGrants;
use wamn_identity_protocol::secret::PatToken;
use wamn_identity_protocol::{AuthorityClass, PrincipalId};

use wamn_postgres::WamnPostgres;

/// The status row of the environment, which `wamn_app` reads under its
/// database boundary.
const ENVIRONMENT_INACTIVE_SQL: &str = "SELECT NOT active FROM platform.env";

/// The user row of one principal, each role it holds and each permission
/// of that role, in one read. No row means no user row. A role without
/// permissions, such as `admin`, yields one row with a null permission, and a
/// principal without roles yields one row with a null role.
const PRINCIPAL_GRANTS_SQL: &str = "SELECT u.active, r.role_name, p.permission, u.type = 'user' \
    FROM platform.\"user\" u LEFT JOIN platform.user_role r \
    ON r.user_id = u.id \
    LEFT JOIN platform.permission p \
    ON p.role_name = r.role_name \
    WHERE u.id = $1";

fn principal_grants_from_rows(
    rows: &[sqlx::postgres::PgRow],
) -> Result<wamn_authority::auth_cache::PrincipalGrants, sqlx::Error> {
    let mut grants = wamn_authority::auth_cache::PrincipalGrants {
        present: !rows.is_empty(),
        ..Default::default()
    };
    for row in rows {
        grants.active = row.try_get(0)?;
        grants.user = row.try_get(3)?;
        if let Some(role) = row.try_get::<Option<String>, _>(1)? {
            let permissions = grants.roles.entry(role).or_default();
            permissions.extend(row.try_get::<Option<String>, _>(2)?);
        }
    }
    Ok(grants)
}

/// Read the current user, roles and permissions without choosing the transaction identity.
async fn principal_grants(
    connection: &mut sqlx::PgConnection,
    user_id: &wamn_identity_protocol::PrincipalId,
) -> Result<PrincipalGrants, sqlx::Error> {
    let rows = sqlx::query(PRINCIPAL_GRANTS_SQL)
        .bind(uuid::Uuid::from(*user_id))
        .fetch_all(connection)
        .await?;
    principal_grants_from_rows(&rows)
}

/// An absent environment row remains active.
async fn environment_inactive(connection: &mut sqlx::PgConnection) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar(ENVIRONMENT_INACTIVE_SQL)
        .fetch_optional(connection)
        .await?
        .unwrap_or(false))
}

/// Check one PAT on the host's platform login.
async fn project_pat(
    project: &mut sqlx::PgConnection,
    token: &PatToken,
) -> Result<Option<Arc<ProjectPat>>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT u.id, u.sub, p.hash, p.expires_at
           FROM platform.pat p JOIN platform."user" u ON u.id=p.user_id
           WHERE p.prefix=$1 AND p.revoked_at IS NULL AND p.expires_at > now()
             AND u.issuer='wamn' AND u.type='service' AND u.active"#,
    )
    .bind(token.lookup().to_string())
    .fetch_optional(project)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let hash =
        row.try_get::<&str, _>("hash")?
            .parse()
            .map_err(|source| sqlx::Error::ColumnDecode {
                index: "hash".into(),
                source: Box::new(source),
            })?;
    if !token.matches(&hash) {
        return Ok(None);
    }
    let deadline: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
        row.try_get("expires_at")?;
    Ok(Some(Arc::new(ProjectPat {
        principal: PrincipalId::from(row.try_get::<uuid::Uuid, _>("id")?),
        subject: row.try_get::<&str, _>("sub")?.into(),
        deadline: deadline.into(),
    })))
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
    let result = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM platform.\"user\" WHERE issuer = $1 AND sub = $2 AND active AND type = 'user'",
    )
    .bind(issuer)
    .bind(subject)
    .fetch_optional(&mut *connection)
    .await;
    match result {
        Ok(principal) => Ok(principal.map(wamn_identity_protocol::PrincipalId::from)),
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
) -> anyhow::Result<Option<Arc<ProjectPat>>> {
    let (mut connection, _) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await?;
    match project_pat(&mut connection, token).await {
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
        AuthorityClass::GuestSql => postgres
            .checkout_guest(project, project_env_key)
            .await
            .context(SqlError::ConnectionUnavailable)?,
        _ => postgres.checkout_platform(project, class).await?,
    };
    let result = principal_grants(&mut connection, principal)
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
/// from its row of `platform.env`, read as `wamn_app`
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
    let inactive = match environment_inactive(&mut connection).await {
        Ok(inactive) => inactive,
        Err(error) => {
            postgres.destroy(connection);
            return Err(error).context("read the environment status");
        }
    };
    Ok(inactive)
}

/// Read the grants used to check a workflow caller's role without consulting the cache.
pub(super) async fn read_guest_grants(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    key: &wamn_registry::ProjectEnvKey,
    principal: &wamn_identity_protocol::PrincipalId,
) -> anyhow::Result<wamn_authority::auth_cache::PrincipalGrants> {
    read_principal_grants(postgres, project, key, principal, AuthorityClass::GuestSql).await
}
