//! Service principals and PAT administration in a project database.

use sqlx::FromRow as _;
use sqlx::types::Uuid;
use std::time::Duration;
use wamn_authentication::administration::{GrantAuthority, GrantChange, GrantError};
use wamn_authentication::api_service::ApiServiceStore;
use wamn_authentication::service::{ServiceError, ServiceErrorType};
use wamn_identity_protocol::records::PatRecord;
use wamn_identity_protocol::{
    PrincipalId, pat_prefix::PatPrefix, secret::PatToken, wire::PatResponse,
};
use wamn_registry::{ProjectEnvKey, platform_domain::PlatformDomain};

pub use super::user::{disable, write_service};

const MAX_PAT_TTL: Duration = Duration::from_hours(365 * 24);

struct ApiServiceTransaction<'transaction, 'connection>(
    &'transaction mut sqlx::Transaction<'connection, sqlx::Postgres>,
);

/// Adapt the caller's prepared transaction for API service creation.
pub fn api_service_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> impl ApiServiceStore {
    ApiServiceTransaction(transaction)
}

#[async_trait::async_trait]
impl ApiServiceStore for ApiServiceTransaction<'_, '_> {
    async fn lock_grants(&mut self, key: &ProjectEnvKey) -> Result<(), ServiceError> {
        crate::operation::lock_operation_grants(self.0, key)
            .await
            .map_err(|source| ServiceError::database("lock project operation grants", source))
    }

    async fn write_service(
        &mut self,
        domain: &PlatformDomain,
        subject: &str,
        display_name: &str,
    ) -> Result<PrincipalId, ServiceError> {
        write_service(self.0, domain, subject, display_name).await
    }

    async fn grant_role(
        &mut self,
        key: &ProjectEnvKey,
        principal: &PrincipalId,
        role: &wamn_role::RoleName,
    ) -> Result<(), GrantError> {
        crate::administration::mutate_grant(
            self.0,
            key,
            GrantAuthority::Provisioning,
            GrantChange::UserRole {
                user_id: principal,
                role: role.as_str(),
                grant: true,
            },
        )
        .await
        .map(drop)
    }

    async fn issue_pat(
        &mut self,
        principal: &PrincipalId,
        label: &str,
        lifetime: Duration,
    ) -> Result<PatResponse, ServiceError> {
        issue(self.0, principal, label, lifetime).await
    }
}

/// Read the platform domain from the system database.
pub async fn platform_domain(
    system: &mut sqlx::PgConnection,
) -> Result<PlatformDomain, ServiceError> {
    sqlx::query_scalar::<_, Option<String>>(crate::control::PLATFORM_DOMAIN_SQL)
        .fetch_one(system)
        .await
        .and_then(|domain| {
            domain
                .map(|domain| crate::decode_text(domain, 0))
                .transpose()
        })
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
    let token = PatToken::mint().map_err(|source| {
        ServiceError::with_source(ServiceErrorType::Entropy, "mint the project PAT", source)
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
    .bind(token.lookup().to_string())
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

#[derive(sqlx::FromRow)]
struct PatRow {
    id: Uuid,
    token_prefix: String,
    label: String,
    created_at: String,
    expires_at: String,
    revoked_at: Option<String>,
}

impl PatRow {
    fn into_record(self) -> Result<PatRecord, sqlx::Error> {
        let Self {
            id,
            token_prefix,
            label,
            created_at,
            expires_at,
            revoked_at,
        } = self;
        let token_prefix =
            PatPrefix::try_from(token_prefix).map_err(|source| sqlx::Error::ColumnDecode {
                index: "token_prefix".into(),
                source: Box::new(source),
            })?;
        Ok(PatRecord::new(
            id,
            token_prefix,
            label,
            created_at,
            expires_at,
            revoked_at,
        ))
    }
}

/// List token metadata, newest first.
pub async fn list(
    project: &mut sqlx::PgConnection,
    principal: &PrincipalId,
) -> Result<Vec<PatRecord>, ServiceError> {
    sqlx::query(
        r#"SELECT p.id, p.prefix AS token_prefix, p.label,
                  to_char(p.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS created_at,
                  to_char(p.expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS expires_at,
                  to_char(p.revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS revoked_at
           FROM platform.pat p JOIN platform."user" u ON p.user_id=u.id
           WHERE u.id=$1 AND u.issuer='wamn' AND u.type='service'
           ORDER BY p.created_at DESC, p.id"#,
    )
    .bind(Uuid::from(*principal))
    .try_map(|row| PatRow::from_row(&row)?.into_record())
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
    .bind(prefix.to_string())
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
