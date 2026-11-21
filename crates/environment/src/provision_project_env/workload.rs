//! CloudNativePG logins and their public project connection configuration.

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_postgres::ConnectionConfig;
use wamn_provision::{WorkloadRoleFamily, sql};
use wamn_registry::Triple;

/// The connection of each project login, derived from the owner connection.
pub fn connections(
    owner: &ConnectionConfig,
) -> anyhow::Result<Vec<(WorkloadRoleFamily, ConnectionConfig)>> {
    WorkloadRoleFamily::ALL
        .into_iter()
        .map(|family| {
            let mut connection = owner.clone();
            connection.username = family.login_name().parse()?;
            connection.credentials.name = wamn_registry::name::project_login_certificate(
                &connection.database,
                connection.username.as_str(),
            );
            Ok((family, connection))
        })
        .collect()
}

/// The project chart's `databaseConfig` value: one connection file for each login.
pub fn configuration_values(
    triple: &Triple,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
    connections: &[(WorkloadRoleFamily, ConnectionConfig)],
) -> anyhow::Result<Value> {
    let mut data = serde_json::Map::new();
    data.insert("instance".into(), instance.to_string().into());
    for (family, connection) in connections {
        data.insert(
            format!("{}.json", family.component_stem()),
            connection.to_json()?.into(),
        );
    }
    let login = |family| {
        connections
            .iter()
            .find(|(candidate, _)| *candidate == family)
            .map(|(_, connection)| connection)
            .context("project login is absent")
    };
    data.insert(
        "app.json".into(),
        json!({
            "compiled":login(WorkloadRoleFamily::App)?,
            "dynamic":login(WorkloadRoleFamily::Query)?,
        })
        .to_string()
        .into(),
    );
    Ok(json!({
        "name":wamn_provision::project_env_secret_name(&triple.org, &triple.project, &triple.env),
        "data":data,
    }))
}

/// Grant each project login its database surface once CloudNativePG has created it.
pub async fn grant(
    project: &mut sqlx::PgConnection,
    connections: &[(WorkloadRoleFamily, ConnectionConfig)],
) -> anyhow::Result<()> {
    for (family, connection) in connections {
        wamn_platform_install::credentials::wait_role(project, &connection.username).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql::grant_workload_login_sql(
            *family,
            &connection.database,
            &connection.username,
        )))
        .execute(&mut *project)
        .await?;
    }
    Ok(())
}
