//! A durable queue item whose host dies during application SQL completes
//! after the host restarts (wamn-7k2fh; the cluster case that b8199fb4c
//! deleted).
//!
//! The queue services run the real Receiving component in process. The first
//! one is the host that dies: its serving task is aborted while its
//! record_receipt statement waits on a purchase order row that this test
//! holds, and its database backend is terminated, as process death closes
//! its connections. The second is the restarted host.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_integration_tests::local_application::{
    LocalApplication, LocalApplicationConfig, LocalPackage, LocalQueue,
};
use wamn_runtime::plugins::wamn_jetstream::{WamnJetstream, WamnJetstreamConfig};
use wamn_test_infrastructure::scratch::ScratchRoot;
use wamn_workflow::{
    PostgresWorkflows, QueueService, QueueServiceConfig, StartRequest, Trigger, Workflows as _,
};

const ORDER: &str = "00000000-0000-0000-0000-000000000304";
const LINE: &str = "00000000-0000-0000-0000-000000000504";
const IDEMPOTENCY_KEY: &str = "queue-recovery";
/// Short, so the dead host's lease expires soon after it dies.
const LEASE_TTL_MS: u64 = 2_000;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN"]
async fn a_run_killed_during_application_sql_completes_after_the_host_restarts()
-> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let components = std::path::PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = std::path::PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let receiving = super::repository_root()?.join("apps/wamn_receiving");
    let wiring = wamn_catalog::WiringDocument::parse(&json!({
        "format-version":"0.1","wiring-id":"receiving_record_receipt","version":1,"entry":"operation",
        "nodes":{"operation":{"component":"receiving","interface-version":"0.1.0",
            "operation":*super::BASE_RECORD_RECEIPT}}
    }))?;
    // The release publishes the record_receipt route, whose intent the queued
    // call logs in its host-owned item transaction.
    let mut attachments =
        wamn_schema_generator::route_schema::read_package_attachments(&receiving)?;
    attachments.retain(|_, attachment| {
        matches!(&attachment.target, wamn_catalog::AttachmentTarget::Route { operation, .. }
            if wamn_schema_generator::sealed_operation_reference(operation)
                == super::BASE_RECORD_RECEIPT_REFERENCE)
    });
    ensure!(
        attachments.len() == 1,
        "the base publishes one record_receipt route"
    );
    let identity = super::identity();
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system.url(),
        database_url: project.url(),
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        tenant: identity.tenant.as_str(),
        org: identity.org.as_str(),
        project: identity.project.as_str(),
        environment: identity.environment.as_str(),
        schema: "receiving",
        caller_role: super::ADMIN_ROLE,
        route_host: "receiving.local.test",
        attachments: &attachments,
        packages: &[LocalPackage {
            root: &receiving,
            component: "receiving",
            wirings: std::slice::from_ref(&wiring),
        }],
    })
    .await?;
    let queue = application.queue.clone();
    let result = tokio::task::LocalSet::new()
        .run_until(recover(&queue, project.url()))
        .await;
    application.shutdown().await?;
    result
}

async fn recover(queue: &LocalQueue, project_url: &str) -> anyhow::Result<()> {
    let identity = super::identity();
    let tenant = identity.tenant.as_str();
    let (client, task) = super::connect(project_url).await?;
    let (lock, lock_task) = super::connect(project_url).await?;
    let (workflow_client, workflow_connection) =
        tokio_postgres::connect(project_url, tokio_postgres::NoTls).await?;
    let workflow_task = tokio::spawn(workflow_connection);
    let workflows = PostgresWorkflows::new(
        workflow_client,
        "wamn_run",
        tenant,
        identity.environment.as_str(),
    );
    let result = async {
        // Provisioning writes the environment's run policy; the local
        // application does not provision.
        client
            .execute(
                "INSERT INTO wamn_run.environment_policies \
                   (tenant_id, expected_environment, durability_class) VALUES ($1, $2, 'standard')",
                &[&tenant, &identity.environment.as_str()],
            )
            .await
            .context("write the environment's run policy")?;
        super::bind_fixture_principal(&client, tenant).await?;
        client
            .batch_execute(
                "INSERT INTO receiving.item (id, item_number) VALUES \
                   ('00000000-0000-0000-0000-000000000101', 'ITEM-101'); \
                 INSERT INTO receiving.location (id, location_code) VALUES \
                   ('00000000-0000-0000-0000-000000000201', 'DOCK-1');",
            )
            .await
            .context("seed the queued receipt item and location")?;
        super::environment::seed_materializer_order(&client, ORDER, LINE, "PO-304").await?;
        // The local application's operator service holds the admin role.
        let principal: String = client
            .query_one(
                "SELECT id::text FROM platform.\"user\" WHERE tenant_id = $1 AND type = 'service' \
                   AND email = 'local@example.invalid'",
                &[&tenant],
            )
            .await
            .context("read the local operator service")?
            .get(0);
        lock.batch_execute("BEGIN").await?;
        lock.query_one(
            "SELECT id FROM receiving.purchase_order WHERE id = $1::text::uuid FOR UPDATE",
            &[&ORDER],
        )
        .await?;
        let blocker: i32 = lock.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
        let request = StartRequest {
            manifest_digest: queue.release.release().manifest_digest.as_str().to_owned(),
            package_id: super::BASE_PACKAGE_ID.to_owned(),
            wiring_id: "receiving_record_receipt".to_owned(),
            wiring_version: 1,
            trigger: Trigger::Automation {
                service_principal_id: principal,
            },
            idempotency_key: IDEMPOTENCY_KEY.to_owned(),
            input: json!([{"request_id":"queue-recovery","value":{
                "idempotency_key":IDEMPOTENCY_KEY,"purchase_order_id":ORDER,
                "receipt_reference":"QUEUE-RECOVERY","occurred_at":"2026-08-31T12:34:00.000000Z",
                "line":[{"purchase_order_line_id":LINE,"quantity":"9.0000",
                    "location_id":"00000000-0000-0000-0000-000000000201"}]}}]),
        };
        let run = workflows.start(&request).await?;

        let (stop_first, first) = serve(queue, "queue-recovery-first").await?;
        let (generation, backend) = blocked_attempt(&client, &run, blocker, 0).await?;
        // The host dies: its process stops at once and its connections close.
        first.abort();
        drop(stop_first);
        ensure!(
            first.await.is_err_and(|error| error.is_cancelled()),
            "the dead host's queue task must be cancelled"
        );
        client
            .query_one("SELECT pg_terminate_backend($1)", &[&backend])
            .await?;
        let dead = client
            .query_one(
                "SELECT r.status, q.lease_generation FROM wamn_run.runs r \
                   JOIN wamn_run.run_queue q USING (tenant_id, run_id) WHERE run_id = $1",
                &[&run],
            )
            .await?;
        ensure!(
            dead.get::<_, String>(0) == "running" && dead.get::<_, i64>(1) == generation,
            "the dead host's run stays running under its lease"
        );
        ensure!(
            receipts(&client).await? == 0,
            "the dead host committed no receipt"
        );

        let (stop_second, second) = serve(queue, "queue-recovery-restarted").await?;
        let (recovered, recovered_backend) =
            blocked_attempt(&client, &run, blocker, generation).await?;
        ensure!(
            recovered_backend != backend,
            "recovery runs on a new database connection"
        );
        lock.batch_execute("ROLLBACK").await?;
        let outcome = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let row = client
                    .query_one(
                        "SELECT status, result_json FROM wamn_run.runs WHERE run_id = $1",
                        &[&run],
                    )
                    .await?;
                let status: String = row.get(0);
                if status != "running" && status != "dispatched" {
                    ensure!(status == "completed", "the recovered run ended {status}");
                    return Ok::<Value, anyhow::Error>(row.get(1));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("the recovered run did not complete within 30 seconds")??;
        stop_second.send(true)?;
        tokio::time::timeout(Duration::from_secs(10), second)
            .await
            .context("the restarted host's queue drains")???;
        ensure!(
            outcome[0]["request_id"] == "queue-recovery"
                && outcome[0]["value"]["receipt_id"].is_string(),
            "the recovered command returns its committed receipt: {outcome}"
        );
        ensure!(
            receipts(&client).await? == 1,
            "the interrupted command commits exactly one receipt"
        );
        ensure!(
            client
                .query_opt(
                    "SELECT run_id FROM wamn_run.run_queue WHERE run_id = $1",
                    &[&run]
                )
                .await?
                .is_none(),
            "the completed run leaves the durable queue"
        );
        ensure!(
            workflows.start(&request).await? == run,
            "admission replay returns the completed run"
        );
        println!(
            "QUEUE_RECOVERY result=pass run={run} first_generation={generation} recovered_generation={recovered}"
        );
        Ok(())
    }
    .await;
    drop(workflows);
    workflow_task.abort();
    drop(lock);
    lock_task.abort();
    drop(client);
    task.abort();
    result
}

/// Bind a queue service of the local release as the runner `runner`, and
/// serve it on its own task until the returned sender says stop.
async fn serve(
    queue: &LocalQueue,
    runner: &str,
) -> anyhow::Result<(
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let jetstream = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig {
            nats_url: None,
            ..WamnJetstreamConfig::default()
        })
        .with_release(Some(Arc::clone(&queue.release))),
    );
    let service = QueueService::bind(
        Arc::clone(&queue.driver),
        Arc::clone(&queue.postgres),
        jetstream,
        &queue.release,
        QueueServiceConfig {
            project: super::identity().project.as_str().to_owned(),
            runner: runner.to_owned(),
            lease_ttl_ms: LEASE_TTL_MS,
        },
    )
    .await?;
    let (stop, stopping) = tokio::sync::watch::channel(false);
    // The queue future is not Send, so it runs on the local set of the test.
    let task = tokio::task::spawn_local(async move { service.serve(stopping).await });
    Ok((stop, task))
}

/// The receipts the queued command committed.
async fn receipts(client: &tokio_postgres::Client) -> anyhow::Result<i64> {
    Ok(client
        .query_one(
            "SELECT count(*) FROM receiving.receipt WHERE purchase_order_id = $1::text::uuid",
            &[&ORDER],
        )
        .await?
        .get(0))
}

/// Wait until an attempt of `run` with a lease generation above `after` runs
/// application SQL that `blocker` blocks, and return its generation and
/// backend.
async fn blocked_attempt(
    client: &tokio_postgres::Client,
    run: &str,
    blocker: i32,
    after: i64,
) -> anyhow::Result<(i64, i32)> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(row) = client
                .query_opt(
                    "SELECT q.lease_generation, a.pid FROM wamn_run.run_queue q \
                       JOIN wamn_run.runs r USING (tenant_id, run_id) \
                       CROSS JOIN pg_stat_activity a \
                     WHERE q.run_id = $1 AND r.status = 'running' AND q.lease_generation > $2 \
                       AND $3 = ANY(pg_blocking_pids(a.pid)) AND a.query LIKE '%purchase_order%'",
                    &[&run, &after, &blocker],
                )
                .await?
            {
                return Ok::<_, anyhow::Error>((row.get(0), row.get(1)));
            }
            let status: String = client
                .query_one(
                    "SELECT status FROM wamn_run.runs WHERE run_id = $1",
                    &[&run],
                )
                .await?
                .get(0);
            ensure!(
                status == "running" || status == "dispatched",
                "the queued run ended before it blocked: {status}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("the queued run did not reach blocked application SQL within 60 seconds")?
}
