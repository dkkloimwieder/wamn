//! An interrupted application command completes once after queue recovery.
//! The CLI authors and enables the workflow. Its control route starts and replays it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_integration_tests::local_application::{
    LocalApplication, LocalApplicationConfig, LocalPackage, LocalQueue,
};
use wamn_runtime::plugins::wamn_jetstream::{WamnJetstream, WamnJetstreamConfig};
use wamn_test_infrastructure::scratch::ScratchRoot;
use wamn_workflow::{QueueService, QueueServiceConfig};

const RECORD_RECEIPT: &str = "wamn-receiving:receiving/record-receipt";
const ORDER: &str = "00000000-0000-0000-0000-000000000304";
const LINE: &str = "00000000-0000-0000-0000-000000000504";
const IDEMPOTENCY_KEY: &str = "queue-recovery";
/// Short, so the dead host's lease expires soon after it dies.
const LEASE_TTL_MS: u64 = 2_000;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_RECEIVING_DEV_BIN"]
async fn a_run_killed_during_application_sql_completes_after_the_host_restarts()
-> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_RECEIVING_DEV_BIN",
    );
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
            "operation":wamn_test_infrastructure::operations::sealed(RECORD_RECEIPT)}}
    }))?;
    // The release publishes the record_receipt route, whose intent the queued
    // call logs in its host-owned request element transaction.
    let mut attachments =
        wamn_schema_generator::route_schema::read_package_attachments(&receiving)?;
    attachments.retain(|_, attachment| {
        matches!(&attachment.target, wamn_catalog::AttachmentTarget::Route { operation, .. }
            if wamn_catalog::sealed_operation_reference(operation)
                == RECORD_RECEIPT)
    });
    ensure!(
        attachments.len() == 1,
        "the base publishes one record_receipt route"
    );
    let identity = super::identity();
    std::fs::write(
        scratch.path().join("environment.k"),
        include_str!("../../../../tests/integration/fixtures/local-environments/receiving.k"),
    )?;
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system.url(),
        database_url: project.url(),
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        env_key: identity.env_key().as_str(),
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
        }],
    })
    .await?;
    let result = Box::pin(tokio::task::LocalSet::new().run_until(recover(
        &application,
        &application.queue,
        project.url(),
        system.url(),
        &wiring,
        scratch.path(),
    )))
    .await;
    application.shutdown().await?;
    result
}

async fn recover(
    application: &LocalApplication,
    queue: &LocalQueue,
    project_url: &str,
    system_url: &str,
    wiring: &wamn_catalog::WiringDocument,
    root: &std::path::Path,
) -> anyhow::Result<()> {
    let identity = super::identity();
    let env_key = identity.env_key();
    let (client, task) = super::connect(project_url).await?;
    let (lock, lock_task) = super::connect(project_url).await?;
    let result = async {
        // Provisioning writes the environment's run policy; the local
        // application does not provision.
        client
            .execute(
                "SELECT set_config('app.user_id', $1, false)",
                &[&wamn_control_provision::PlatformComponent::Provisioning
                    .principal_id()
                    .to_string()],
            )
            .await?;
        client
            .execute(
                "INSERT INTO wamn_run.environment_policies \
                   (env_key, expected_environment) VALUES ($1, $2)",
                &[&env_key, &identity.environment.as_str()],
            )
            .await
            .context("write the environment's run policy")?;
        super::bind_fixture_principal(&client).await?;
        client
            .batch_execute(
                "INSERT INTO receiving.item (id, item_number) VALUES \
                   ('00000000-0000-0000-0000-000000000101', 'ITEM-101'); \
                 INSERT INTO receiving.location (id, location_code) VALUES \
                   ('00000000-0000-0000-0000-000000000201', 'DOCK-1');",
            )
            .await
            .context("seed the queued receipt request element and location")?;
        client.batch_execute(
            "INSERT INTO receiving.supplier (id, name) VALUES \
             ('00000000-0000-0000-0000-000000000404', 'SUPPLIER-404'); \
             INSERT INTO receiving.purchase_order (id, purchase_order_number, supplier_id, status, row_version) \
             VALUES ('00000000-0000-0000-0000-000000000304', 'PO-304', \
                     '00000000-0000-0000-0000-000000000404', 'open', 1); \
             INSERT INTO receiving.purchase_order_line \
                 (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES ('00000000-0000-0000-0000-000000000504', \
                     '00000000-0000-0000-0000-000000000304', 1, \
                     '00000000-0000-0000-0000-000000000101', 9.0000, 0.0000);",
        ).await?;
        // The local application's operator service holds the admin role.
        let principal: String = client
            .query_one(
                "SELECT id::text FROM platform.\"user\" WHERE type = 'service' \
                   AND email = 'local@example.invalid'",
                &[],
            )
            .await
            .context("read the local operator service")?
            .get(0);
        let local = root.join("local-artifacts");
        std::fs::create_dir_all(&local)?;
        std::fs::write(local.join(wamn_runtime::local_application::LOCAL_RELEASE_FILE),
            serde_json::to_vec(&json!({
                "manifest-digest":queue.release.release().manifest_digest.as_str(),
                "applications":&queue.release.scope().applications,
            }))?)?;
        let document = root.join("workflow.json");
        std::fs::write(&document, serde_json::to_vec(wiring)?)?;
        let binary = std::env::var_os("WAMN_RECEIVING_DEV_BIN").context("workflow CLI binary")?;
        let coordinate = format!("{}/{}/{}", identity.org, identity.project, identity.environment);
        for verb in ["author", "enable"] {
            let mut command = tokio::process::Command::new(&binary);
            command.env_clear().env("WAMN_CONTROL_DATABASE_URL", system_url)
                .args(["--log-level", "error", "workflow", verb, "--admin-database-url", project_url,
                    "--environment", &identity.environment, "--coordinate", &coordinate,
                    "--application", "receiving", "--principal-id", &principal, "--dev-root"])
                .arg(root).kill_on_drop(true);
            if verb == "author" {
                command.arg("--document").arg(&document).args(["--execution-type", "invoker"]);
            } else {
                command.args(["--workflow-id", &wiring.wiring_id, "--version", &wiring.version.to_string()]);
            }
            let output = command.output().await?;
            ensure!(output.status.success(), "workflow {verb} failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        lock.batch_execute("BEGIN").await?;
        lock.query_one(
            "SELECT id FROM receiving.purchase_order WHERE id = $1::text::uuid FOR UPDATE",
            &[&ORDER],
        )
        .await?;
        let blocker: i32 = lock.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
        let input = json!([{"request_id":"queue-recovery","value":{
                "idempotency_key":IDEMPOTENCY_KEY,"purchase_order_id":ORDER,
                "receipt_reference":"QUEUE-RECOVERY","occurred_at":"2026-08-31T12:34:00.000000Z",
                "line":[{"purchase_order_line_id":LINE,"quantity":"9.0000",
                    "location_id":"00000000-0000-0000-0000-000000000201"}]}}]);

        let start = async || -> anyhow::Result<String> {
            let response: Value = reqwest::Client::new()
                .post(format!("{}/wamn_control/workflow/start", application.endpoint))
                .header("Host", &application.route_host).bearer_auth(&application.bearer)
                .json(&json!([{"request_id": IDEMPOTENCY_KEY, "value": {
                    "application":"receiving", "workflow_id":wiring.wiring_id,
                    "input_json":input, "idempotency_key":IDEMPOTENCY_KEY,
                }}])).send().await?.error_for_status()?.json().await?;
            response[0]["value"]["run_id"].as_str().map(str::to_owned)
                .with_context(|| format!("workflow.start refused: {response}"))
        };
        let run = start().await?;

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
                   JOIN wamn_run.run_queue q USING (env_key, run_id) WHERE run_id = $1",
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
            start().await? == run,
            "admission replay returns the completed run"
        );
        println!(
            "QUEUE_RECOVERY result=pass run={run} first_generation={generation} recovered_generation={recovered}"
        );
        Ok(())
    }
    .await;
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
    let jetstream = Arc::new(WamnJetstream::new(WamnJetstreamConfig::default()));
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
                       JOIN wamn_run.runs r USING (env_key, run_id) \
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
