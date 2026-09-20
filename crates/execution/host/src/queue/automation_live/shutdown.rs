//! Queue-adapter shutdown while production-admitted work executes a native guest.

use std::time::Duration;

use tokio::time::timeout;
use wamn_runtime::plugins::wamn_logging::{Capture, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::WamnPostgres;
use wash_runtime::engine::Engine;
use wash_runtime::host::probes::Liveness;

use super::{
    BareSchemaName, EnqueueRun, NODE_TYPES, OPERATION, QUEUE_CLAIM_SCOPE, QueueScope, RouterDriver,
    WamnJetstream, enqueue,
};

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
    schema: &BareSchemaName,
    request: &EnqueueRun,
    execution: Execution<'_>,
) -> anyhow::Result<()> {
    let run = enqueue(admin, schema, request).await?;
    let liveness = Liveness::new(Duration::from_secs(90));
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let queue = super::super::serve_queue(stopping, &liveness, async || {
        Box::pin(super::drain_one(
            execution.driver,
            execution.postgres,
            execution.jetstream,
            execution.scope,
            1000,
            &liveness,
        ))
        .await
    });
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
            "SELECT r.status,q.lease_generation FROM wamn_run.runs r JOIN wamn_run.run_queue q USING (tenant_id,run_id) WHERE run_id=$1",
            &[&run],
        )
        .await?;
    assert_eq!(before.get::<_, String>(0), "running");
    let generation: i64 = before.get(1);
    stop.send(true)?;
    let error = wamn_runtime::lifecycle::bounded_cleanup(
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
    assert!(execution.postgres.invocation(scope).is_none());
    assert!(execution.postgres.session_claims(scope).is_none());
    assert!(execution.logging.claim_snapshot(scope).is_none());
    let after = admin
        .query_one(
            "SELECT r.status,q.lease_generation,r.result_json::text FROM wamn_run.runs r JOIN wamn_run.run_queue q USING (tenant_id,run_id) WHERE run_id=$1",
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
        .claim_next_production(
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
        other => panic!("aborted run must be reclaimable: {other:?}"),
    }
    let stale = execution
        .postgres
        .complete_production(
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
    super::run_automation(Some("adapter-stop")).await
}
