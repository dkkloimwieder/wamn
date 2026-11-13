//! CNPG role credentials and non-secret component connection configuration.

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use wamn_control_provision::connection::{ManagedRole, managed_role};
use wamn_control_provision::{
    WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType, sql, workload_login,
};
use wamn_kubernetes::{kubectl, secret};
use wamn_postgres::{ConnectionConfig, SecretRef};
use wamn_registry::Triple;

use crate::environment::{Platform, document::EnvironmentDocument};

/// Reconcile one basic-auth Secret and one entry in CNPG's role declarations.
/// A resource-version comparison preserves other environments' role declarations.
pub(crate) async fn reconcile(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    cluster: &str,
    declaration: &ManagedRole,
) -> anyhow::Result<()> {
    let username = declaration.name.as_str();
    let secret_name = declaration.password_secret.name.as_str();
    secret::update(kubeconfig, context, namespace, secret_name, |secret| {
        if secret.metadata.resource_version.is_none() {
            let mut bytes = [0u8; 32];
            SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| anyhow::anyhow!("generate database credential"))?;
            *secret =
                secret::Secret::basic_auth(namespace, secret_name, username, &hex::encode(bytes));
        } else {
            anyhow::ensure!(
                secret.content_type == "kubernetes.io/basic-auth",
                "old database Secret shape refused"
            );
            let user = secret
                .data
                .get("username")
                .context("database Secret has no username")?;
            anyhow::ensure!(
                STANDARD.decode(user)? == username.as_bytes()
                    && secret.data.contains_key("password"),
                "database Secret names another role"
            );
        }
        Ok(())
    })
    .await?;
    let role = serde_json::to_value(declaration)?;
    edit_roles(kubeconfig, context, namespace, cluster, |roles| {
        if let Some(existing) = roles.iter_mut().find(|entry| entry["name"] == username) {
            *existing = role.clone();
        } else {
            roles.push(role.clone());
        }
    })
    .await
}

/// Stop managing logins before their owning environment drops them.
pub(crate) async fn remove_roles(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    cluster: &str,
    names: &[String],
) -> anyhow::Result<()> {
    edit_roles(kubeconfig, context, namespace, cluster, |roles| {
        roles.retain(|role| {
            role["name"]
                .as_str()
                .is_none_or(|name| !names.iter().any(|entry| entry == name))
        });
    })
    .await
}

async fn edit_roles(
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    cluster: &str,
    edit: impl Fn(&mut Vec<Value>),
) -> anyhow::Result<()> {
    for attempt in 0..5 {
        let object: Value = kubectl::get(
            kubeconfig,
            context,
            namespace,
            "cluster.postgresql.cnpg.io",
            cluster,
        )
        .await?
        .context("CNPG cluster is absent from the org namespace")?;
        let mut managed = object["spec"]["managed"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut roles = managed
            .get("roles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let previous = roles.clone();
        edit(&mut roles);
        if previous == roles {
            return Ok(());
        }
        managed.insert("roles".into(), roles.into());
        let patch = json!([
            {"op":"test", "path":"/metadata/resourceVersion", "value":object["metadata"]["resourceVersion"]},
            {"op":"add", "path":"/spec/managed", "value":managed}
        ]).to_string();
        let result = kubectl::kubectl(
            kubeconfig,
            context,
            namespace,
            &[
                "patch",
                "cluster.postgresql.cnpg.io",
                cluster,
                "--type=json",
                "-p",
                &patch,
            ],
            None,
        )
        .await;
        match result {
            Ok(_) => return Ok(()),
            Err(error) if attempt < 4 && error.is::<kubectl::Conflict>() => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("last role declaration update returns its result")
}

pub(crate) fn configuration(
    namespace: &str,
    cluster: &str,
    host: String,
    port: u16,
    database: String,
    username: String,
    secret: String,
) -> ConnectionConfig {
    ConnectionConfig {
        host,
        port,
        database,
        username,
        credentials: SecretRef {
            namespace: namespace.into(),
            name: secret,
        },
        server_ca: SecretRef {
            namespace: namespace.into(),
            name: format!("{cluster}-ca"),
        },
    }
}

pub(crate) async fn wait_role(
    connection: &mut sqlx::PgConnection,
    username: &str,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1 AND rolcanlogin)",
        )
        .bind(username)
        .fetch_one(&mut *connection)
        .await?;
        if present {
            return Ok(());
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "CNPG did not reconcile the database login"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

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
            &platform.target.kubeconfig,
            &platform.target.context,
            namespace,
            role_cluster,
            &managed_role(username.clone(), family.acl_role(), secret.clone()),
        )
        .await?;
        wait_role(connection, &username).await?;
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

/// Create a managed control-database login and publish its public configuration.
pub async fn provision_control_role(
    input: &str,
    namespace: &str,
    family: WorkloadRoleFamily,
) -> anyhow::Result<()> {
    let options = wamn_postgres::connect_options(input).await?;
    let database = options
        .get_database()
        .context("control database is absent")?;
    let cluster = options
        .get_host()
        .split('.')
        .next()
        .context("control host is absent")?
        .trim_end_matches("-rw");
    let username = workload_login(
        family,
        WorkloadRoleIdentity::Org {
            org: namespace,
            database,
        },
    )?;
    let secret = wamn_control_provision::org_workload_secret_name(family, namespace);
    let kubeconfig = std::env::var_os("KUBECONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let mut connection = wamn_postgres::connect(input).await?;
    let surface = sql::stable_surface_sql(family)
        .unwrap_or_else(|| sql::ensure_workload_acl_role_sql(family));
    sqlx::raw_sql(sqlx::AssertSqlSafe(surface))
        .execute(&mut connection)
        .await?;
    reconcile(
        &kubeconfig,
        "",
        namespace,
        cluster,
        &managed_role(username.clone(), family.acl_role(), secret.clone()),
    )
    .await?;
    wait_role(&mut connection, &username).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql::grant_workload_login_sql(
        family, database, &username,
    )))
    .execute(&mut connection)
    .await?;
    let config = configuration(
        namespace,
        cluster,
        options.get_host().into(),
        options.get_port(),
        database.into(),
        username,
        secret,
    );
    let name = wamn_registry::name::control_administration_secret_name(namespace);
    let object = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":namespace},"data":{"control.json":config.to_json()?}});
    kubectl::apply(&kubeconfig, "", namespace, &object).await?;
    Ok(())
}
