//! The SQL Job's native worker waits for deployment before resolving reset inputs.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::json;
use wamn_environment_control::environment::{
    Platform, document::EnvironmentDocument, ensure, lock::LifecycleLock, sql::ExpectedDatabase,
};

#[tokio::test]
async fn reset_worker_waits_before_validation_and_pinning() -> anyhow::Result<()> {
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let system = postgres.create_database("wamn_system")?;
    let admin = wamn_package_control::database::connect(system.url()).await?;
    admin
        .batch_execute(
            "CREATE ROLE wamn_system NOLOGIN; ALTER DATABASE wamn_system OWNER TO wamn_system",
        )
        .await?;
    wamn_environment_control::provision_system::provision_system(
        &wamn_environment_control::provision_system::ProvisionSystemRequest {
            system_database_url: system.url().into(),
            platform_domain: "example.invalid".into(),
        },
    )
    .await?;
    admin.batch_execute(&format!(
        "BEGIN; SET LOCAL ROLE wamn_system; {} UPDATE registry.project_envs SET disposable=true; COMMIT;",
        wamn_control_provision::test_database::seed_org_project_env_sql("acme", "apps", "dev"),
    )).await?;
    admin
        .batch_execute(&wamn_environment_control::provision_project_env::role_posture_sql())
        .await?;
    let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":"acme", "project":"apps", "env":"dev", "target":"cloud", "policy":"dev",
        "applications":{"a":{"current":"none", "hostname":"a.invalid", "packages":["a"]}}
    }))?)?;
    let instance = wamn_environment_control::provision_project_env::read_project_env_instance(
        system.url(),
        &document.triple(),
    )
    .await?;
    let name = wamn_control_provision::project_env_database_name("acme", "apps", "dev", &instance);
    // CNPG supplies the database itself; this test exercises its native SQL worker.
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            wamn_pg_core::quote_ident(&name)
        ))
        .await?;
    let mut project_url = url::Url::parse(system.url())?;
    project_url.set_path(&name);
    let platform = Platform {
        system_database_url: system.url().into(),
        target: wamn_release_control::release_chart::Target {
            kubeconfig: PathBuf::new(),
            context: String::new(),
            namespace: "hosts".into(),
        },
        chart: PathBuf::new(),
        release_artifact_base: "registry.invalid/releases".into(),
        registry_auth_file: scratch.path().join("registry.json"),
        oci_ca_paths: Vec::new(),
        insecure_registry: false,
        package_artifact_base: None,
        component_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    ensure::ensure_project_database(&platform, &document, &[], &admin, system.url()).await?;
    let project_admin = wamn_package_control::database::connect(project_url.as_str()).await?;
    // Validation must see the state after the deploying application releases its lock.
    let baseline: String = project_admin
        .query_one("SELECT sha256 FROM platform.schema_baseline", &[])
        .await?
        .get(0);
    project_admin
        .execute(
            "UPDATE platform.schema_baseline SET sha256=$1",
            &[&format!("sha256:{}", "0".repeat(64))],
        )
        .await?;
    let file = scratch.path().join("environment.json");
    let expected = scratch.path().join("database.json");
    std::fs::write(&file, document.canonical_json()?)?;
    std::fs::write(
        &expected,
        serde_json::to_vec(&ExpectedDatabase::read(&platform, &document).await?)?,
    )?;
    let deploy = LifecycleLock::acquire_application(system.url(), &document.triple(), "a").await?;
    let mut worker_url = url::Url::parse(system.url())?;
    worker_url
        .query_pairs_mut()
        .append_pair("application_name", "reset-worker-lock-test");
    let mut worker = tokio::process::Command::new(env!("CARGO_BIN_EXE_wamn-ctl"))
        .args(["env", "reset"])
        .arg(&file)
        .arg(&expected)
        .env("WAMN_APPLY_DATABASE_URL", worker_url.as_str())
        .env("WAMN_SYSTEM_ADMIN_URL", system.url())
        .env(
            "WAMN_RELEASE_ARTIFACT_BASE",
            &platform.release_artifact_base,
        )
        .env("WAMN_REGISTRY_AUTH_FILE", &platform.registry_auth_file)
        .env("WAMN_NATS_URL", "tls://unused.invalid:4222")
        .env("WAMN_NATS_TLS_CA", scratch.path().join("unused-ca"))
        .env("WAMN_NATS_TLS_CERT", scratch.path().join("unused-cert"))
        .env("WAMN_NATS_TLS_KEY", scratch.path().join("unused-key"))
        .env_remove("WAMN_RELEASE_HOST_GROUP")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            assert!(worker.try_wait()?.is_none());
            let waiting: bool = admin.query_one(
                "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE application_name='reset-worker-lock-test' AND query LIKE '%pg_try_advisory_lock(%')", &[],
            ).await?.get(0);
            if waiting { break Ok::<_, anyhow::Error>(()); }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.context("wait for the worker's exclusive lock attempt")??;
    assert!(admin.query_one("SELECT pending_database IS NULL FROM registry.project_envs WHERE org='acme' AND project='apps' AND env='dev'", &[]).await?.get::<_, bool>(0));
    project_admin
        .execute(
            "UPDATE platform.schema_baseline SET sha256=$1",
            &[&baseline],
        )
        .await?;
    deploy.release().await;
    let output = tokio::time::timeout(Duration::from_secs(15), worker.wait_with_output()).await??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pinned: serde_json::Value = admin.query_one("SELECT pending_database FROM registry.project_envs WHERE org='acme' AND project='apps' AND env='dev'", &[]).await?.get(0);
    assert_eq!(pinned["inputs"]["project_baseline"], baseline);
    assert!(pinned["initialization_identity"].is_string());
    Ok(())
}
