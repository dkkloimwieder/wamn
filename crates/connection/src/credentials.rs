//! Test credential fixtures and isolated database logins.

/// Create an isolated test login with the public role identity and grants.
pub async fn database_login(
    system_input: &str,
    target_input: Option<&str>,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    environment: &wamn_registry::EnvName,
    family: wamn_provision::WorkloadRoleFamily,
) -> anyhow::Result<String> {
    use wamn_provision::{WorkloadRoleIdentity, WorkloadRoleIdentityType};
    let control = matches!(
        family.identity_type(),
        WorkloadRoleIdentityType::Control | WorkloadRoleIdentityType::Org
    );
    let input = if control {
        system_input
    } else {
        target_input.unwrap_or(system_input)
    };
    let (_, database) = wamn_postgres::identity(input)?;
    let key = wamn_registry::project_env_key(org, project, environment);
    let identity = match family.identity_type() {
        WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &key,
            database: &database,
        },
        WorkloadRoleIdentityType::ProjectEnvironment => WorkloadRoleIdentity::ProjectEnvironment {
            org,
            project,
            environment,
            database: &database,
        },
        WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
            org,
            project,
            environment,
            database: &database,
        },
        WorkloadRoleIdentityType::Org => WorkloadRoleIdentity::Org {
            org,
            database: &database,
        },
    };
    let login = wamn_provision::workload_login(family, identity)?;
    let mut connection = wamn_postgres::connect(input).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::test_database::login_sql(
            family,
            &database,
            &login,
            "database-interface-only",
        ),
    ))
    .execute(&mut connection)
    .await?;
    if control {
        sqlx::query(
            wamn_provision::sql::upsert_control_author_project_env_key_mapping_sql(),
        )
        .bind(&login)
        .bind(&key)
        .bind(org)
        .bind(project)
        .bind(environment)
        .execute(&mut connection)
        .await?;
    }
    let mut url = url::Url::parse(input)?;
    url.set_username(login.as_str())
        .map_err(|()| anyhow::anyhow!("fixture login"))?;
    url.set_password(Some("database-interface-only"))
        .map_err(|()| anyhow::anyhow!("fixture password"))?;
    Ok(url.into())
}
