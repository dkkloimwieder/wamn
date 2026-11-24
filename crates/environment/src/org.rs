//! Org project definitions and environment state in the system database.
//!
//! Control routes call these functions inside the caller's transaction and
//! under the caller's bound actor.

use sqlx::Row as _;
use wamn_authentication::identity_error::{IdentityError, IdentityErrorType, IdentityRefusal};
use wamn_postgres::identity::org::{lock_org_memberships, project_envs};
use wamn_postgres::identity_error::database_error;
use wamn_registry::EnvironmentStatus;

const PROJECT_IN_ORG_SQL: &str = wamn_postgres::identity_sql::PROJECT_IN_ORG_SQL;

const ENV_IN_PROJECT_SQL: &str = wamn_postgres::identity_sql::ENV_IN_PROJECT_SQL;

const CREATE_PROJECT_SQL: &str = wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_CREATE_PROJECT_SQL;

/// Create an org-level project definition. An existing project is refused.
pub async fn create_project(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
) -> Result<(), IdentityError> {
    lock_org_memberships(client, org).await?;
    let created = sqlx::query(CREATE_PROJECT_SQL)
        .bind(org.as_str())
        .bind(project.as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    if created == 0 {
        return Err(IdentityError::refused(
            IdentityErrorType::Conflict,
            IdentityRefusal::ProjectExists,
            format!("project {project} exists in org {org}"),
        ));
    }
    Ok(())
}

const ORG_ENVIRONMENTS_SQL: &str =
    wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_ORG_ENVIRONMENTS_SQL;

/// The environments of `org`, or of one project of it, as `(project, env)`.
pub async fn org_environments(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: Option<&wamn_registry::ProjectId>,
) -> Result<Vec<(wamn_registry::ProjectId, wamn_registry::EnvName)>, IdentityError> {
    sqlx::query_as::<_, (String, String)>(ORG_ENVIRONMENTS_SQL)
        .bind(org.as_str())
        .bind(project.map(wamn_registry::ProjectId::as_str))
        .fetch_all(&mut *client)
        .await
        .and_then(|rows| {
            rows.into_iter()
                .map(|(project, env)| {
                    Ok((
                        wamn_postgres::decode_text(project, 0)?,
                        wamn_postgres::decode_text(env, 1)?,
                    ))
                })
                .collect()
        })
        .map_err(|error| database_error(&error))
}

/// The environments of one project of `org` that a status route changes:
/// one environment when `env` names it, or every environment of the project.
/// A project or an environment outside the org is refused before anything
/// is written.
pub async fn status_environments(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: Option<&wamn_registry::EnvName>,
) -> Result<Vec<wamn_registry::EnvName>, IdentityError> {
    let Some(env) = env else {
        let in_org: bool = sqlx::query(PROJECT_IN_ORG_SQL)
            .bind(org.as_str())
            .bind(project.as_str())
            .fetch_one(&mut *client)
            .await
            .map_err(|error| database_error(&error))?
            .get(0);
        if !in_org {
            return Err(IdentityError::refused(
                IdentityErrorType::NotFound,
                IdentityRefusal::ProjectNotFound,
                format!("project {project} is not a project of org {org}"),
            ));
        }
        return Ok(project_envs(client, org, project)
            .await?
            .into_iter()
            .map(|entry| entry.name)
            .collect());
    };
    let found: bool = sqlx::query(ENV_IN_PROJECT_SQL)
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(env.as_str())
        .fetch_one(&mut *client)
        .await
        .map_err(|error| database_error(&error))?
        .get(0);
    if !found {
        return Err(IdentityError::refused(
            IdentityErrorType::NotFound,
            IdentityRefusal::EnvironmentNotFound,
            format!("environment {project}/{env} is not an environment of org {org}"),
        ));
    }
    Ok(vec![env.clone()])
}

const ENVIRONMENT_STATUS_SQL: &str =
    wamn_registry::sql::IDENTITY_PLATFORM_SRC_ORG_ENVIRONMENT_STATUS_SQL;

/// Write the status of the named environments of one project of `org`.
/// The control routes read them with [`status_environments`] first.
pub async fn set_environment_status(
    client: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    environments: &[wamn_registry::EnvName],
    status: EnvironmentStatus,
) -> Result<(), IdentityError> {
    sqlx::query(ENVIRONMENT_STATUS_SQL)
        .bind(org.as_str())
        .bind(project.as_str())
        .bind(
            environments
                .iter()
                .map(wamn_registry::EnvName::as_str)
                .collect::<Vec<_>>(),
        )
        .bind(status.as_str())
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| database_error(&error))?;
    Ok(())
}
