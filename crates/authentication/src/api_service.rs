//! Create an API service identity, grant authored roles, and issue its PAT.

use anyhow::ensure;
use std::time::Duration;
use wamn_identity_protocol::{PrincipalId, wire::PatResponse};
use wamn_registry::{ProjectEnvKey, platform_domain::PlatformDomain};
use wamn_role::RoleName;

use crate::{administration::GrantError, service::ServiceError};

/// Project writes within one transaction prepared by the caller.
#[async_trait::async_trait]
pub trait ApiServiceStore: Send {
    async fn lock_grants(&mut self, key: &ProjectEnvKey) -> Result<(), ServiceError>;

    async fn write_service(
        &mut self,
        domain: &PlatformDomain,
        subject: &str,
        display_name: &str,
    ) -> Result<PrincipalId, ServiceError>;

    async fn grant_role(
        &mut self,
        key: &ProjectEnvKey,
        principal: &PrincipalId,
        role: &RoleName,
    ) -> Result<(), GrantError>;

    async fn issue_pat(
        &mut self,
        principal: &PrincipalId,
        label: &str,
        lifetime: Duration,
    ) -> Result<PatResponse, ServiceError>;
}

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
/// Returns an error for an invalid name, missing roles, or a refused project write.
pub async fn create(
    store: &mut impl ApiServiceStore,
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
    store.lock_grants(key).await?;
    let subject = format!("api.{}.{}.{}", request.org, request.project, request.name);
    let principal = store
        .write_service(domain, &subject, request.display_name)
        .await?;
    for role in request.roles {
        store.grant_role(key, &principal, role).await?;
    }
    Ok(store
        .issue_pat(&principal, request.name, request.lifetime)
        .await?)
}
