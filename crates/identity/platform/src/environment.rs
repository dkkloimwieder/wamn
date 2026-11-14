//! Fill identity rows when an environment is provisioned.

use crate::application::{self, ApplicationTarget, ApplicationUser};
use anyhow::{Context as _, ensure};
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeSet;

/// Fill platform, service, member and administrator rows from system authority.
pub async fn fill(
    system_url: &str,
    project_url: &str,
    target: ApplicationTarget<'_>,
    changes: &wamn_broker::Publisher,
) -> anyhow::Result<()> {
    let mut system = wamn_postgres::connect(system_url).await?;
    let mut system = system.begin().await?;
    crate::org::lock_org_memberships(&mut system, target.org).await?;
    let domain: Option<String> =
        sqlx::query_scalar(wamn_registry::sql::IDENTITY_PLATFORM_SRC_APPLICATION_PROJECT_AUTHORITY)
            .fetch_one(&mut *system)
            .await?;
    let domain = domain.context("platform domain is absent")?;
    let mut project = wamn_postgres::connect(project_url).await?;
    let mut project = project.begin().await?;
    fill_platform(&mut project, &domain).await?;
    let mut drops = wamn_broker::auth_changes::AuthDrops::default();
    let mut principals = BTreeSet::new();
    for row in sqlx::query("SELECT DISTINCT p.id::text, p.subject, p.display_name FROM identity.project_roles r JOIN identity.principals p ON p.id=r.principal_id WHERE r.org=$1 AND r.project=$2 AND p.type='service' AND p.status='active'")
        .bind(target.org).bind(target.project).fetch_all(&mut *system).await? {
        let id: String = row.get(0);
        let principal_id = id.parse()?;
        let subject: String = row.get(1);
        let display_name: String = row.get(2);
        let present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM platform.\"user\" WHERE id=$1::text::uuid)")
            .bind(&id).fetch_one(&mut *project).await?;
        if !present {
            application::write_service(&mut project, target.project_env_key, ApplicationUser { principal_id: &principal_id, email: &format!("{subject}@{domain}"), display_name: &display_name }, &mut drops).await?;
        }
        principals.insert(id);
    }
    principals.extend(sqlx::query_scalar::<_, String>("SELECT id::text FROM identity.principals WHERE identity.has_project_access(id,$1,$2,$3)").bind(target.org).bind(target.env).bind(target.project).fetch_all(&mut *system).await?);
    principals.extend(
        sqlx::query_scalar::<_, String>(
            "SELECT id::text FROM platform.\"user\" WHERE type IN ('user','service')",
        )
        .fetch_all(&mut *project)
        .await?,
    );
    for id in principals {
        application::project_authority(&mut system, &mut project, target, &id.parse()?, &mut drops)
            .await?;
    }
    ensure!(
        !target.project_env_key.is_empty(),
        "project environment key is empty"
    );
    project.commit().await?;
    system.commit().await?;
    changes.publish(drops).await;
    Ok(())
}

/// Fill the platform principal rows in an initialized project database.
pub async fn fill_platform(project: &mut sqlx::PgConnection, domain: &str) -> anyhow::Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::platform_principals_sql(domain)?,
    ))
    .execute(project)
    .await?;
    Ok(())
}
