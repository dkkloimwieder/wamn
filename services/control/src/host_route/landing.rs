//! The member's browser application list and default.

use std::sync::LazyLock;

use anyhow::Context as _;
use serde::Deserialize;
use sqlx::{Connection as _, PgPool, types::Json};
use wamn_identity_protocol::PrincipalId;
use wamn_registry::{AppName, EnvName, OrgId, ProjectId, name, platform_domain::PlatformDomain};
use wamn_routes::host_wire::{AppChoice, AppMineResult, ListedApp};

use crate::Writer;

// The app chart publishes the applied client pin in the Service annotation.
const ELIGIBLE_APPS: &str = "
    SELECT a.env, e.prod, a.project, a.app
    FROM registry.env_app a
    JOIN registry.env e USING (org, env)
    WHERE a.org = $1 AND a.active AND e.active AND a.client_web IS NOT NULL
      AND identity.has_project_access($2, a.org, a.env, a.project)";

#[derive(Debug, Deserialize)]
struct EligibleApp {
    env: EnvName,
    prod: bool,
    project: ProjectId,
    app: AppName,
}

#[derive(Debug, sqlx::FromRow)]
struct MineRow {
    platform_domain: Option<String>,
    default_project: Option<String>,
    default_app: Option<String>,
    apps: Json<Vec<EligibleApp>>,
}

pub(super) async fn mine(
    pool: &PgPool,
    org: &OrgId,
    caller: &PrincipalId,
) -> anyhow::Result<AppMineResult> {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        format!(
            "WITH eligible AS ({ELIGIBLE_APPS})
         SELECT meta.platform_domain, m.default_project, m.default_app,
                (SELECT COALESCE(jsonb_agg(a ORDER BY a.env, a.project, a.app), '[]')
                 FROM eligible a) AS apps
         FROM registry.meta meta
         LEFT JOIN identity.org_membership m ON m.org = $1 AND m.principal_id = $2"
        )
    });
    let row: MineRow = sqlx::query_as(SQL.as_str())
        .bind(org.as_str())
        .bind(uuid::Uuid::from(*caller))
        .fetch_one(pool)
        .await?;
    let domain: PlatformDomain = row
        .platform_domain
        .context("the platform domain is unset")?
        .parse()?;
    let apex = name::org_host(org, &domain);
    let default = row
        .default_project
        .zip(row.default_app)
        .filter(|(project, app)| {
            row.apps
                .iter()
                .any(|entry| entry.project.as_str() == project && entry.app.as_str() == app)
        })
        .map(|(project, app)| AppChoice { project, app });
    let apps = row
        .apps
        .0
        .into_iter()
        .map(|entry| {
            let mount = name::app_mount(&entry.project, &entry.app);
            let url = if entry.prod {
                format!("https://{apex}{mount}")
            } else {
                let host = name::env_host(org, &entry.env, &domain);
                format!("https://{host}{mount}")
            };
            ListedApp {
                env: entry.env.into(),
                prod: entry.prod,
                project: entry.project.into(),
                app: entry.app.into(),
                url,
            }
        })
        .collect();
    Ok(AppMineResult { default, apps })
}

pub(super) async fn set_default(
    writer: &Writer,
    org: &OrgId,
    caller: &PrincipalId,
    operation: &str,
    choice: Option<&AppChoice>,
) -> anyhow::Result<bool> {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        format!(
            "WITH eligible AS ({ELIGIBLE_APPS})
         UPDATE identity.org_membership m SET default_project = $3, default_app = $4
         WHERE m.org = $1 AND m.principal_id = $2 AND m.active
           AND EXISTS (SELECT FROM identity.principal p WHERE p.id = m.principal_id AND p.active)
           AND EXISTS (SELECT FROM registry.org o WHERE o.id = m.org AND o.active)
           AND ($3::text IS NULL OR EXISTS (SELECT FROM eligible WHERE project = $3 AND app = $4))"
        )
    });
    let mut writer = writer.lock().await?;
    let mut transaction = writer.begin().await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        caller,
        "",
        "",
        Some(operation),
        None,
    )
    .await?;
    let result = sqlx::query(SQL.as_str())
        .bind(org.as_str())
        .bind(uuid::Uuid::from(*caller))
        .bind(choice.map(|choice| choice.project.as_str()))
        .bind(choice.map(|choice| choice.app.as_str()))
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(result.rows_affected() != 0)
}
