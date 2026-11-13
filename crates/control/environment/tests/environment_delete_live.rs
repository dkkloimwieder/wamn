//! Environment deletion refusals and cancellation of the final transaction.

use std::time::Duration;

use serde_json::json;
use tokio_postgres::Client;
use wamn_control_registry::Triple;
use wamn_environment_control::{
    environment::{
        delete::{CONTROL_TABLES, delete_registry_rows},
        lock::LifecycleLock,
    },
    provision_project_env::provisioning_transaction,
};
use wamn_platform_identity::{
    auth_changes::{AuthChanges, AuthDrops},
    connection_changes::ConnectionDrops,
    connections::administration,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[tokio::test]
async fn stranded_run_refusal_preserves_the_environment_before_uninstall() -> anyhow::Result<()> {
    use std::{
        os::unix::fs::PermissionsExt as _,
        path::{Path, PathBuf},
    };
    use wamn_environment_control::environment::{Platform, delete::delete};
    use wamn_environment_control::reconcile_run_plane::{
        ReconcileRunPlaneRequest, reconcile_run_plane,
    };

    // Isolate PATH in a child test process; never change the test harness environment.
    if !Path::new("delete-mutations").exists() {
        let scratch = ScratchRoot::create()?;
        std::fs::write(scratch.path().join("delete-mutations"), "")?;
        for (name, body) in [
            (
                "kubectl",
                r#"#!/bin/sh
shift 6
case "$1 $2" in
  'get database') printf 'database/present\n' ;;
  'get --raw'|'get pods') printf '{"items":[]}\n' ;;
  get\ *) ;;
  *) printf '%s\n' "$*" >> delete-mutations ;;
esac
"#,
            ),
            (
                "helm",
                r#"#!/bin/sh
case "$1 $2" in
  list\ *) cat releases.json ;;
  'get values') cat values.json ;;
  *) printf '%s\n' "$*" >> delete-mutations ;;
esac
"#,
            ),
        ] {
            let path = scratch.path().join(name);
            std::fs::write(&path, body)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        }
        let status = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "stranded_run_refusal_preserves_the_environment_before_uninstall",
                "--nocapture",
            ])
            .current_dir(scratch.path())
            .env(
                "PATH",
                std::env::join_paths([scratch.path(), Path::new("/usr/bin"), Path::new("/bin")])?,
            )
            .status()
            .await?;
        assert!(status.success(), "environment deletion regression failed");
        return Ok(());
    }

    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut system = connect(database.url()).await?;
    system.batch_execute("SET ROLE wamn_system").await?;
    let triple = Triple::new("acme", "app", "dev");
    let transaction = provisioning_transaction(&mut system).await?;
    transaction
        .batch_execute(
            &wamn_control_provision::test_database::seed_org_project_env_sql(
                &triple.org,
                &triple.project,
                &triple.env,
            ),
        )
        .await?;
    transaction.commit().await?;
    wamn_environment_control::provision_system::set_platform_domain(&mut system, "example.invalid")
        .await?;
    system.batch_execute("RESET ROLE").await?;
    let project_name = wamn_control_provision::project_env_database_name(
        &triple.org,
        &triple.project,
        &triple.env,
        "aaaaaaa1",
    );
    system
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&project_name)
        ))
        .await?;
    let mut project_url = url::Url::parse(database.url())?;
    project_url.set_path(&project_name);
    reconcile_run_plane(ReconcileRunPlaneRequest {
        system_database_url: database.url().into(),
        admin_database_url: project_url.to_string(),
        org: triple.org.clone(),
        project: triple.project.clone(),
        env: triple.env.to_string(),
        schema: "wamn_run".into(),
        dry_run: false,
        changes: AuthChanges::default(),
    })
    .await?;
    // The database-owner policy membership installed by environment::ensure.
    system
        .batch_execute("GRANT wamn_platform TO wamn_db_owner WITH INHERIT TRUE, SET FALSE")
        .await?;
    let mut project = connect(project_url.as_str()).await?;
    let env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    let bytes = b"stranded release";
    let digest = wamn_execution_contract::bytes_sha256(bytes);
    let transaction = project.transaction().await?;
    transaction
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await?;
    transaction.execute("INSERT INTO catalog.releases(env_key,manifest_digest,canonical_bytes) VALUES($1,$2,$3)", &[&env_key, &digest, &&bytes[..]]).await?;
    transaction.execute("INSERT INTO wamn_run.runs(env_key,run_id,package_id,environment,status,manifest_digest,wiring_id,wiring_version,wiring_hash,binding_world_json) VALUES($1,'stranded-work','app',$2,'dispatched',$3,'work',1,$3,'[]')", &[&env_key, &triple.env.as_str(), &digest]).await?;
    transaction.commit().await?;
    let run_before: serde_json::Value = project
        .query_one("SELECT to_jsonb(r) FROM wamn_run.runs r", &[])
        .await?
        .get(0);
    let release = wamn_control_provision::environment_state::application_release_name(
        &triple.org,
        &triple.project,
        &triple.env,
        "app",
    )?;
    std::fs::write(
        "releases.json",
        serde_json::to_vec(&json!([{"name":release}]))?,
    )?;
    std::fs::write(
        "values.json",
        serde_json::to_vec(&json!({
            "environment":triple.to_string(),
            "applications":{"app":{"packages":["app"]}}
        }))?,
    )?;
    let platform = Platform {
        system_database_url: database.url().into(),
        target: wamn_release_control::release_chart::Target {
            kubeconfig: "unused".into(),
            context: "stub".into(),
            namespace: "hosts".into(),
        },
        chart: PathBuf::new(),
        release_artifact_base: String::new(),
        registry_auth_file: PathBuf::new(),
        registry_token_metadata: false,
        oci_ca_paths: Vec::new(),
        insecure_registry: false,
        package_artifact_base: None,
        component_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: project_url.port().unwrap(),
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    system.batch_execute("SET ROLE wamn_system").await?;
    let before: serde_json::Value = system
        .query_one("SELECT to_jsonb(e) FROM registry.project_envs e", &[])
        .await?
        .get(0);
    let error = delete(&platform, &triple, false, database.url())
        .await
        .unwrap_err();
    for field in ["run_id", "manifest_digest"] {
        assert!(
            error
                .to_string()
                .contains(run_before[field].as_str().unwrap()),
            "{error:#}"
        );
    }
    assert!(
        std::fs::read("delete-mutations")?.is_empty(),
        "refusal must precede every external mutation"
    );
    let after: serde_json::Value = system
        .query_one("SELECT to_jsonb(e) FROM registry.project_envs e", &[])
        .await?
        .get(0);
    assert_eq!(after, before);
    let run_after: serde_json::Value = project
        .query_one("SELECT to_jsonb(r) FROM wamn_run.runs r", &[])
        .await?
        .get(0);
    assert_eq!(run_after, run_before);
    Ok(())
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
    tokio::spawn(connection);
    Ok(client)
}

#[tokio::test]
async fn cancellation_retains_registry_inputs_until_cleanup_commits() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut system = connect(database.url()).await?;
    system.batch_execute("SET ROLE wamn_system").await?;
    let triple = Triple::new("acme", "app", "dev");
    let other = Triple::new("acme", "app", "prod");
    let transaction = provisioning_transaction(&mut system).await?;
    for scope in [&triple, &other] {
        transaction
            .batch_execute(
                &wamn_control_provision::test_database::seed_org_project_env_sql(
                    &scope.org,
                    &scope.project,
                    &scope.env,
                ),
            )
            .await?;
        let env_key =
            wamn_control_provision::project_env_key(&scope.org, &scope.project, &scope.env);
        transaction
            .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
            .await?;
        transaction.execute(
            "INSERT INTO catalog.authoring_command_audit \
             (env_key,command_id,command_type,principal_id,principal_type,principal_subject,effective_role,org,project,environment,target_ref,request_hash,outcome_bytes) \
             VALUES ($1,'delete-fixture','gate','fixture','service','fixture','project-author',$2,$3,$4,'fixture','sha256:'||repeat('a',64),'{}'::bytea)",
            &[&env_key, &scope.org, &scope.project, &scope.env.as_str()],
        ).await?;
        transaction
            .execute(
                "INSERT INTO wamn_run.gate_reports (env_key,wiring_hash,passed,summary) \
             VALUES ($1,'sha256:'||repeat('a',64),true,'{}')",
                &[&env_key],
            )
            .await?;
    }
    transaction.batch_execute(
        "INSERT INTO registry.target(name,type,status) VALUES ('delete-fixture','local','active'); \
         UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='delete-fixture')",
    ).await?;
    let scratch = ScratchRoot::create()?;
    let store = wamn_credential_store::file::FileStore::new(scratch.path().to_path_buf());
    let mut auth = AuthDrops::default();
    let mut changes = ConnectionDrops::default();
    for (operation, request) in [
        (
            "integration.write",
            json!({"project":"app","name":"erp","integration_type":"http","auth_type":"http_basic"}),
        ),
        (
            "connection.write",
            json!({"project":"app","env":"dev","name":"erp","parameters":{"primary-authority":"https://example.invalid","tls-verification":"disabled"},"credential":"{\"username\":\"fixture\",\"password\":\"private\"}"}),
        ),
    ] {
        administration::execute_with_store(
            &transaction,
            "acme",
            operation,
            &request.to_string(),
            &mut auth,
            &mut changes,
            Some(&store),
        )
        .await?;
    }
    transaction.commit().await?;
    let credential = system
        .query_one(
            "SELECT id::text, ref, version FROM identity.credential",
            &[],
        )
        .await?;
    let id: String = credential.get(0);
    let reference: String = credential.get(1);
    let version: String = credential.get(2);
    assert_control_facts(&mut system, &triple, true).await?;
    assert_control_facts(&mut system, &other, true).await?;

    let mut blocker = connect(database.url()).await?;
    let blocker_pid: i32 = blocker
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let held = blocker.transaction().await?;
    held.batch_execute("LOCK TABLE catalog.tenant_environments IN SHARE MODE")
        .await?;
    let mut deleting = connect(database.url()).await?;
    deleting.batch_execute("SET ROLE wamn_system").await?;
    let deleting_pid: i32 = deleting
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let url = database.url().to_owned();
    let deleting_triple = triple.clone();
    let task = tokio::spawn(async move {
        LifecycleLock::hold(&url, &deleting_triple, async |_| {
            delete_registry_rows(&mut deleting, &deleting_triple, "aaaaaaa1", Some(&store)).await
        })
        .await
    });
    // PostgreSQL stops the real delete after connection removal, before commit.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = system
                .query_one(
                    "SELECT $1 = ANY(pg_blocking_pids($2))",
                    &[&blocker_pid, &deleting_pid],
                )
                .await?
                .get(0);
            if waiting {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert!(!scratch.path().join(&reference).exists());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    held.rollback().await?;
    // Wait for cancellation to close the database connection and roll back.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if system
                .query_opt(
                    "SELECT 1 FROM pg_stat_activity WHERE pid=$1",
                    &[&deleting_pid],
                )
                .await?
                .is_none()
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert!(system.query_opt("SELECT 1 FROM registry.project_envs WHERE org='acme' AND project='app' AND env='dev'", &[]).await?.is_some());
    let retained = system
        .query_one(
            "SELECT ref, version FROM identity.credential WHERE id=$1::text::uuid",
            &[&id],
        )
        .await?;
    assert_eq!(retained.get::<_, String>(0), reference);
    assert_eq!(retained.get::<_, String>(1), version);
    assert_control_facts(&mut system, &triple, true).await?;
    let store = wamn_credential_store::file::FileStore::new(scratch.path().to_path_buf());
    for _ in 0..2 {
        LifecycleLock::hold(database.url(), &triple, async |_| {
            delete_registry_rows(&mut system, &triple, "aaaaaaa1", Some(&store)).await
        })
        .await?;
    }
    assert!(system.query_opt("SELECT 1 FROM registry.project_envs WHERE org='acme' AND project='app' AND env='dev'", &[]).await?.is_none());
    assert!(
        system
            .query_opt(
                "SELECT 1 FROM identity.credential WHERE id=$1::text::uuid",
                &[&id]
            )
            .await?
            .is_none()
    );
    assert!(system.query_opt("SELECT 1 FROM registry.connection WHERE org='acme' AND project='app' AND env='dev'", &[]).await?.is_none());
    assert_control_facts(&mut system, &triple, false).await?;
    assert_control_facts(&mut system, &other, true).await?;
    Ok(())
}

async fn assert_control_facts(
    system: &mut Client,
    triple: &Triple,
    present: bool,
) -> anyhow::Result<()> {
    let env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    for (table, trigger) in CONTROL_TABLES {
        let transaction = provisioning_transaction(system).await?;
        transaction
            .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
            .await?;
        assert_eq!(
            transaction
                .query_opt(
                    &format!("SELECT 1 FROM {table} WHERE env_key=$1"),
                    &[&env_key]
                )
                .await?
                .is_some(),
            present,
            "control fact in {table} for {triple}"
        );
        if present && trigger.is_some() {
            let error = transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE env_key=$1"),
                    &[&env_key],
                )
                .await
                .expect_err("control facts remain immutable outside environment deletion");
            assert_eq!(
                error.code(),
                Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
            );
        }
        transaction.rollback().await?;
    }
    Ok(())
}
