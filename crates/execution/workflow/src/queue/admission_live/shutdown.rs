//! Queue-adapter shutdown while production-admitted work executes a native guest.

use sqlx::Row as _;
use std::time::Duration;

use tokio::time::timeout;
use wamn_run_state::RunStore as _;
use wamn_runtime::plugins::wamn_logging::{Capture, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::WamnPostgres;
use wash_runtime::engine::Engine;
use wash_runtime::host::probes::Liveness;

use super::{
    NODE_TYPES, OPERATION, QUEUE_LEASE_INVOCATION, QueueTarget, RouterDriver, WamnJetstream,
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
    pub(super) target: &'a QueueTarget,
    pub(super) jetstream: &'a WamnJetstream,
}

pub(super) async fn run(
    admin: &mut sqlx::PgConnection,
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
                execution.target,
                1000,
                &liveness,
            ))
            .await
        },
        async || {
            Ok(execution
                .postgres
                .pinned_backlog(
                    QUEUE_LEASE_INVOCATION,
                    &execution.target.package_ids,
                    &execution.target.environment,
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
    let targets: Vec<_> = execution
        .postgres
        .scopes
        .scopes()
        .into_iter()
        .filter(|scope| execution.postgres.invocation(scope).is_some())
        .collect();
    assert_eq!(
        targets.len(),
        1,
        "one real guest invocation holds authority"
    );
    let target = &targets[0];
    assert_eq!(
        execution
            .postgres
            .session_identity(target)
            .unwrap()
            .user_id
            .as_deref(),
        Some(super::SERVICE)
    );
    assert!(execution.logging.identity_snapshot(target).is_some());
    let before = sqlx::query("SELECT r.status,q.lease_generation FROM wamn_run.runs r JOIN wamn_run.run_queue q USING (project_env_key,run_id) WHERE run_id=$1").bind(&run).fetch_one(&mut *admin)
        .await?;
    assert_eq!(before.get::<String, _>(0), "running");
    let generation: i64 = before.get(1);
    let destroyed_connections = execution.postgres.destroyed_connections();
    // Hold the queue row through shutdown so lease renewal must wait on this
    // transaction. Cancellation must destroy its connection, not repool it.
    sqlx::raw_sql("BEGIN")
        .execute(&mut *admin)
        .await
        .map(drop)?;
    sqlx::query("SELECT run_id FROM wamn_run.run_queue WHERE run_id=$1 FOR UPDATE")
        .bind(&run)
        .fetch_one(&mut *admin)
        .await?;
    stop.send(true)?;
    wamn_engine::lifecycle::bounded_cleanup(
        wash_runtime::washlet::COMMAND_DRAIN_TIMEOUT,
        &mut queue,
    )
    .await
    .expect_err("active spinning guest exceeds drain budget");
    drop(queue);
    assert!(
        execution.postgres.destroyed_connections() > destroyed_connections,
        "shutdown must discard the interrupted lease renewal connection"
    );
    sqlx::raw_sql("ROLLBACK")
        .execute(&mut *admin)
        .await
        .map(drop)?;
    assert!(execution.postgres.invocation(target).is_none());
    assert!(execution.postgres.session_identity(target).is_none());
    assert!(execution.logging.identity_snapshot(target).is_none());
    let after = sqlx::query("SELECT r.status,q.lease_generation,r.result_json::text FROM wamn_run.runs r JOIN wamn_run.run_queue q USING (project_env_key,run_id) WHERE run_id=$1").bind(&run).fetch_one(&mut *admin)
        .await?;
    assert_eq!(
        after.get::<String, _>(0),
        "running",
        "aborted delivery remains recoverable, never completed"
    );
    assert_eq!(
        after.get::<i64, _>(1),
        generation,
        "shutdown never leases again"
    );
    assert!(after.get::<Option<String>, _>(2).is_none());
    timeout(Duration::from_secs(3), async {
        loop {
            let expired: bool = sqlx::query("SELECT lease_expires_at <= clock_timestamp() FROM wamn_run.run_queue WHERE run_id=$1").bind(&run).fetch_one(&mut *admin).await?.get(0);
            if expired { return Ok::<_, anyhow::Error>(()); }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("aborted lease expires without further renewal")?;
    let recovered = execution
        .postgres
        .lease_next(
            QUEUE_LEASE_INVOCATION,
            &execution.target.package_ids,
            &execution.target.environment,
            1000,
        )
        .await?;
    match recovered {
        super::super::ProductionLeaseResult::Ready {
            run_id,
            lease_generation,
            ..
        } => {
            assert_eq!(run_id, run);
            assert!(lease_generation > generation, "recovery owns a new fence");
        }
        super::super::ProductionLeaseResult::Empty => {
            panic!("aborted run must be recoverable: Empty")
        }
    }
    let stale = execution
        .postgres
        .complete(
            QUEUE_LEASE_INVOCATION,
            &run,
            generation,
            &wamn_runtime::plugins::wamn_postgres::ProductionCompletion::completed(
                serde_json::json!({"stale":true}),
                None,
            ),
        )
        .await?;
    assert_eq!(stale, super::super::ProductionCompletionResult::FenceLost);
    execution
        .postgres
        .revoke_session_identity(QUEUE_LEASE_INVOCATION);
    assert!(
        execution
            .postgres
            .session_identity(QUEUE_LEASE_INVOCATION)
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
    super::run_current(super::Mode::Shutdown).await
}
