//! Project membership projection under system authority.

use crate::identity::application;
use sqlx::Connection as _;
use std::collections::BTreeSet;
use wamn_authentication::administration::ApplicationTarget;

/// Fill member rows from system authority.
pub async fn fill(
    system: &mut sqlx::PgConnection,
    project: &mut sqlx::PgConnection,
    issuer: &str,
    target: ApplicationTarget<'_>,
) -> anyhow::Result<()> {
    let mut system = system.begin().await?;
    crate::identity::org::lock_org_memberships(&mut system, target.org).await?;
    let mut project = project.begin().await?;
    crate::bind_platform_principal(&mut project, crate::PlatformComponent::Provisioning).await?;
    let mut principals = BTreeSet::new();
    principals.extend(
        sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT id FROM identity.principals WHERE identity.has_project_access(id,$1,$2,$3)",
        )
        .bind(target.org.as_str())
        .bind(target.env.as_str())
        .bind(target.project.as_str())
        .fetch_all(&mut *system)
        .await?,
    );
    principals.extend(
        sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM platform.\"user\" WHERE type='user'")
            .fetch_all(&mut *project)
            .await?,
    );
    for id in principals {
        let id = wamn_identity_protocol::PrincipalId::from(id);
        application::project_authority(&mut system, &mut project, target, &id, issuer).await?;
    }
    project.commit().await?;
    system.commit().await?;
    Ok(())
}
