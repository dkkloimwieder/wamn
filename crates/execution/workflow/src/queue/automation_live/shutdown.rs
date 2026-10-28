//! Queue-adapter shutdown while production-admitted work executes a native guest.

use std::time::Duration;

use tokio::time::timeout;
use wamn_run_state::RunStore as _;
use wamn_runtime::plugins::wamn_logging::{Capture, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::WamnPostgres;
use wash_runtime::engine::Engine;
use wash_runtime::host::probes::Liveness;

use super::{NODE_TYPES, OPERATION, QUEUE_CLAIM_SCOPE, QueueScope, RouterDriver, WamnJetstream};

pub(super) fn component_bytes() -> Vec<u8> {
    wat::parse_str(format!(r#"(component
      {NODE_TYPES}
      (import "wasi:logging/logging@0.1.0-draft" (instance $logging
        (type $level' (enum "trace" "debug" "info" "warn" "error" "critical"))
        (export "level" (type $level (eq $level')))
        (export "log" (func (param "level" $level) (param "context" string) (param "message" string)))))
      (core module $memory
        (memory (export "memory") 16)
        (data (i32.const 32) "guest-entered")
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
          (local $old i32) global.get $next local.tee $old
          local.get $size i32.add global.set $next local.get $old))
      (core instance $memory (instantiate $memory))
      (core func $log (canon lower (func $logging "log") (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core module $main
        (import "memory" "memory" (memory 16))
        (import "host" "log" (func $log (param i32 i32 i32 i32 i32)))
        (func (export "run") (param i32) (result i32)
          i32.const 2 i32.const 0 i32.const 0 i32.const 32 i32.const 13 call $log
          (loop br 0)
          unreachable))
      (core instance $main (instantiate $main (with "memory" (instance $memory)) (with "host" (instance (export "log" (func $log))))))
      (func $run (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory") (realloc (func $memory "realloc"))))
      (instance $handler
        (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission))
        (export "run" (func $run)))
      (export "{OPERATION}" (instance $handler)))"#)).expect("logging guest that stays active until shutdown")
}

pub(super) struct Execution<'a> {
    pub(super) driver: &'a RouterDriver,
    pub(super) postgres: &'a WamnPostgres,
    pub(super) engine: &'a Engine,
    pub(super) logging: &'a WamnLogging,
    pub(super) capture: &'a Capture,
    pub(super) scope: &'a QueueScope,
    pub(super) jetstream: &'a WamnJetstream,
}

pub(super) async fn run(
    admin: &mut tokio_postgres::Client,
    workflows: &super::WorkflowsFixture,
    request: &super::StartInput,
    execution: Execution<'_>,
) -> anyhow::Result<()> {
    let run = workflows.start(request).await?;
    let liveness = Liveness::new(Duration::from_secs(90));
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let queue = super::super::serve_queue(
        stopping,
        &liveness,
        async || {
            Box::pin(super::drain_one(
                execution.driver,
                execution.postgres,
                execution.jetstream,
                execution.scope,
                1000,
                &liveness,
            ))
            .await
        },
        async || {
            Ok(execution
                .postgres
                .pinned_backlog(
                    QUEUE_CLAIM_SCOPE,
                    &execution.scope.package_ids,
                    &execution.scope.environment,
                )
                .await?)
        },
    );
    let mut queue = Box::pin(queue);
    timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut queue => panic!("queue stopped before guest entry: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {
                    if execution.capture.snapshot().iter().any(|record| record.message == "guest-entered") { break; }
                }
            }
        }
    }).await.expect("actual guest entry before signal");
    let scopes = execution.postgres.invocation_scopes_for_test();
    assert_eq!(scopes.len(), 1, "one real guest invocation holds authority");
    let scope = &scopes[0];
    assert_eq!(
        execution
            .postgres
            .session_claims(scope)
            .unwrap()
            .user_id
            .as_deref(),
        Some(super::SERVICE)
    );
    assert!(execution.logging.claim_snapshot(scope).is_some());
    let before = admin
        .query_one(
            "SELECT r.status,q.lease_generation FROM wamn_run.runs r JOIN wamn_run.run_queue q USING (env_key,run_id) WHERE run_id=$1",
            &[&run],
        )
        .await?;
    assert_eq!(before.get::<_, String>(0), "running");
    let generation: i64 = before.get(1);
    let destroyed_connections = execution.postgres.destroyed_connections();
    // Hold the queue row through shutdown so lease renewal must wait on this
    // transaction. Cancellation must destroy its connection, not repool it.
    admin.batch_execute("BEGIN").await?;
    admin
        .query_one(
            "SELECT run_id FROM wamn_run.run_queue WHERE run_id=$1 FOR UPDATE",
            &[&run],
        )
        .await?;
    stop.send(true)?;
    let error = wamn_engine::lifecycle::bounded_cleanup(
        wash_runtime::washlet::COMMAND_DRAIN_TIMEOUT,
        &mut queue,
    )
    .await
    .expect_err("active spinning guest exceeds drain budget");
    assert!(
        format!("{error:#}").contains("shutdown budget"),
        "{error:#}"
    );
    drop(queue);
    assert!(
        execution.postgres.destroyed_connections() > destroyed_connections,
        "shutdown must discard the interrupted lease renewal connection"
    );
    admin.batch_execute("ROLLBACK").await?;
    assert!(execution.postgres.invocation(scope).is_none());
    assert!(execution.postgres.session_claims(scope).is_none());
    assert!(execution.logging.claim_snapshot(scope).is_none());
    let after = admin
        .query_one(
            "SELECT r.status,q.lease_generation,r.result_json::text FROM wamn_run.runs r JOIN wamn_run.run_queue q USING (env_key,run_id) WHERE run_id=$1",
            &[&run],
        )
        .await?;
    assert_eq!(
        after.get::<_, String>(0),
        "running",
        "aborted delivery remains recoverable, never completed"
    );
    assert_eq!(
        after.get::<_, i64>(1),
        generation,
        "shutdown never claims again"
    );
    assert!(after.get::<_, Option<String>>(2).is_none());
    timeout(Duration::from_secs(3), async {
        loop {
            let expired: bool = admin.query_one("SELECT lease_expires_at <= clock_timestamp() FROM wamn_run.run_queue WHERE run_id=$1", &[&run]).await?.get(0);
            if expired { return Ok::<_, anyhow::Error>(()); }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("aborted lease expires without further renewal")?;
    let reclaimed = execution
        .postgres
        .claim_next(
            QUEUE_CLAIM_SCOPE,
            &execution.scope.package_ids,
            &execution.scope.environment,
            1000,
        )
        .await?;
    match reclaimed {
        super::super::ProductionClaimResult::Ready {
            run_id,
            lease_generation,
            ..
        } => {
            assert_eq!(run_id, run);
            assert!(lease_generation > generation, "recovery owns a new fence");
        }
        super::super::ProductionClaimResult::Empty => {
            panic!("aborted run must be reclaimable: Empty")
        }
    }
    let stale = execution
        .postgres
        .complete(
            QUEUE_CLAIM_SCOPE,
            &run,
            generation,
            &wamn_runtime::plugins::wamn_postgres::ProductionCompletion::completed(
                serde_json::json!({"stale":true}),
                None,
            ),
        )
        .await?;
    assert_eq!(stale, super::super::ProductionCompletionResult::FenceLost);
    execution.postgres.revoke_session_claims(QUEUE_CLAIM_SCOPE);
    assert!(
        execution
            .postgres
            .session_claims(QUEUE_CLAIM_SCOPE)
            .is_none()
    );
    timeout(Duration::from_secs(25), async {
        while execution.engine.guest_memory().in_use() != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native abandoned guest releases its store within its grace");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn active_drain_preserves_recovery_and_revokes_authority() -> anyhow::Result<()> {
    super::run_automation(super::Mode::Shutdown).await
}

pub(super) async fn missing_workflow(
    admin: &tokio_postgres::Client,
    workflows: &super::WorkflowsFixture,
    request: &super::StartInput,
    queue: &super::QueueService,
    manifest_digest: &str,
) -> anyhow::Result<()> {
    let missing = "missing-workflow";
    admin
        .execute(
            "INSERT INTO wamn_run.runs \
             (env_key,run_id,flow_id,flow_version,status,package_id,manifest_digest, \
              environment,wiring_id,wiring_version,input_json,trigger_source,durability_class) \
             VALUES ($1,$2,'missing-workflow',1,'dispatched','automation',$3, \
                     'test','echo-manual',1,'{}','http','standard')",
            &[&super::ENV_KEY, &missing, &manifest_digest],
        )
        .await?;
    // Put the claim without a workflow first in the durable queue.
    admin
        .execute(
            "INSERT INTO wamn_run.run_queue (env_key,run_id,available_at,stream_seq) \
             VALUES ($1,$2,'2000-01-01',0)",
            &[&super::ENV_KEY, &missing],
        )
        .await?;
    let next = workflows.start(request).await?;
    let (_stop, stopping) = tokio::sync::watch::channel(true);
    timeout(Duration::from_secs(5), queue.serve(stopping))
        .await
        .expect("the same drain fails the missing workflow and delivers the next run")?;
    let row = admin
        .query_one(
            "SELECT status,fail_type,result_json::text FROM wamn_run.runs WHERE run_id=$1",
            &[&missing],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, String>(1), "unresolvable-name");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(2))?,
        serde_json::json!({"error": {
            "code": "workflow_resolution_refused",
            "run-id": missing,
        }})
    );
    let row = admin
        .query_one(
            "SELECT status,result_json::text FROM wamn_run.runs WHERE run_id=$1",
            &[&next],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), "completed");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(1))?,
        request.input
    );
    assert!(
        !admin
            .query_one(
                "SELECT EXISTS (SELECT FROM wamn_run.run_queue WHERE run_id IN ($1,$2))",
                &[&missing, &next],
            )
            .await?
            .get::<_, bool>(0)
    );
    queue.revoke();
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn drain_fails_a_claim_without_workflow_and_continues() -> anyhow::Result<()> {
    super::run_automation(super::Mode::MissingWorkflow).await
}
