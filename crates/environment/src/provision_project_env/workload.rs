//! CloudNativePG logins and their public project connection configuration.

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_postgres::ConnectionConfig;
use wamn_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
use wamn_registry::Triple;

pub(crate) fn connections(
    triple: &Triple,
    owner: &ConnectionConfig,
) -> anyhow::Result<Vec<(WorkloadRoleFamily, ConnectionConfig)>> {
    let key = wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env);
    let database = owner.database.as_str();
    WorkloadRoleFamily::project_env_families()
        .map(|family| {
            let (username, secret) = match family {
                WorkloadRoleFamily::Migration => {
                    return Ok((family, owner.clone()));
                }
                WorkloadRoleFamily::Platform => {
                    let role = wamn_provision::workload_role::platform_managed_role(database);
                    (role.name, role.password_secret.name)
                }
                _ => (
                    workload_login(
                        family,
                        WorkloadRoleIdentity::ProjectEnvKey {
                            project_env_key: &key,
                            database,
                        },
                    )?,
                    wamn_provision::workload_secret_name(
                        family,
                        &triple.org,
                        &triple.project,
                        &triple.env,
                    ),
                ),
            };
            let mut connection = owner.clone();
            connection.username = username;
            connection.credentials.name = secret;
            Ok((family, connection))
        })
        .collect()
}

pub(crate) fn configuration_values(
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

pub(crate) async fn grant(
    project: &mut sqlx::PgConnection,
    connections: &[(WorkloadRoleFamily, ConnectionConfig)],
) -> anyhow::Result<()> {
    for (family, connection) in connections {
        let surface = sql::stable_surface_sql(*family)
            .unwrap_or_else(|| sql::ensure_workload_acl_role_sql(*family));
        sqlx::raw_sql(sqlx::AssertSqlSafe(surface))
            .execute(&mut *project)
            .await?;
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
