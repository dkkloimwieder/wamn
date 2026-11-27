//! Create an API service identity, grant authored roles, and issue its PAT.

use anyhow::ensure;
use sqlx::{Postgres, Transaction};
use std::time::Duration;
use wamn_identity_protocol::wire::PatResponse;
use wamn_registry::{ProjectEnvKey, platform_domain::PlatformDomain};
use wamn_role::RoleName;

use crate::identity::app::service;
use crate::service::ServiceError;
use wamn_authentication::administration::{self, GrantAuthority, GrantChange};

/// Authored grants and credential lifetime for an API service.
#[derive(Debug)]
pub struct Request<'a> {
    pub org: &'a wamn_registry::OrgId,
    pub project: &'a wamn_registry::ProjectId,
    pub name: &'a str,
    pub display_name: &'a str,
    pub roles: &'a [RoleName],
    pub lifetime: Duration,
}

/// Write the service row, grants, and PAT in one project transaction.
///
/// # Errors
/// Returns an error for an invalid name, missing or inactive roles, or a refused project write.
pub async fn create(
    tx: &mut Transaction<'_, Postgres>,
    key: &ProjectEnvKey,
    domain: &PlatformDomain,
    request: &Request<'_>,
) -> anyhow::Result<PatResponse> {
    ensure!(
        wamn_registry::identifiers::api_service_name_is_valid(request.name),
        "invalid API service name"
    );
    ensure!(
        !request.roles.is_empty(),
        "an API service requires an authored role"
    );
    wamn_postgres::operation_grant_lock::lock_operation_grants(tx, key)
        .await
        .map_err(|source| ServiceError::database("lock project operation grants", source))?;
    let subject = format!("api.{}.{}.{}", request.org, request.project, request.name);
    let principal = service::write_service(tx, domain, &subject, request.display_name).await?;
    for role in request.roles {
        administration::mutate_grant(
            tx,
            key,
            GrantAuthority::Provisioning,
            GrantChange::UserRole {
                user_id: &principal,
                role: role.as_str(),
                grant: true,
            },
        )
        .await?;
    }
    Ok(service::issue(tx, &principal, request.name, request.lifetime).await?)
}
