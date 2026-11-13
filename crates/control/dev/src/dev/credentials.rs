//! Explicit database authorities for disposable local development.
use anyhow::Context as _;
use postgres_protocol::escape::{escape_identifier, escape_literal};
use ring::rand::SecureRandom as _;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType};

pub(super) async fn login(
    input: &str,
    role: &str,
    acl: Option<&str>,
    surface: &str,
) -> anyhow::Result<String> {
    let (_, database) = wamn_postgres::identity(input)?;
    let mut password = [0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut password)
        .map_err(|_| anyhow::anyhow!("generate local database password"))?;
    let password = hex::encode(password);
    let quoted = escape_identifier(role);
    let mut client = wamn_postgres::connect(input).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("{surface} DO $local$ BEGIN IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname={}) THEN CREATE ROLE {quoted} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS; END IF; END $local$; ALTER ROLE {quoted} PASSWORD {}; GRANT CONNECT ON DATABASE {} TO {quoted};", escape_literal(role), escape_literal(&password), escape_identifier(&database))))
        .execute(&mut client).await?;
    if let Some(acl) = acl {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "GRANT {} TO {quoted} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE",
            escape_identifier(acl)
        )))
        .execute(&mut client)
        .await?;
    }
    let mut url = url::Url::parse(input)
        .context("local development requires an explicit operator database URL")?;
    url.set_username(role)
        .map_err(|_| anyhow::anyhow!("set local database login"))?;
    url.set_password(Some(&password))
        .map_err(|_| anyhow::anyhow!("set local database password"))?;
    Ok(url.into())
}

pub(super) async fn workload(
    input: &str,
    coordinate: &super::environment::JourneyTarget,
    family: WorkloadRoleFamily,
) -> anyhow::Result<String> {
    let (_, database) = wamn_postgres::identity(input)?;
    let key = coordinate.project_env_key();
    let identity = match family.identity_type() {
        WorkloadRoleIdentityType::Org => WorkloadRoleIdentity::Org {
            org: &coordinate.org,
            database: &database,
        },
        WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &key,
            database: &database,
        },
        WorkloadRoleIdentityType::ProjectEnvironment => WorkloadRoleIdentity::ProjectEnvironment {
            org: &coordinate.org,
            project: &coordinate.project,
            environment: &coordinate.environment,
            database: &database,
        },
        WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
            org: &coordinate.org,
            project: &coordinate.project,
            environment: &coordinate.environment,
            database: &database,
        },
    };
    let role = wamn_control_provision::workload_login(family, identity)?;
    let surface = wamn_control_provision::sql::ensure_workload_acl_role_sql(family);
    let url = login(input, &role, Some(family.acl_role()), &surface).await?;
    let mut client = wamn_postgres::connect(input).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::grant_workload_login_sql(family, &database, &role),
    ))
    .execute(&mut client)
    .await?;
    if family.identity_type() == WorkloadRoleIdentityType::Control {
        let mapped: Option<String> = sqlx::query_scalar(
            wamn_control_provision::sql::upsert_control_author_project_env_key_mapping_sql(),
        )
        .bind(&role)
        .bind(&key)
        .bind(&coordinate.org)
        .bind(&coordinate.project)
        .bind(&coordinate.environment)
        .fetch_optional(&mut client)
        .await?;
        anyhow::ensure!(
            mapped.as_deref() == Some(key.as_str()),
            "control login maps to another environment"
        );
    }
    Ok(url)
}

pub(super) async fn remove_login(input: &str, role: &str) -> anyhow::Result<()> {
    let mut connection = wamn_postgres::connect(input).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY {}; DROP ROLE IF EXISTS {};",
        escape_identifier(role),
        escape_identifier(role)
    )))
    .execute(&mut connection)
    .await?;
    Ok(())
}
