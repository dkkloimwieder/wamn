//! Service principals and PAT administration in a project database.

use serde::Serialize;
use sqlx::types::Uuid;
use std::{error::Error, fmt, time::Duration};
use wamn_identity_protocol::{
    PrincipalId, pat_prefix::PatPrefix, secret::PatToken, wire::PatResponse,
};
use wamn_registry::platform_domain::PlatformDomain;

pub use super::user::{disable, write_service};

const MAX_PAT_TTL: Duration = Duration::from_hours(365 * 24);

/// Classification of a project service error at the control boundary.
#[derive(Debug, Clone, Copy)]
pub enum ServiceErrorType {
    InvalidInput(&'static str),
    NotFound(&'static str),
    DomainUnset,
    Database,
    Entropy,
}

/// A failed service operation with its original error and context.
#[derive(Debug)]
pub struct ServiceError {
    pub type_: ServiceErrorType,
    context: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ServiceError {
    pub(super) fn refusal(type_: ServiceErrorType, context: &'static str) -> Self {
        Self {
            type_,
            context,
            source: None,
        }
    }

    pub(super) fn database(context: &'static str, source: sqlx::Error) -> Self {
        Self {
            type_: ServiceErrorType::Database,
            context,
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context)
    }
}

impl Error for ServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Read the platform domain from the system database.
pub async fn platform_domain(
    system: &mut sqlx::PgConnection,
) -> Result<PlatformDomain, ServiceError> {
    sqlx::query_scalar::<_, Option<PlatformDomain>>(wamn_registry::sql::PLATFORM_DOMAIN_SQL)
        .fetch_one(system)
        .await
        .map_err(|source| ServiceError::database("read the platform domain", source))?
        .ok_or_else(|| {
            ServiceError::refusal(
                ServiceErrorType::DomainUnset,
                "platform-domain-unset: service principals need the platform domain",
            )
        })
}

/// Resolve an active project service by its local subject.
pub async fn active_service(
    project: &mut sqlx::PgConnection,
    subject: &str,
) -> Result<Option<PrincipalId>, ServiceError> {
    sqlx::query_scalar::<_, Uuid>(
        r#"SELECT id FROM platform."user"
           WHERE issuer='wamn' AND sub=$1 AND type='service' AND status='active'
           FOR UPDATE"#,
    )
    .bind(subject)
    .fetch_optional(project)
    .await
    .map(|id| id.map(PrincipalId::from))
    .map_err(|source| {
        ServiceError::database("resolve the active project service principal", source)
    })
}

/// The injected project writer used when creating a connection.
pub async fn write_connection_service(
    system: &mut sqlx::PgConnection,
    project: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    subject: &str,
    display_name: &str,
) -> anyhow::Result<PrincipalId> {
    let domain = platform_domain(system).await?;
    Ok(write_service(project, &domain, subject, display_name).await?)
}

/// Issue a token for an active service in the current project transaction.
pub async fn issue(
    project: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &PrincipalId,
    label: &str,
    lifetime: Duration,
) -> Result<PatResponse, ServiceError> {
    let label = label.trim();
    if label.is_empty() || label.len() > 200 {
        return Err(ServiceError::refusal(
            ServiceErrorType::InvalidInput("label"),
            "invalid PAT label",
        ));
    }
    if lifetime.as_secs() == 0 || lifetime > MAX_PAT_TTL {
        return Err(ServiceError::refusal(
            ServiceErrorType::InvalidInput("lifetime_seconds"),
            "invalid PAT lifetime",
        ));
    }
    let token = PatToken::mint().map_err(|source| ServiceError {
        type_: ServiceErrorType::Entropy,
        context: "mint the project PAT",
        source: Some(Box::new(source)),
    })?;
    let (created_at, expires_at) = sqlx::query_as(
        r#"INSERT INTO platform.pat (user_id, prefix, hash, label, expires_at)
           SELECT id, $2, $3, $4, now() + ($5::bigint * interval '1 second')
           FROM platform."user"
           WHERE id=$1 AND issuer='wamn' AND type='service' AND status='active'
           FOR UPDATE
           RETURNING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
                     to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')"#,
    )
    .bind(Uuid::from(*principal))
    .bind(token.lookup())
    .bind(token.digest().to_string())
    .bind(label)
    .bind(lifetime.as_secs().cast_signed())
    .fetch_optional(&mut **project)
    .await
    .map_err(|source| ServiceError::database("store the project PAT", source))?
    .ok_or_else(|| {
        ServiceError::refusal(
            ServiceErrorType::NotFound("principal_id"),
            "project service principal does not exist or is disabled",
        )
    })?;
    let token_prefix = *token.lookup();
    let mut token = token.into_secret();
    Ok(PatResponse {
        token_prefix,
        token: std::mem::take(&mut *token),
        principal_id: *principal,
        created_at,
        expires_at,
    })
}

/// Public metadata of a stored token.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PatRecord {
    id: sqlx::types::Uuid,
    token_prefix: PatPrefix,
    label: String,
    created_at: String,
    expires_at: String,
    revoked_at: Option<String>,
}

/// List token metadata, newest first.
pub async fn list(
    project: &mut sqlx::PgConnection,
    principal: &PrincipalId,
) -> Result<Vec<PatRecord>, ServiceError> {
    sqlx::query_as(
        r#"SELECT p.id, p.prefix AS token_prefix, p.label,
                  to_char(p.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS created_at,
                  to_char(p.expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS expires_at,
                  to_char(p.revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS revoked_at
           FROM platform.pat p JOIN platform."user" u ON p.user_id=u.id
           WHERE u.id=$1 AND u.issuer='wamn' AND u.type='service'
           ORDER BY p.created_at DESC, p.id"#,
    )
    .bind(Uuid::from(*principal))
    .fetch_all(project)
    .await
    .map_err(|source| ServiceError::database("list the project's service PATs", source))
}

/// Revoke a token in this project database, retaining the first revocation stamp.
pub async fn revoke(
    project: &mut sqlx::PgConnection,
    prefix: &PatPrefix,
) -> Result<(), ServiceError> {
    sqlx::query_scalar(
        r#"UPDATE platform.pat p SET revoked_at=COALESCE(p.revoked_at, now())
           FROM platform."user" u
           WHERE p.prefix=$1 AND p.user_id=u.id AND u.issuer='wamn' AND u.type='service'
           RETURNING p.id"#,
    )
    .bind(prefix)
    .fetch_optional(project)
    .await
    .map_err(|source| ServiceError::database("revoke the project PAT", source))?
    .map(|_: sqlx::types::Uuid| ())
    .ok_or_else(|| {
        ServiceError::refusal(
            ServiceErrorType::NotFound("token_prefix"),
            "project PAT does not exist",
        )
    })
}
