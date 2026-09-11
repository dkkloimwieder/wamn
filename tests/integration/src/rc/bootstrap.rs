//! Fresh RC provisioning through the existing control library.

use std::fs;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_control_provision::sql;
use wamn_ctl::dev::{environment::connect, pat_issuer};
use wamn_ctl::provision_org::{self, TemplateArg};
use wamn_ctl::provision_project_env::{self, ProvisionProjectEnvArgs, WorkloadGenerationArgs};

use super::{NAMESPACE, Resources, TENANT, apply, command_json, kubectl, save};

pub(super) async fn run(resources: &Resources) -> anyhow::Result<()> {
    let network: Value = serde_json::from_slice(
        &super::resources::recorded(
            resources,
            "inspect-postgres",
            Command::new(&resources.lifecycle).args(["inspect", "wamn-rc-postgres"]),
        )
        .await?,
    )
    .context("parse the owned PostgreSQL network")?;
    let mappings = network
        .pointer("/Ports/5432~1tcp")
        .and_then(Value::as_array)
        .context("PostgreSQL has an ephemeral port mapping")?;
    ensure!(
        mappings.len() == 1 && mappings[0]["HostIp"] == "127.0.0.1",
        "PostgreSQL must bind only one loopback port"
    );
    let port: u16 = mappings[0]["HostPort"]
        .as_str()
        .context("PostgreSQL host port is text")?
        .parse()?;
    ensure!(port != 0, "PostgreSQL host port is nonzero");
    let admin_url = format!("postgres://postgres@127.0.0.1:{port}/postgres?sslmode=disable");
    let system_url = format!("postgres://postgres@127.0.0.1:{port}/wamn_system");
    let (admin, admin_task) = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Ok(pair) = connect(&admin_url).await {
                return pair;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .context("fresh PostgreSQL did not become reachable")?;
    let setup=async {
        admin.batch_execute("CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS").await?;
        admin.batch_execute(&sql::ensure_control_author_acl_role_sql()).await?;
        admin.batch_execute("CREATE DATABASE wamn_system OWNER wamn_system").await?;
        Ok::<_,anyhow::Error>(())
    }.await;
    drop(admin);
    admin_task.abort();
    setup?;
    let (system, system_task) = connect(&system_url).await?;
    let installed=async {
        system.batch_execute("SET ROLE wamn_system").await?;
        system.batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL).await?;
        system.batch_execute(wamn_control_provision::CONTROL_PORTABLE_STORE_SQL).await?;
        system.batch_execute("RESET ROLE").await?;
        system.batch_execute(sql::revoke_public_connect_floor_sql()).await?;
        system.batch_execute("DO $$ BEGIN EXECUTE format('REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); END $$;").await?;
        Ok::<_,anyhow::Error>(())
    }.await;
    drop(system);
    system_task.abort();
    installed?;
    let mut org = provision_org::provision_org_args(
        "rc".into(),
        TemplateArg::Trials,
        "rc-pg".into(),
        Some(system_url.clone()),
    );
    org.emit_clusters = Some(resources.work.join("bootstrap-clusters.json"));
    provision_org::run(org).await?;
    save(
        resources,
        "bootstrap-substrate.json",
        &json!({"passed":true,"substrate":"postgresql-18","port":port,"operation":"provision-org"}),
    )?;
    let issuer = pat_issuer::start(&system_url, &resources.work).await?;
    let password = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let database_path = resources.work.join("bootstrap-database.json");
    let role_path = resources.work.join("bootstrap-role.sql");
    let privilege_path = resources.work.join("bootstrap-privilege.sql");
    let secret_path = resources.work.join("bootstrap-database-secret.json");
    let management_path = resources.work.join("management-pat.json");
    let route_path = resources.work.join("route-pat.json");
    let provisioned = provision_project_env::run(ProvisionProjectEnvArgs {
        org: Some("rc".into()),
        project: Some("app".into()),
        env: Some("dev".into()),
        tenant: Some(TENANT.into()),
        disposable: false,
        system_database_url: Some(system_url),
        cluster: Some("rc-pg".into()),
        connection_limit: None,
        app_password: Some(password),
        app_host: Some("rc-pg-rw".into()),
        app_port: 5432,
        namespace: NAMESPACE.into(),
        secret_namespace: None,
        target_admin_database_url: None,
        workload: WorkloadGenerationArgs::default(),
        emit_database: Some(database_path.clone()),
        emit_role_sql: Some(role_path.clone()),
        emit_privilege_sql: Some(privilege_path.clone()),
        emit_secret: Some(secret_path.clone()),
        pat_issuer: issuer.args.clone(),
        emit_management_author_pat_secret: Some(management_path.clone()),
        emit_route_caller_pat_secret: Some(route_path.clone()),
        revoke_pat_prefix: None,
    })
    .await;
    let stopped = issuer.stop().await;
    provisioned?;
    stopped?;
    for path in [
        &database_path,
        &role_path,
        &privilege_path,
        &secret_path,
        &management_path,
        &route_path,
    ] {
        ensure!(
            fs::metadata(path)?.len() > 0,
            "provisioning must emit each requested private file"
        );
    }
    let database: Value = serde_json::from_slice(&fs::read(&database_path)?)?;
    let name = validate_database(&database)?;
    let (admin, task) = connect(&admin_url).await?;
    let applied=async {
        admin.batch_execute(&fs::read_to_string(&role_path)?).await?;
        admin.batch_execute(&format!("{} OWNER wamn_db_owner", sql::create_database_named_sql(name))).await?;
        let row=admin.query_one("SELECT datname,pg_get_userbyid(datdba) FROM pg_database WHERE datname=$1",&[&name]).await?;
        ensure!(row.get::<_,String>(0)==name && row.get::<_,String>(1)=="wamn_db_owner","the emitted database must use its stable owner");
        let roles=admin.query("SELECT rolname,rolcanlogin,rolinherit,rolpassword IS NULL FROM pg_authid WHERE rolname IN ('wamn_app','wamn_db_owner') ORDER BY rolname",&[]).await?;
        ensure!(roles.len()==2,"both stable application roles must exist");
        let mut observed=Vec::new();
        for (row,expected) in roles.iter().zip(["wamn_app","wamn_db_owner"]) {
            ensure!(row.get::<_,String>(0)==expected&&!row.get::<_,bool>(1)&&!row.get::<_,bool>(2)&&row.get::<_,bool>(3),"stable roles must be NOLOGIN, NOINHERIT and have no password");
            observed.push(json!({"name":expected,"login":false,"inherit":false,"password_is_null":true}));
        }
        save(resources,"bootstrap-roles.json",&json!(observed))?;
        Ok::<_,anyhow::Error>(())
    }.await;
    drop(admin);
    task.abort();
    applied?;
    let database_url = format!("postgres://postgres@127.0.0.1:{port}/{name}?sslmode=disable");
    let (project, task) = connect(&database_url).await?;
    let applied = async {
        project
            .batch_execute(&fs::read_to_string(&privilege_path)?)
            .await?;
        let row = project.query_one("SELECT current_database()", &[]).await?;
        ensure!(
            row.get::<_, String>(0) == name,
            "the privilege connection must use the emitted database"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    drop(project);
    task.abort();
    applied?;
    save(
        resources,
        "bootstrap-database.json",
        &json!({"name":name,"owner":"wamn_db_owner","current_database":name,"declaration":database}),
    )?;
    for (path, expected) in [
        (&secret_path, "wamn-db-rc--app--dev"),
        (&management_path, "wamn-pat-management-author-rc--app--dev"),
        (&route_path, "wamn-pat-route-caller-rc--app--dev"),
    ] {
        let secret: Value = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            secret["kind"] == "Secret"
                && secret["metadata"]["namespace"] == NAMESPACE
                && secret["metadata"]["name"] == expected,
            "the emitted credential must belong to the RC environment"
        );
        apply(resources, path).await?;
    }
    let secrets = command_json(kubectl(resources).args([
        "-n",
        NAMESPACE,
        "get",
        "secrets",
        "wamn-db-rc--app--dev",
        "wamn-pat-management-author-rc--app--dev",
        "wamn-pat-route-caller-rc--app--dev",
        "-o",
        "json",
    ]))
    .await?;
    let metadata = secrets["items"]
        .as_array()
        .context("the RC credentials form a Kubernetes list")?
        .iter()
        .map(|item| {
            json!({
                "name":item["metadata"]["name"],"namespace":item["metadata"]["namespace"],
                "managed_by":item["metadata"]["labels"]["app.kubernetes.io/managed-by"],
                "component":item["metadata"]["labels"]["app.kubernetes.io/component"],
            })
        })
        .collect::<Vec<_>>();
    save(
        resources,
        "bootstrap-secrets.json",
        &json!({"items":metadata}),
    )
}

fn validate_database(database: &Value) -> anyhow::Result<&str> {
    ensure!(
        database["apiVersion"] == "postgresql.cnpg.io/v1"
            && database["kind"] == "Database"
            && database["metadata"]["namespace"] == "wamn-system"
            && database["spec"]["cluster"]["name"] == "rc-pg"
            && database["spec"]["owner"] == "wamn_db_owner"
            && database["spec"]["ensure"] == "present"
            && database["spec"]["databaseReclaimPolicy"] == "retain",
        "the emitted Database must preserve the RC placement and owner"
    );
    let name = database["spec"]["name"]
        .as_str()
        .context("the Database names its database")?;
    let suffix = name
        .strip_prefix("wamn-db-rc--app--dev--")
        .context("the Database belongs to rc/app/dev")?;
    ensure!(
        suffix.len() == 8
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
        "the Database instance must have eight lowercase letters or digits"
    );
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn database_owner_and_environment_cannot_drift() {
        let valid = json!({"apiVersion":"postgresql.cnpg.io/v1","kind":"Database","metadata":{"namespace":"wamn-system"},"spec":{"name":"wamn-db-rc--app--dev--a1b2c3d4","owner":"wamn_db_owner","ensure":"present","databaseReclaimPolicy":"retain","cluster":{"name":"rc-pg"}}});
        assert!(validate_database(&valid).is_ok());
        for (path, value) in [
            ("/spec/owner", json!("postgres")),
            ("/spec/name", json!("wamn-db-other--app--dev--a1b2c3d4")),
            ("/spec/cluster/name", json!("other")),
        ] {
            let mut changed = valid.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(validate_database(&changed).is_err());
        }
    }
}
