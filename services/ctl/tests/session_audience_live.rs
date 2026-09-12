//! Exercise session-target publication through the compiled provisioning CLI.
//!
//! The ignored test requires a disposable PostgreSQL 18 server on wamn_system.
//! Set WAMN_SESSION_AUDIENCE_CLI_PG_URL and
//! WAMN_SESSION_AUDIENCE_CLI_ALLOW_SCHEMA_RESET=1. It replaces system schemas,
//! creates two fixture databases, and closes the cluster PUBLIC CONNECT floor.

mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;
use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_control_provision::session_target::{SESSION_TARGET_KEY, SessionTarget};
use wamn_control_provision::{
    CredentialGeneration, SYSTEM_SCHEMA_SQL, WorkloadRoleFamily, WorkloadRoleScope,
    project_env_database_name, sql, workload_generation_role,
};
use wamn_pg_core::quote_ident;

const ORG: &str = "sessioncli";
const PROJECT: &str = "receiving";
const ENVIRONMENT: &str = "dev";
const INSTANCE: &str = "k3m9x2p7";
const OTHER_INSTANCE: &str = "p7x2m9k3";
const TENANT: &str = "t1";

fn database(instance: &str) -> String {
    project_env_database_name(ORG, PROJECT, ENVIRONMENT, instance)
}

fn generation_role(generation: CredentialGeneration) -> String {
    workload_generation_role(
        WorkloadRoleFamily::SessionRoleReader,
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database: &database(INSTANCE),
        },
        generation,
    )
    .expect("session reader has project-environment scope")
}

fn database_url(raw: &str, name: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(raw).map_err(|_| anyhow::anyhow!("parse fixture database URL"))?;
    url.set_path(&format!("/{name}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .map_err(|_| anyhow::anyhow!("connect disposable session-target database"))?;
    tokio::spawn(connection);
    Ok(client)
}

async fn cli(
    system_url: &str,
    project_url: &str,
    tenant: &str,
    action: &str,
    generation: &str,
    output_path: Option<&Path>,
) -> anyhow::Result<Output> {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_wamn-ctl"));
    command
        .args([
            "provision-project-env",
            "--org",
            ORG,
            "--project",
            PROJECT,
            "--env",
            ENVIRONMENT,
            "--tenant",
            tenant,
            "--system-database-url",
            system_url,
            "--target-admin-database-url",
            project_url,
            action,
            generation,
        ])
        .env_remove("WAMN_APP_PASSWORD")
        .env_remove("WAMN_SYSTEM_ADMIN_URL")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = output_path {
        command.arg("--emit-session-role-reader-secret").arg(path);
    }
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .context("session-target CLI timed out")?
        .context("run session-target CLI")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for raw in [system_url, project_url] {
        let parsed = Url::parse(raw).map_err(|_| anyhow::anyhow!("parse fixture URL"))?;
        if let Some(password) = parsed.password().filter(|password| !password.is_empty()) {
            anyhow::ensure!(
                !stdout.contains(password) && !stderr.contains(password),
                "CLI exposed an administrator password"
            );
        }
    }
    Ok(output)
}

fn success(output: &Output) -> anyhow::Result<()> {
    anyhow::ensure!(
        output.status.success(),
        "session-target CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn refusal(output: &Output, reason: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !output.status.success(),
        "session-target CLI accepted a refused action"
    );
    anyhow::ensure!(
        String::from_utf8_lossy(&output.stderr).contains(reason),
        "session-target CLI reported another refusal: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn read_target(path: &Path, generation: CredentialGeneration) -> anyhow::Result<SessionTarget> {
    anyhow::ensure!(
        fs::metadata(path)?.permissions().mode() & 0o777 == 0o600,
        "session-target Secret is not private"
    );
    let secret: Value = serde_json::from_slice(&fs::read(path)?)?;
    anyhow::ensure!(
        secret["kind"] == "Secret"
            && secret["metadata"]["name"] == "wamn-session-role-reader-sessioncli--receiving--dev"
            && secret["metadata"]["namespace"] == "wamn-system",
        "session-target Secret has the wrong identity"
    );
    let data = secret["stringData"].as_object().context("Secret data")?;
    anyhow::ensure!(
        data.len() == 1,
        "Secret contains more than its target document"
    );
    let text = data[SESSION_TARGET_KEY]
        .as_str()
        .context("target document")?;
    let target = SessionTarget::from_json(text.as_bytes())?;
    anyhow::ensure!(
        target.audience() == "urn:wamn:project-env:sessioncli:receiving:dev:k3m9x2p7"
            && target.triple().org == ORG
            && target.triple().project == PROJECT
            && target.triple().env.as_str() == ENVIRONMENT
            && target.instance_suffix() == INSTANCE
            && target.tenant_id() == TENANT
            && target.connection().database() == database(INSTANCE)
            && target.connection().generation() == generation,
        "emitted target changed a trusted binding"
    );
    let mut tampered: Value = serde_json::from_str(text)?;
    tampered["instance_suffix"] = OTHER_INSTANCE.into();
    anyhow::ensure!(
        SessionTarget::from_json(&serde_json::to_vec(&tampered)?).is_err(),
        "target accepted a swapped instance"
    );
    Ok(target)
}

async fn inactive(admin: &Client, generation: CredentialGeneration) -> anyhow::Result<()> {
    let row = admin
        .query_one(
            sql::workload_generation_state_sql(),
            &[&generation_role(generation)],
        )
        .await?;
    anyhow::ensure!(
        !row.get::<_, bool>("rolcanlogin")
            && !row.get::<_, bool>("password_set")
            && row.get::<_, Vec<String>>("memberships").is_empty()
            && row.get::<_, Vec<String>>("connect_databases").is_empty()
            && row.get::<_, i64>("sessions") == 0,
        "failed or retired generation is still active"
    );
    Ok(())
}

async fn reset(admin: &Client) -> anyhow::Result<()> {
    for instance in [INSTANCE, OTHER_INSTANCE] {
        admin
            .batch_execute(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                quote_ident(&database(instance))
            ))
            .await?;
    }
    admin.batch_execute("DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS registry CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE;").await?;
    for role in [
        generation_role(CredentialGeneration::A),
        generation_role(CredentialGeneration::B),
        WorkloadRoleFamily::SessionRoleReader.acl_role().to_string(),
    ] {
        if admin
            .query_opt("SELECT 1 FROM pg_roles WHERE rolname = $1", &[&role])
            .await?
            .is_some()
        {
            admin
                .batch_execute(&format!(
                    "DROP OWNED BY {}; DROP ROLE {}",
                    quote_ident(&role),
                    quote_ident(&role)
                ))
                .await?;
        }
    }
    Ok(())
}

async fn setup(admin: &Client, system_url: &str) -> anyhow::Result<Client> {
    admin.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$; GRANT CREATE ON DATABASE wamn_system TO wamn_system;").await?;
    admin
        .batch_execute(&format!(
            "SET ROLE wamn_system; {SYSTEM_SCHEMA_SQL} RESET ROLE;"
        ))
        .await?;
    admin.batch_execute(&format!(r#"
        INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ('{ORG}', 'pooled', 'fixture');
        INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image)
          VALUES ('{ORG}','{ENVIRONMENT}','"own"',0,1,'1Gi','1','1Gi','postgres:18');
        INSERT INTO registry.projects (org,id) VALUES ('{ORG}','{PROJECT}');
        INSERT INTO registry.project_envs (org,project,env,secret_name,instance_suffix)
          VALUES ('{ORG}','{PROJECT}','{ENVIRONMENT}','fixture-db','{INSTANCE}');
    "#)).await?;
    for instance in [INSTANCE, OTHER_INSTANCE] {
        admin
            .batch_execute(&format!(
                "CREATE DATABASE {}",
                quote_ident(&database(instance))
            ))
            .await?;
    }
    let project = connect(&database_url(system_url, &database(INSTANCE))?).await?;
    project.batch_execute(&sql::ensure_app_role_sql("")).await?;
    project
        .batch_execute(include_str!("../../../deploy/sql/app-schema.sql"))
        .await?;
    project.batch_execute("CREATE SCHEMA catalog; CREATE SCHEMA wamn_run; CREATE TABLE catalog.probe (id int); CREATE TABLE wamn_run.probe (id int);").await?;
    project
        .batch_execute(&format!(
            "REVOKE TEMPORARY ON DATABASE {} FROM PUBLIC",
            quote_ident(&database(INSTANCE))
        ))
        .await?;
    project.batch_execute("INSERT INTO app_system.users (tenant_id,id,email) VALUES ('t1','11111111-1111-1111-1111-111111111111','one@example.invalid'),('t2','11111111-1111-1111-1111-111111111111','two@example.invalid'); INSERT INTO app_system.roles (tenant_id,name) VALUES ('t1','member'),('t2','other'); INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ('t1','11111111-1111-1111-1111-111111111111','member'),('t2','11111111-1111-1111-1111-111111111111','other');").await?;
    Ok(project)
}

async fn journey(admin: &Client, system_url: &str, directory: &Path) -> anyhow::Result<()> {
    let project = setup(admin, system_url).await?;
    let project_url = database_url(system_url, &database(INSTANCE))?;
    let a_path = directory.join("a.json");
    let b_path = directory.join("b.json");
    let previous = b"previous-output\n";
    fs::write(&a_path, previous)?;
    let inode = fs::metadata(&a_path)?.ino();
    refusal(
        &cli(
            system_url,
            &database_url(system_url, &database(OTHER_INSTANCE))?,
            TENANT,
            "--prepare-session-role-reader-generation",
            "a",
            Some(&a_path),
        )
        .await?,
        "exact project database",
    )?;
    anyhow::ensure!(
        fs::read(&a_path)? == previous,
        "refusal replaced the prior output"
    );
    let output = cli(
        system_url,
        &project_url,
        TENANT,
        "--prepare-session-role-reader-generation",
        "a",
        Some(&a_path),
    )
    .await?;
    success(&output)?;
    let a_target = read_target(&a_path, CredentialGeneration::A)?;
    anyhow::ensure!(
        fs::metadata(&a_path)?.ino() != inode,
        "target publication was not an atomic replacement"
    );
    let password = Url::parse(a_target.connection().url())?
        .password()
        .context("prepared password")?
        .to_owned();
    anyhow::ensure!(
        !String::from_utf8_lossy(&output.stdout).contains(&password)
            && !String::from_utf8_lossy(&output.stderr).contains(&password),
        "CLI exposed the prepared credential"
    );
    let a = connect(a_target.connection().url()).await?;
    let rows = a.query("SELECT r.role_name FROM app_system.users u JOIN app_system.user_roles r ON r.tenant_id=u.tenant_id AND r.user_id=u.id WHERE u.tenant_id=$1 AND u.status='active'", &[&TENANT]).await?;
    anyhow::ensure!(
        rows.len() == 1 && rows[0].get::<_, String>(0) == "member",
        "reader cannot load the target tenant's fresh roles"
    );
    for query in [
        "SELECT email FROM app_system.users",
        "SELECT * FROM app_system.user_roles",
        "SELECT * FROM app_system.permissions",
        "SELECT * FROM catalog.probe",
        "SELECT * FROM wamn_run.probe",
        "UPDATE app_system.users SET status='disabled'",
        "DELETE FROM app_system.user_roles",
    ] {
        let error = a
            .simple_query(query)
            .await
            .expect_err("reader accepted an ungranted operation");
        anyhow::ensure!(
            error.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
            "reader operation failed for a reason other than its ACL"
        );
    }
    anyhow::ensure!(
        connect(&database_url(
            a_target.connection().url(),
            &database(OTHER_INSTANCE)
        )?)
        .await
        .is_err(),
        "reader generation connected to another physical database"
    );
    let broken = directory.join("blocked-output");
    fs::create_dir(&broken)?;
    fs::write(broken.join("previous"), previous)?;
    refusal(
        &cli(
            system_url,
            &project_url,
            TENANT,
            "--prepare-session-role-reader-generation",
            "b",
            Some(&broken),
        )
        .await?,
        "install credential output",
    )?;
    inactive(&project, CredentialGeneration::B).await?;
    anyhow::ensure!(
        fs::read(broken.join("previous"))? == previous,
        "failed publication damaged prior data"
    );
    a.query_one("SELECT 1", &[]).await?;
    success(
        &cli(
            system_url,
            &project_url,
            TENANT,
            "--prepare-session-role-reader-generation",
            "b",
            Some(&b_path),
        )
        .await?,
    )?;
    let b_target = read_target(&b_path, CredentialGeneration::B)?;
    anyhow::ensure!(
        a_target.audience() == b_target.audience() && a_target.tenant_id() == b_target.tenant_id(),
        "generation rotation changed the audience binding"
    );
    let b = connect(b_target.connection().url()).await?;
    success(
        &cli(
            system_url,
            &project_url,
            TENANT,
            "--retire-session-role-reader-generation",
            "a",
            None,
        )
        .await?,
    )?;
    inactive(&project, CredentialGeneration::A).await?;
    anyhow::ensure!(
        a.simple_query("SELECT 1").await.is_err()
            && connect(a_target.connection().url()).await.is_err(),
        "retired reader generation remains usable"
    );
    b.query_one("SELECT 1", &[]).await?;
    drop(a);
    drop(b);
    drop(project);
    Ok(())
}

#[tokio::test]
async fn compiled_session_reader_refuses_invalid_tenants_before_io() {
    let url = "postgres://postgres:session-target-fixture-password@127.0.0.1:1/wamn_system";
    for tenant in ["bad/tenant", "", &"x".repeat(65)] {
        let output = cli(
            url,
            url,
            tenant,
            "--prepare-session-role-reader-generation",
            "a",
            Some(Path::new("/tmp/session-target-invalid-must-not-exist.json")),
        )
        .await
        .unwrap();
        refusal(&output, "tenant").unwrap();
        assert!(!String::from_utf8_lossy(&output.stderr).contains("connect"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly armed disposable PostgreSQL 18"]
async fn compiled_cli_publishes_bound_session_targets_and_rotates_reader_generations() {
    let _lock = support::lock();
    let url = std::env::var("WAMN_SESSION_AUDIENCE_CLI_PG_URL")
        .expect("set WAMN_SESSION_AUDIENCE_CLI_PG_URL");
    assert_eq!(
        std::env::var("WAMN_SESSION_AUDIENCE_CLI_ALLOW_SCHEMA_RESET").as_deref(),
        Ok("1")
    );
    assert_eq!(Url::parse(&url).expect("armed URL").path(), "/wamn_system");
    let admin = connect(&url)
        .await
        .expect("connect disposable administrator");
    let valid: bool = admin.query_one("SELECT current_database()='wamn_system' AND current_setting('server_version_num')::int BETWEEN 180000 AND 189999 AND rolsuper FROM pg_roles WHERE rolname=current_user", &[]).await.unwrap().get(0);
    assert!(
        valid,
        "fixture needs a PostgreSQL 18 system database administrator"
    );
    reset(&admin).await.expect("reset disposable fixture");
    let output = std::process::Command::new("mktemp")
        .args(["-d", "/tmp/wamn-session-audience-cli.XXXXXXXX"])
        .output()
        .expect("create test directory");
    assert!(output.status.success());
    let directory = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert!(
        directory.parent() == Some(Path::new("/tmp"))
            && directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("wamn-session-audience-cli.")
    );
    let result = journey(&admin, &url, &directory).await;
    let cleanup = reset(&admin).await;
    fs::remove_dir_all(&directory).expect("remove only the generated test directory");
    cleanup.expect("clean disposable session-target fixture");
    result.expect("compiled session-target publication test");
}
