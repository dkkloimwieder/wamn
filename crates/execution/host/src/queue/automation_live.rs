//! Operator admission through the real queue, router, and scoped database roles.

use super::{QUEUE_CLAIM_SCOPE, QueueScope, QueueService, QueueServiceConfig, drain_one};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio_postgres::NoTls;
use wamn_catalog::{ComponentDeclaration, ServingManifest, WiringDocument};
use wamn_control::enqueue_run::{EnqueueRun, enqueue};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_run_state::RunStore as _;
use wamn_runtime::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_runtime::component_artifact_source::{ComponentArtifactSource, local_component_path};
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_jetstream::WamnJetstream;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, ClassCredentials, ReleaseIdentity, SessionClaims, WamnPostgres,
    WamnPostgresConfig,
};
use wamn_runtime::release_manifest::LoadedRelease;
use wamn_schema_control::BareSchemaName;
use wash_runtime::host::probes::Liveness;

use crate::{RouterDriver, RouterDriverConfig, WiringCacheCapacity};
use wamn_project_state::PlatformComponent;

#[path = "automation_live/shutdown.rs"]
mod shutdown;

const TENANT: &str = "automation-live";
const SERVICE: &str = "00000000-0000-4000-8000-000000000074";
const OPERATION: &str = "automation:echo/run@1.0.0";
const HASH: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const NODE_TYPES: &str = r#"
      (import "wamn:node/types@0.1.0" (instance $node
        (type $json' string)
        (export "json" (type $json (eq $json')))
        (type $context' (record
          (field "wiring-id" string) (field "wiring-version" u32)
          (field "node-id" string) (field "delivery-id" string)
          (field "input-port" (option string)) (field "occurrence" u32)
          (field "traceparent" (option string)) (field "tracestate" (option string))
          (field "deadline-ms" (option u64)) (field "config" $json)))
        (export "node-context" (type $context (eq $context')))
        (type $detail' (record (field "message" string) (field "code" (option string))))
        (export "error-detail" (type $detail (eq $detail')))
        (type $rate' (record (field "detail" $detail) (field "retry-after-ms" (option u64))))
        (export "rate-limit-detail" (type $rate (eq $rate')))
        (type $error' (variant (case "retryable" $detail) (case "rate-limited" $rate)
          (case "terminal" $detail) (case "invalid-input" $detail) (case "cancelled")))
        (export "node-error" (type $error (eq $error')))
        (type $emission' (record (field "payload" $json) (field "port" (option string))))
        (export "emission" (type $emission (eq $emission')))))
      (alias export $node "json" (type $json))
      (alias export $node "node-context" (type $context))
      (alias export $node "node-error" (type $error))
      (alias export $node "emission" (type $emission))
"#;

fn component_bytes() -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
      {NODE_TYPES}
      (core module $memory
        (memory (export "memory") 16)
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
          (local $old i32) global.get $next local.tee $old
          local.get $size i32.add global.set $next local.get $old))
      (core instance $memory (instantiate $memory))
      (core module $main
        (import "memory" "memory" (memory 16))
        (func (export "run") (param $input i32) (result i32)
          local.get $input i32.load offset=100 i32.const 4 i32.eq
          if unreachable end
          i32.const 264 local.get $input i32.load offset=96 i32.store
          i32.const 268 local.get $input i32.load offset=100 i32.store
          i32.const 256))
      (core instance $main (instantiate $main (with "memory" (instance $memory))))
      (func $run (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory")
          (realloc (func $memory "realloc"))))
      (instance $handler
        (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission))
        (export "run" (func $run)))
      (export "{OPERATION}" (instance $handler)))"#
    ))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_admission_delivers_with_current_service_permissions() -> anyhow::Result<()> {
    run_automation(None).await
}

async fn run_automation(shutdown_signal: Option<&str>) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (mut admin, connection) = tokio_postgres::connect(database.url(), NoTls).await?;
    tokio::spawn(connection);
    let schema = BareSchemaName::new("wamn_run")?;
    admin
        .batch_execute("CREATE SCHEMA application_data")
        .await?;
    wamn_control::reconcile_run_plane::reconcile(&admin, &schema, true).await?;
    admin.batch_execute("ALTER TABLE wamn_run.runs DROP COLUMN service_principal_id CASCADE; ALTER TABLE wamn_run.runs ALTER COLUMN run_id DROP DEFAULT").await?;
    wamn_control::reconcile_run_plane::reconcile(&admin, &schema, true).await?;
    let converged = wamn_control::reconcile_run_plane::reconcile(&admin, &schema, false).await?;
    assert!(
        converged.actions.is_empty(),
        "run schema did not converge: {converged:?}"
    );
    admin
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await?;
    admin
        .batch_execute(&wamn_control_provision::platform_principals_sql(
            TENANT,
            "automation.invalid",
        )?)
        .await?;
    admin.execute("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:automation-fixture',false)", &[&PlatformComponent::Provisioning.principal_id().to_string()]).await?;
    admin.batch_execute(&format!("INSERT INTO app_system.users (tenant_id,id,type,email) VALUES ('{TENANT}','{SERVICE}','service','automation@example.invalid');
      INSERT INTO app_system.roles (tenant_id,name) VALUES ('{TENANT}','automation');
      INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ('{TENANT}','{SERVICE}','automation');
      INSERT INTO app_system.permissions (tenant_id,role_name,permission) VALUES ('{TENANT}','automation','{OPERATION}');
      INSERT INTO wamn_run.environment_policies (tenant_id,expected_environment,durability_class) VALUES ('{TENANT}','test','standard');
      INSERT INTO catalog.packages (tenant_id,package_id,package_version,manifest_sha256) VALUES ('{TENANT}','automation','1.0.0','{HASH}');
      INSERT INTO catalog.effective_releases (tenant_id,effective_release_id,environment,verified_publisher_principal) VALUES ('{TENANT}',1,'test','automation-fixture');
      INSERT INTO catalog.effective_release_packages (tenant_id,effective_release_id,package_id,package_version) VALUES ('{TENANT}',1,'automation','1.0.0');")).await?;
    let engine = Arc::new(build_engine(&[])?);
    let bytes = if shutdown_signal.is_some() {
        shutdown::component_bytes()
    } else {
        component_bytes()
    };
    let declaration: ComponentDeclaration = serde_json::from_value(json!({
        "scope": {"tenant-id":TENANT,"package-id":"automation","package-version":"1.0.0"},
        "component":"echo","interface-version":"0.1.0",
        "operations":{(OPERATION):{"registered-operation":OPERATION,
            "input-ports":[{"name":"input","schema":{}}],
            "output-ports":[{"name":"main","schema":{}}], "parameters":[{"name":"deadline-ms","schema":{"type":"integer"},"required":false}]}}, "connections":[]
    }))?;
    let admitted = validate_component_admission(
        &engine,
        &bytes,
        ComponentAdmissionRequest {
            declaration,
            admitted_platform_packages: BTreeSet::from(["wamn:node".to_owned()]),
            effect_free_operation_dependencies: BTreeSet::new(),
        },
    )?
    .component;
    let document = WiringDocument::parse(&json!({
        "format-version":"0.1", "wiring-id":"echo", "version":1,"entry":"echo",
        "nodes":{"echo":{"component":"echo","interface-version":"0.1.0","operation":OPERATION,"params":{"deadline-ms":60000}}},
        "edges":[],"cases":[]
    }))?;
    let graph_hash = document.wiring_hash();
    let transaction = admin.transaction().await?;
    wamn_control::push_component::append_or_verify_admitted_component(
        &transaction,
        &admitted,
        &wamn_control::push_component::admitted_projection_hash(&admitted, &[])?,
    )
    .await?;
    transaction.commit().await?;
    admin.execute("INSERT INTO catalog.wirings (tenant_id,package_id,package_version,wiring_id,version,graph_json,wiring_hash) VALUES ($1,'automation','1.0.0','echo',1,$2::text::jsonb,$3)",
        &[&TENANT,&serde_json::to_string(&document)?,&graph_hash.as_str()]).await?;
    admin.execute("INSERT INTO catalog.release_components (tenant_id,effective_release_id,wiring_package_id,wiring_package_version,wiring_id,wiring_version,node_id,package_id,package_version,component_digest) VALUES ($1,1,'automation','1.0.0','echo',1,'echo','automation','1.0.0',$2)", &[&TENANT,&admitted.component_digest]).await?;
    let manifest: ServingManifest = serde_json::from_value(json!({
        "format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        "release":{"tenant-id":TENANT,"effective-release-id":1,"environment":"test","packages":[{"package-id":"automation","package-version":"1.0.0"}]},
        "components":[{"package-id":"automation","component":"echo","interface-version":"0.1.0","digest":admitted.component_digest,
          "operations":{(OPERATION):{"registered-operation":OPERATION,"fresh-only":false,"dependencies":[],"statements":{}}}}],
        "routes":[],"wirings":[{"package-id":"automation","wiring-id":"echo","wiring-version":1,"graph-hash":graph_hash.as_str()}],"attachments":{},"registrations":{}
    }))?;
    let canonical = manifest.canonical_bytes();
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &canonical,
        "automation-live",
    )?);
    admin.execute("INSERT INTO catalog.release_manifest_v3_snapshots (tenant_id,effective_release_id,manifest_digest,canonical_bytes) VALUES ($1,1,$2,$3)",
        &[&TENANT,&release.release().manifest_digest.as_str(),&canonical]).await?;
    let mut credentials = ClassCredentials::default();
    let db_name: String = admin
        .query_one("SELECT current_database()", &[])
        .await?
        .get(0);
    for (family, class) in [
        (WorkloadRoleFamily::App, AuthorityClass::GuestSql),
        (
            WorkloadRoleFamily::ExecutorPlatform,
            AuthorityClass::ExecutorPlatform,
        ),
        (
            WorkloadRoleFamily::HttpAdmitter,
            AuthorityClass::CallableHttp,
        ),
    ] {
        let role_scope = if family == WorkloadRoleFamily::App {
            WorkloadRoleScope::Tenant {
                tenant: TENANT,
                database: &db_name,
            }
        } else {
            WorkloadRoleScope::ProjectEnvironment {
                org: TENANT,
                project: "default",
                environment: "test",
                database: &db_name,
            }
        };
        let role = workload_generation_role(family, role_scope, CredentialGeneration::A)?;
        admin
            .batch_execute(&sql::prepare_workload_generation_sql(
                family,
                &db_name,
                &role,
                "automation-test-only",
                "2099-01-01T00:00:00Z",
            ))
            .await?;
        // Ambient application lookup must not select the queue's storage.
        admin
            .batch_execute(&format!(
                "ALTER ROLE \"{role}\" SET search_path = application_data"
            ))
            .await?;
        let mut url = url::Url::parse(database.url())?;
        url.set_username(&role).unwrap();
        url.set_password(Some("automation-test-only")).unwrap();
        credentials = credentials.with_class(class, url.to_string());
    }
    let postgres = Arc::new(WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(credentials),
        guest_pool_max_size: 2,
        platform_pool_max_size: 2,
        wait_timeout_ms: 5000,
        statement_timeout_ms: 10000,
        row_limit: 100,
    })?);
    postgres
        .bind_session_claims(
            QUEUE_CLAIM_SCOPE,
            &SessionClaims {
                tenant: TENANT.to_owned(),
                project: Some("default".to_owned()),
                schema: Some("wamn_run".to_owned()),
                runner: Some("automation-live".to_owned()),
                user_id: Some(PlatformComponent::Executor.principal_id().to_string()),
                operation: Some(PlatformComponent::Executor.principal_name().to_owned()),
                release: Some(ReleaseIdentity {
                    effective_release_id: 1,
                    manifest_digest: release.release().manifest_digest.clone(),
                }),
                ..SessionClaims::default()
            },
        )
        .await?;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    std::fs::write(
        local_component_path(scratch.path(), &admitted.component_digest)?,
        bytes,
    )?;
    let (logging, capture) = WamnLogging::new_with_capture(
        &wamn_runtime::plugins::wamn_logging::WamnLoggingConfig::default(),
    )?;
    let logging = Arc::new(logging);
    let driver = Arc::new(RouterDriver::new(
        Arc::clone(&engine),
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new()?),
        Arc::new(WamnCredentials::empty()),
        Arc::clone(&logging),
        Arc::from([]),
        Arc::clone(&release),
        ComponentArtifactSource::local(scratch.path().to_owned()),
        RouterDriverConfig {
            warm_reuse: crate::warm_reuse::WarmReuse::default(),
            owner_prefix: "automation-live".to_owned(),
            project: "default".to_owned(),
            schema: Some("application_data".to_owned()),
            cache_capacity: WiringCacheCapacity::default(),
        },
    )?);
    let scope = QueueScope {
        tenant_id: TENANT.to_owned(),
        project: "default".to_owned(),
        package_ids: vec!["automation".to_owned()],
        environment: "test".to_owned(),
    };
    let jetstream = Arc::new(WamnJetstream::from_env());
    let liveness = Liveness::new(Duration::from_secs(90));
    let mut request = EnqueueRun {
        tenant: TENANT.to_owned(),
        environment: "test".to_owned(),
        package_id: "automation".to_owned(),
        effective_release_id: 1,
        wiring_id: "echo".to_owned(),
        wiring_version: 1,
        service_principal_id: SERVICE.to_owned(),
        idempotency_key: "first".to_owned(),
        input: json!({"queued":true}),
    };
    if shutdown_signal.is_some() {
        return shutdown::run(
            &mut admin,
            &schema,
            &request,
            shutdown::Execution {
                driver: &driver,
                postgres: &postgres,
                engine: &engine,
                logging: &logging,
                capture: &capture,
                scope: &scope,
                jetstream: &jetstream,
            },
        )
        .await;
    }
    let first = QueueService::bind(
        Arc::clone(&driver),
        Arc::clone(&postgres),
        Arc::clone(&jetstream),
        &release,
        QueueServiceConfig {
            project: "default".to_owned(),
            runner: "automation-live-first".to_owned(),
            lease_ttl_ms: 30_000,
        },
    )
    .await?;
    let run = enqueue(&mut admin, &schema, &request).await?;
    assert!(admin.query_one(
        "SELECT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id=$1) AND to_regclass('application_data.run_queue') IS NULL",
        &[&run],
    ).await?.get::<_, bool>(0));
    assert_eq!(enqueue(&mut admin, &schema, &request).await?, run);
    request.input = json!({"different":true});
    assert!(enqueue(&mut admin, &schema, &request).await.is_err());
    request.input = json!({"queued":true});
    let (_stop_first, stopped_first) = tokio::sync::watch::channel(true);
    first.serve(stopped_first).await?;
    first.revoke();
    drop(first);

    let second = QueueService::bind(
        Arc::clone(&driver),
        Arc::clone(&postgres),
        Arc::clone(&jetstream),
        &release,
        QueueServiceConfig {
            project: "default".to_owned(),
            runner: "automation-live-second".to_owned(),
            lease_ttl_ms: 30_000,
        },
    )
    .await?;
    let (stop_second, stopping_second) = tokio::sync::watch::channel(false);
    let serving_second = second.serve(stopping_second);
    tokio::pin!(serving_second);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut serving_second => {
                    panic!("restarted queue service stopped before completing durable work: {result:?}")
                }
                () = tokio::time::sleep(Duration::from_millis(10)) => {
                    let status: String = admin.query_one(
                        "SELECT status FROM wamn_run.runs WHERE run_id=$1",
                        &[&run],
                    ).await?.get(0);
                    if status == "completed" {
                        return Ok::<_, anyhow::Error>(());
                    }
                }
            }
        }
    })
    .await
    .expect("restarted queue service completes pending run")?;
    stop_second.send(true)?;
    tokio::time::timeout(Duration::from_secs(1), &mut serving_second)
        .await
        .expect("restarted queue service drains")?;
    let row = admin.query_one("SELECT status,result_json::text,service_principal_id::text,deadline_adjustments_json::text FROM wamn_run.runs WHERE run_id=$1",&[&run]).await?;
    assert_eq!(row.get::<_, String>(0), "completed");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(1))?,
        request.input
    );
    assert_eq!(row.get::<_, String>(2), SERVICE);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(3))?,
        json!([{"node":"echo","requested-ms":60000,"effective-ms":30000}])
    );
    assert!(!drain_one(&driver, &postgres, &jetstream, &scope, 30000, &liveness).await?);
    request.idempotency_key = "trap".to_owned();
    request.input = serde_json::Value::Null;
    let trapped_run = enqueue(&mut admin, &schema, &request).await?;
    let error = drain_one(&driver, &postgres, &jetstream, &scope, 30000, &liveness)
        .await
        .unwrap_err();
    let adjustments = &error
        .downcast_ref::<crate::DeadlineAdjustments>()
        .expect("a guest trap retains the effective deadline")
        .0;
    assert_eq!(
        serde_json::to_value(adjustments)?,
        json!([{"node":"echo","requested-ms":60000,"effective-ms":30000}])
    );
    let row = admin
        .query_one(
            "SELECT deadline_adjustments_json::text FROM wamn_run.runs WHERE run_id=$1",
            &[&trapped_run],
        )
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(0))?,
        json!([{"node":"echo","requested-ms":60000,"effective-ms":30000}])
    );
    assert!(
        !postgres
            .record_deadline_adjustments(QUEUE_CLAIM_SCOPE, &trapped_run, 999, &json!([]))
            .await?
    );
    let row = admin
        .query_one(
            "SELECT deadline_adjustments_json::text FROM wamn_run.runs WHERE run_id=$1",
            &[&trapped_run],
        )
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(0))?,
        json!([{"node":"echo","requested-ms":60000,"effective-ms":30000}])
    );
    request.input = json!({"queued":true});
    request.idempotency_key = "revoked".to_owned();
    let denied = enqueue(&mut admin, &schema, &request).await?;
    admin
        .execute(
            "DELETE FROM app_system.permissions WHERE tenant_id=$1",
            &[&TENANT],
        )
        .await?;
    let refusal = drain_one(&driver, &postgres, &jetstream, &scope, 30000, &liveness)
        .await
        .unwrap_err();
    assert_eq!(
        refusal.to_string(),
        format!("permission denied for operation {OPERATION}")
    );
    let row = admin
        .query_one(
            "SELECT status,result_json::text FROM wamn_run.runs WHERE run_id=$1",
            &[&denied],
        )
        .await?;
    assert_ne!(row.get::<_, String>(0), "completed");
    assert!(row.get::<_, Option<String>>(1).is_none());
    admin.execute("UPDATE app_system.users SET status='disabled' WHERE tenant_id=$1 AND id=$2::text::uuid",&[&TENANT,&SERVICE]).await?;
    request.idempotency_key = "inactive".to_owned();
    assert!(enqueue(&mut admin, &schema, &request).await.is_err());
    second.revoke();
    Ok(())
}
