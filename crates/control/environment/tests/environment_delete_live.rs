//! Environment deletion refusals and cancellation of the final transaction.

use sqlx::{Connection as _, Row as _};
use std::time::Duration;

use serde_json::json;
use sqlx::PgConnection as Client;
use wamn_broker::{Publisher, auth_changes::AuthDrops, connection_changes::ConnectionDrops};
use wamn_environment_control::{
    environment::{delete::delete_registry_rows, lock::LifecycleLock},
    provision_project_env::provisioning_transaction,
};
use wamn_platform_identity::connections::administration;
use wamn_registry::Triple;
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
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut system)
        .await
        .map(drop)?;
    let triple = Triple::new("acme", "dev", "app");
    let mut transaction = provisioning_transaction(&mut system).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::seed_org_project_env_sql(
            &triple.org,
            &triple.project,
            &triple.env,
        ))
            .to_string(),
    ))
    .execute(&mut *transaction)
    .await
    .map(drop)?;
    transaction.commit().await?;
    wamn_environment_control::provision_system::set_platform_domain(&mut system, "example.invalid")
        .await?;
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut system)
        .await
        .map(drop)?;
    let project_name = wamn_control_provision::project_env_database_name(
        &triple.org,
        &triple.project,
        &triple.env,
        "aaaaaaa1",
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&project_name)
        ))
            .to_string(),
    ))
    .execute(&mut system)
    .await
    .map(drop)?;
    let mut project_url = url::Url::parse(database.url())?;
    project_url.set_path(&project_name);
    reconcile_run_plane(ReconcileRunPlaneRequest {
        platform_migration_database_url: Some(
            wamn_control_provision::test_database::platform_migration_input(
                &project_url,
                wamn_control_provision::migrations::MigrationDatabase::Project,
            )
            .await
            .unwrap(),
        ),
        system_database_url: database.url().into(),
        admin_database_url: project_url.to_string(),
        org: triple.org.clone(),
        project: triple.project.clone(),
        env: triple.env.to_string(),
        schema: "wamn_run".into(),
        dry_run: false,
        changes: Publisher::default(),
    })
    .await?;
    // The database-owner policy membership installed by environment::ensure.
    sqlx::raw_sql("GRANT wamn_platform TO wamn_db_owner WITH INHERIT TRUE, SET FALSE")
        .execute(&mut system)
        .await
        .map(drop)?;
    let mut project = connect(project_url.as_str()).await?;
    let project_env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    let bytes = b"stranded release";
    let digest = wamn_digest::bytes_sha256(bytes);
    let mut transaction = project.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *transaction)
    .await
    .map(drop)?;
    sqlx::query("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES($1,$2,$3)").bind(&project_env_key).bind(&digest).bind(&&bytes[..]).execute(&mut *transaction).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO wamn_run.runs(project_env_key,run_id,package_id,environment,status,manifest_digest,wiring_id,wiring_version,wiring_hash,binding_world_json) VALUES($1,'stranded-work','app',$2,'dispatched',$3,'work',1,$3,'[]')").bind(&project_env_key).bind(&triple.env.as_str()).bind(&digest).execute(&mut *transaction).await.map(|result| result.rows_affected())?;
    transaction.commit().await?;
    let run_before: serde_json::Value = sqlx::query("SELECT to_jsonb(r) FROM wamn_run.runs r")
        .fetch_one(&mut project)
        .await?
        .get(0);
    let release = wamn_control_provision::environment_names::application_release_name(
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
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut system)
        .await
        .map(drop)?;
    let before: serde_json::Value =
        sqlx::query("SELECT to_jsonb(e) FROM registry.environment_projects e")
            .fetch_one(&mut system)
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
    let after: serde_json::Value =
        sqlx::query("SELECT to_jsonb(e) FROM registry.environment_projects e")
            .fetch_one(&mut system)
            .await?
            .get(0);
    assert_eq!(after, before);
    let run_after: serde_json::Value = sqlx::query("SELECT to_jsonb(r) FROM wamn_run.runs r")
        .fetch_one(&mut project)
        .await?
        .get(0);
    assert_eq!(run_after, run_before);
    Ok(())
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

#[tokio::test]
async fn cancellation_retains_registry_inputs_until_cleanup_commits() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut system = connect(database.url()).await?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut system)
        .await
        .map(drop)?;
    let triple = Triple::new("acme", "dev", "app");
    let other = Triple::new("acme", "prod", "app");
    let mut transaction = provisioning_transaction(&mut system).await?;
    for target in [&triple, &other] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::seed_org_project_env_sql(
                &target.org,
                &target.project,
                &target.env,
            ))
                .to_string(),
        ))
        .execute(&mut *transaction)
        .await
        .map(drop)?;
        let project_env_key =
            wamn_control_provision::project_env_key(&target.org, &target.project, &target.env);
        sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
            .bind(&project_env_key)
            .fetch_one(&mut *transaction)
            .await?;
    }
    sqlx::raw_sql("INSERT INTO registry.target(name,type,status) VALUES ('delete-fixture','local','active'); \
         UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='delete-fixture')").execute(&mut *transaction).await.map(drop)?;
    let store = wamn_test_infrastructure::credentials::MemoryStore::default();
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
            &mut transaction,
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
    let credential = sqlx::query("SELECT id::text, ref, version FROM identity.credential")
        .fetch_one(&mut system)
        .await?;
    let id: String = credential.get(0);
    let reference: String = credential.get(1);
    let version: String = credential.get(2);

    let mut blocker = connect(database.url()).await?;
    let blocker_pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await?
        .get(0);
    let mut held = blocker.begin().await?;
    sqlx::raw_sql("LOCK TABLE wamn_authority.author_login_projects IN SHARE MODE")
        .execute(&mut *held)
        .await
        .map(drop)?;
    let mut deleting = connect(database.url()).await?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut deleting)
        .await
        .map(drop)?;
    let deleting_pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut deleting)
        .await?
        .get(0);
    let url = database.url().to_owned();
    let deleting_triple = triple.clone();
    let task = tokio::spawn(async move {
        LifecycleLock::hold(&url, &deleting_triple, async |_| {
            delete_registry_rows(&mut deleting, &deleting_triple, "aaaaaaa1").await
        })
        .await
    });
    // PostgreSQL stops the real delete after connection removal, before commit.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query("SELECT $1 = ANY(pg_blocking_pids($2))")
                .bind(&blocker_pid)
                .bind(&deleting_pid)
                .fetch_one(&mut system)
                .await?
                .get(0);
            if waiting {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert!(task.await.unwrap_err().is_cancelled());
    held.rollback().await?;
    // Wait for cancellation to close the database connection and roll back.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if sqlx::query("SELECT 1 FROM pg_stat_activity WHERE pid=$1")
                .bind(&deleting_pid)
                .fetch_optional(&mut system)
                .await?
                .is_none()
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert!(sqlx::query("SELECT 1 FROM registry.environment_projects WHERE org='acme' AND project='app' AND env='dev'").fetch_optional(&mut system).await?.is_some());
    let retained =
        sqlx::query("SELECT ref, version FROM identity.credential WHERE id=$1::text::uuid")
            .bind(&id)
            .fetch_one(&mut system)
            .await?;
    assert_eq!(retained.get::<String, _>(0), reference);
    assert_eq!(retained.get::<String, _>(1), version);
    for _ in 0..2 {
        LifecycleLock::hold(database.url(), &triple, async |_| {
            delete_registry_rows(&mut system, &triple, "aaaaaaa1").await
        })
        .await?;
    }
    assert!(sqlx::query("SELECT 1 FROM registry.environment_projects WHERE org='acme' AND project='app' AND env='dev'").fetch_optional(&mut system).await?.is_none());
    assert!(
        sqlx::query("SELECT 1 FROM identity.credential WHERE id=$1::text::uuid")
            .bind(&id)
            .fetch_optional(&mut system)
            .await?
            .is_none()
    );
    assert!(
        sqlx::query(
            "SELECT 1 FROM registry.connection WHERE org='acme' AND project='app' AND env='dev'"
        )
        .fetch_optional(&mut system)
        .await?
        .is_none()
    );
    assert!(
        sqlx::query(
            "SELECT 1 FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3"
        )
        .bind(&other.org)
        .bind(&other.project)
        .bind(&other.env.as_str())
        .fetch_optional(&mut system)
        .await?
        .is_some()
    );

    Ok(())
}
