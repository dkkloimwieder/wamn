//! Test credentials held outside the database.
pub use wamn_kubernetes::test_util::MemoryStore;
use wamn_kubernetes::{Error, Store, StoreVersion};

/// A reference to material that the fixture already wrote outside Postgres.
#[derive(Debug)]
pub struct ReferencedCredentials<'a>(pub &'a StoreVersion);

#[async_trait::async_trait]
impl Store for ReferencedCredentials<'_> {
    async fn put(
        &self,
        _: &wamn_registry::Triple,
        _: uuid::Uuid,
        _: &[u8],
    ) -> Result<StoreVersion, Error> {
        Ok(StoreVersion {
            store: self.0.store,
            reference: self.0.reference.clone(),
            version: self.0.version.clone(),
        })
    }
    async fn get(&self, _: &str, _: &str) -> Result<Vec<u8>, Error> {
        unreachable!("fixture only writes references")
    }
    async fn destroy(&self, _: &str, _: &str) -> Result<(), Error> {
        unreachable!("fixture retains its store")
    }
}

/// Create project authority through the administration functions.
pub async fn define(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    request: &serde_json::Value,
    store: &dyn Store,
) -> anyhow::Result<()> {
    use serde_json::json;
    use wamn_broker::auth_changes::AuthDrops;
    use wamn_platform_identity::connections::administration;
    let mut drops = AuthDrops::default();
    if let Some(name) = request["external_system"].as_str() {
        administration::execute(
            tx,
            org,
            "external_system.write",
            &json!({"project":request["project"],"name":name}).to_string(),
            &mut drops,
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        )
        .await?;
    }
    administration::execute(tx,org,"integration.write",&json!({
        "project":request["project"],"name":request["name"],"integration_type":request["integration_type"],
        "auth_type":request["auth_type"],"external_system":request["external_system"],
    }).to_string(),&mut drops, &mut wamn_broker::connection_changes::ConnectionDrops::default()).await?;
    let mut connection = json!({"project":request["project"],"env":request["env"],"name":request["name"],"parameters":request["parameters"]});
    if request["auth_type"] != "none" {
        connection["credential"] = request
            .get("credential")
            .cloned()
            .unwrap_or_else(|| json!("{}"));
    }
    administration::execute_with_store(
        tx,
        org,
        "connection.write",
        &connection.to_string(),
        &mut drops,
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        Some(store),
    )
    .await?;
    Ok(())
}

/// Rotate an existing fixture through the administration path.
pub async fn set_reference(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    project: &str,
    environment: &str,
    name: &str,
    stored: &StoreVersion,
) -> anyhow::Result<()> {
    wamn_platform_identity::connections::administration::execute_with_store(
        tx,
        org,
        "connection.set_credential",
        &serde_json::json!({"project":project,"env":environment,"name":name,"credential":"{}"})
            .to_string(),
        &mut wamn_broker::auth_changes::AuthDrops::default(),
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        Some(&ReferencedCredentials(stored)),
    )
    .await?;
    Ok(())
}

/// Create an isolated test login with the public role identity and grants.
pub async fn database_login(
    system_input: &str,
    target_input: Option<&str>,
    org: &str,
    project: &str,
    environment: &str,
    family: wamn_control_provision::WorkloadRoleFamily,
) -> anyhow::Result<String> {
    use wamn_control_provision::{WorkloadRoleIdentity, WorkloadRoleIdentityType};
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
    let login = wamn_control_provision::workload_login(family, identity)?;
    let mut connection = wamn_postgres::connect(input).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::test_database::login_sql(
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
            wamn_control_provision::sql::upsert_control_author_project_env_key_mapping_sql(),
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
    url.set_username(&login)
        .map_err(|()| anyhow::anyhow!("fixture login"))?;
    url.set_password(Some("database-interface-only"))
        .map_err(|()| anyhow::anyhow!("fixture password"))?;
    Ok(url.into())
}

/// Supply the native apply role and grants for a database interface test.
pub async fn apply_login(input: &str) -> anyhow::Result<String> {
    let mut connection = wamn_postgres::connect(input).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{} CREATE ROLE wamn_apply LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD 'database-interface-only'; {} {}",
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        wamn_control_provision::apply::ensure_apply_role_sql(),
        wamn_control_provision::apply::APPLY_GRANTS_SQL,
    ))).execute(&mut connection).await?;
    let mut input = url::Url::parse(input)?;
    input.set_username("wamn_apply").unwrap();
    input.set_password(Some("database-interface-only")).unwrap();
    Ok(input.into())
}
