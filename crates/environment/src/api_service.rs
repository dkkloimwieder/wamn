//! Create an API service identity, grant authored roles, and issue its PAT.

use anyhow::ensure;
use std::time::Duration;
use wamn_identity_protocol::wire::PatResponse;
use wamn_registry::{ProjectEnvKey, platform_domain::PlatformDomain};
use wamn_role::RoleName;

use wamn_authentication::administration::{GrantAuthority, GrantChange};
use wamn_postgres::administration::mutate_grant;
use wamn_postgres::identity::application::service;

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
pub async fn create(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    key: &ProjectEnvKey,
    domain: &PlatformDomain,
    request: &Request<'_>,
) -> anyhow::Result<PatResponse> {
    ensure!(
        request.name.parse::<wamn_registry::ProjectId>().is_ok(),
        "invalid API service name"
    );
    ensure!(
        !request.roles.is_empty(),
        "an API service requires an authored role"
    );
    wamn_postgres::operation::lock_operation_grants(transaction, key).await?;
    let subject = format!("api.{}.{}.{}", request.org, request.project, request.name);
    let principal =
        service::write_service(transaction, domain, &subject, request.display_name).await?;
    for role in request.roles {
        mutate_grant(
            transaction,
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
    Ok(service::issue(transaction, &principal, request.name, request.lifetime).await?)
}
