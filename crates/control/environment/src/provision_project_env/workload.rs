//! CNPG role credentials and non-secret component connection configuration.

use anyhow::Context as _;
use serde_json::json;
use wamn_control_provision::connection::managed_role;
use wamn_control_provision::{
    WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType, sql, workload_login,
};
use wamn_kubernetes::kubectl;
use wamn_registry::Triple;

use crate::environment::{Platform, document::EnvironmentDocument};

pub(crate) use wamn_platform_install::credentials::{
    configuration, provision_platform_migration, reconcile, remove_roles, wait_role,
};

/// Provision every role once and publish only public connection facts in a ConfigMap.
pub(crate) async fn provision(
    platform: &Platform,
    document: &EnvironmentDocument,
    cluster: &str,
    project_input: &str,
    control_input: &str,
) -> anyhow::Result<()> {
    let namespace = document.org.as_str();
    let mut project = wamn_postgres::connect(project_input).await?;
    let mut control = wamn_postgres::connect(control_input).await?;
    let project_options = wamn_postgres::connect_options(project_input).await?;
    let control_options = wamn_postgres::connect_options(control_input).await?;
    let project_database = project_options
        .get_database()
        .context("project database is absent")?;
    let control_database = control_options
        .get_database()
        .context("control database is absent")?;
    let control_cluster = control_options
        .get_host()
        .split('.')
        .next()
        .context("control database host is absent")?
        .trim_end_matches("-rw");
    let key = wamn_registry::project_env_key(&document.org, &document.project, &document.env);
    let triple = document.triple();
    let mut data = serde_json::Map::new();
    let mut application = None;
    let mut dynamic = None;
    for family in WorkloadRoleFamily::project_env_families() {
        let is_control = family.identity_type() == WorkloadRoleIdentityType::Control;
        let database = if is_control {
            control_database
        } else {
            project_database
        };
        let role_cluster = if is_control { control_cluster } else { cluster };
        let identity = match family.identity_type() {
            WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key: &key,
                database,
            },
            WorkloadRoleIdentityType::ProjectEnvironment => {
                WorkloadRoleIdentity::ProjectEnvironment {
                    org: &document.org,
                    project: &document.project,
                    environment: &document.env,
                    database,
                }
            }
            WorkloadRoleIdentityType::Control => WorkloadRoleIdentity::Control {
                org: &document.org,
                project: &document.project,
                environment: &document.env,
                database,
            },
            WorkloadRoleIdentityType::Org => unreachable!("org roles are not project credentials"),
        };
        let username = workload_login(family, identity)?;
        let secret = wamn_control_provision::workload_secret_name(
            family,
            &document.org,
            &document.project,
            &document.env,
        );
        let connection = if is_control {
            &mut control
        } else {
            &mut project
        };
        let surface = sql::stable_surface_sql(family)
            .unwrap_or_else(|| sql::ensure_workload_acl_role_sql(family));
        sqlx::raw_sql(sqlx::AssertSqlSafe(surface))
            .execute(&mut *connection)
            .await?;
        reconcile(
            &platform.system_database_url,
            &platform.target.kubeconfig,
            &platform.target.context,
            namespace,
            role_cluster,
            &managed_role(username.clone(), family.acl_role(), secret.clone()),
        )
        .await?;
        wait_role(connection, &username).await?;
        if family == WorkloadRoleFamily::Migration {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "GRANT {} TO wamn_db_owner WITH ADMIN FALSE, INHERIT TRUE, SET TRUE",
                postgres_protocol::escape::escape_identifier(&username)
            )))
            .execute(&mut *connection)
            .await?;
        }
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql::grant_workload_login_sql(
            family, database, &username,
        )))
        .execute(&mut *connection)
        .await?;
        if is_control {
            let mapped: Option<String> =
                sqlx::query_scalar(sql::upsert_control_author_project_env_key_mapping_sql())
                    .bind(&username)
                    .bind(&key)
                    .bind(&document.org)
                    .bind(&document.project)
                    .bind(&document.env)
                    .fetch_optional(&mut *connection)
                    .await?;
            anyhow::ensure!(
                mapped.as_deref() == Some(key.as_str()),
                "control login maps to another environment"
            );
        }
        let host = if is_control {
            control_options.get_host().to_owned()
        } else {
            platform
                .database_host
                .clone()
                .unwrap_or_else(|| format!("{role_cluster}-rw.{namespace}.svc.cluster.local"))
        };
        let config = configuration(
            namespace,
            role_cluster,
            host,
            platform.database_port,
            database.into(),
            username,
            secret,
        );
        if family == WorkloadRoleFamily::App {
            application = Some(serde_json::to_value(&config)?);
        }
        if family == WorkloadRoleFamily::Query {
            dynamic = Some(serde_json::to_value(&config)?);
        }
        if family == WorkloadRoleFamily::SessionRoleReader {
            let instance =
                super::read_project_env_instance(&platform.system_database_url, &triple).await?;
            let target = wamn_control_provision::session_target::SessionTarget::new(
                &triple,
                &instance,
                &key,
                &config.to_json()?,
            )?;
            data.insert("session-target.json".into(), target.to_json()?.into());
        }
        if family == WorkloadRoleFamily::Administration {
            let name = wamn_registry::name::control_administration_secret_name(&document.org);
            let key = wamn_control_provision::control_administration_key(
                &document.project,
                &document.env,
            );
            let patch = json!({"data":{key:config.to_json()?}}).to_string();
            kubectl::kubectl(
                &platform.target.kubeconfig,
                &platform.target.context,
                namespace,
                &["patch", "configmap", &name, "--type=merge", "-p", &patch],
                None,
            )
            .await?;
        }
        data.insert(
            format!("{}.json", family.component_stem()),
            config.to_json()?.into(),
        );
    }
    data.insert("app.json".into(), json!({"compiled":application.context("application login is absent")?, "dynamic":dynamic.context("query login is absent")?}).to_string().into());
    let name = wamn_control_provision::project_env_secret_name(
        &document.org,
        &document.project,
        &document.env,
    );
    let config_map = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":namespace},"data":data});
    kubectl::apply(
        &platform.target.kubeconfig,
        &platform.target.context,
        namespace,
        &config_map,
    )
    .await?;
    Ok(())
}

/// Restore grants for existing managed logins after replacing a project database.
pub(crate) async fn restore_database_access(
    system_input: &str,
    input: &str,
    triple: &Triple,
) -> anyhow::Result<()> {
    let instance = super::read_project_env_instance(system_input, triple).await?;
    let mut database = wamn_control_provision::project_env_database_name(
        &triple.org,
        &triple.project,
        &triple.env,
        &instance,
    );
    let key = wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env);
    let mut connection = wamn_postgres::connect(input).await?;
    for family in WorkloadRoleFamily::project_env_families()
        .filter(|family| family.identity_type() != WorkloadRoleIdentityType::Control)
    {
        let identity = if family.identity_type() == WorkloadRoleIdentityType::ProjectEnvKey {
            WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key: &key,
                database: &mut database,
            }
        } else {
            WorkloadRoleIdentity::ProjectEnvironment {
                org: &triple.org,
                project: &triple.project,
                environment: &triple.env,
                database: &mut database,
            }
        };
        let username = workload_login(family, identity)?;
        if sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1)")
            .bind(&username)
            .fetch_one(&mut connection)
            .await?
        {
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql::grant_workload_login_sql(
                family, &database, &username,
            )))
            .execute(&mut connection)
            .await?;
        }
    }
    Ok(())
}
