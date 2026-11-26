//! Writes to project principal rows.

use sqlx::types::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::platform_domain::PlatformDomain;

use wamn_authentication::identity_error::IdentityError;
use wamn_authentication::identity_error::database_error;

use crate::service::{ServiceError, ServiceErrorType};
use wamn_authentication::administration::AppUser;

use super::service::active_service;

const USER_ROW_SQL: &str = "INSERT INTO platform.\"user\" \
    (id, issuer, sub, type, email, display_name) VALUES ($1, $2, $3, 'user', $4, $5) \
    ON CONFLICT (id) DO UPDATE SET active = true \
    WHERE platform.\"user\".type = 'user' AND NOT platform.\"user\".active";

/// Insert or reactivate a projected user, preserving its identity and creation stamps.
pub async fn write_user(
    client: &mut sqlx::PgConnection,
    issuer: &str,
    user: AppUser<'_>,
) -> Result<(), IdentityError> {
    sqlx::query(USER_ROW_SQL)
        .bind(Uuid::from(*user.principal_id))
        .bind(issuer)
        .bind(user.principal_id.to_string())
        .bind(user.email)
        .bind(user.display_name)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Retain the user row for stamps and revoke its authority in the open transaction.
pub(super) async fn disable_project_user(
    client: &mut sqlx::PgConnection,
    principal_id: &PrincipalId,
) -> Result<(), IdentityError> {
    let principal = Uuid::from(*principal_id);
    let disabled = sqlx::query(
        r#"UPDATE platform."user" SET active = false
           WHERE id = $1 AND type = 'user' AND active"#,
    )
    .bind(principal)
    .execute(&mut *client)
    .await
    .map_err(|error| database_error(&error))?;
    if disabled.rows_affected() == 0 {
        return Ok(());
    }
    sqlx::query(
        "UPDATE platform.pat SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(principal)
    .execute(&mut *client)
    .await
    .map_err(|error| database_error(&error))?;
    sqlx::query("DELETE FROM platform.user_role WHERE user_id = $1")
        .bind(principal)
        .execute(client)
        .await
        .map_err(|error| database_error(&error))?;
    Ok(())
}

/// Write a project service principal, reusing its active row on a retry.
pub async fn write_service(
    project: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    domain: &PlatformDomain,
    subject: &str,
    display_name: &str,
) -> Result<PrincipalId, ServiceError> {
    let subject = subject.trim().to_ascii_lowercase();
    let display_name = display_name.trim();
    if subject.is_empty()
        || subject.len() > 254
        || !subject.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'.' | b'_' | b'+' | b'-'))
        })
    {
        return Err(ServiceError::refusal(
            ServiceErrorType::InvalidInput("subject"),
            "invalid service subject",
        ));
    }
    if display_name.is_empty() || display_name.len() > 200 || display_name.starts_with("wamn:") {
        return Err(ServiceError::refusal(
            ServiceErrorType::InvalidInput("display_name"),
            "invalid service display name",
        ));
    }
    let id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO platform."user" (id, issuer, sub, type, email, display_name)
           VALUES (gen_random_uuid(), 'wamn', $1, 'service', $2, $3)
           ON CONFLICT (issuer, sub) DO NOTHING RETURNING id"#,
    )
    .bind(&subject)
    .bind(format!("{subject}@{domain}"))
    .bind(display_name)
    .fetch_optional(&mut **project)
    .await
    .map_err(|source| ServiceError::database("write the project service principal", source))?;
    match id {
        Some(id) => Ok(PrincipalId::from(id)),
        None => active_service(project, &subject).await?.ok_or_else(|| {
            ServiceError::refusal(
                ServiceErrorType::NotFound("subject"),
                "project service principal is disabled or unavailable",
            )
        }),
    }
}

/// Disable a project service, retaining its PAT metadata.
pub async fn disable(
    project: &mut sqlx::PgConnection,
    principal: &PrincipalId,
) -> Result<(), ServiceError> {
    sqlx::query_scalar(
        r#"UPDATE platform."user" SET active=false
           WHERE id=$1 AND issuer='wamn' AND type='service' RETURNING id"#,
    )
    .bind(Uuid::from(*principal))
    .fetch_optional(project)
    .await
    .map_err(|source| ServiceError::database("disable the project service principal", source))?
    .map(|_: Uuid| ())
    .ok_or_else(|| {
        ServiceError::refusal(
            ServiceErrorType::NotFound("principal_id"),
            "project service principal does not exist",
        )
    })
}

/// Disable the services referenced by connections in the retained database.
pub async fn disable_connection_services(
    project: &mut sqlx::PgConnection,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"UPDATE platform."user" u SET active=false
           FROM platform.connection c
           WHERE u.id=c.service_principal_id AND u.issuer='wamn'
             AND u.type='service' AND u.active"#,
    )
    .execute(project)
    .await?;
    Ok(())
}
