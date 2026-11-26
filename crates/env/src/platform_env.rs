//! Database provisioning inputs rendered into the project chart.

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_postgres::database::ConnectionConfig;

/// Project sizing and role values for the chart.
pub fn database_values(
    connection: &ConnectionConfig,
    replicas: u32,
    mut sizing: Value,
    settings: &Value,
) -> anyhow::Result<Value> {
    use wamn_registry::name;
    let database = &connection.database;
    let cluster = name::project_cluster(database);
    let roles = crate::roles::project(database, replicas)?;
    let max_connections = roles.iter().map(|role| role.connection_limit).sum::<u32>() + 10;
    if let Some(backup) = sizing.get_mut("backup") {
        for key in ["endpoint", "bucket", "secret"] {
            backup[key] = settings["backup"][key]
                .as_str()
                .filter(|value| !value.is_empty())
                .with_context(|| format!("installation PostgreSQL backup {key} is absent"))?
                .into();
        }
    }
    sizing["name"] = json!(cluster);
    sizing["image"] = settings["image"]
        .as_str()
        .context("installation PostgreSQL image is absent")?
        .into();
    sizing["roles"] = serde_json::to_value(roles)?;
    // Reserve headroom for PostgreSQL and the operator.
    sizing["maxConnections"] = max_connections.max(100).into();
    sizing["initializeRoles"] = crate::project_setup::ensure_project_roles_sql().into();
    let extensions = crate::sql::PLATFORM_EXTENSIONS.map(|name| json!({"name":name}));
    Ok(json!({
        "namespace":connection.credentials.namespace,
        "cluster":sizing,
        "database":{
            "name":database,"cluster":cluster,"owner":connection.username,"extensions":extensions
        }
    }))
}
