//! An interrupted product command completes once after execution recovery.
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use sqlx::Row as _;
use std::time::Duration;
use wamn_integration_tests::product_application::Application;
const ORDER: &str = "00000000-0000-0000-0000-000000000304";
const LINE: &str = "00000000-0000-0000-0000-000000000504";
const IDEMPOTENCY_KEY: &str = "queue-recovery";

#[tokio::test(flavor = "multi_thread")]
async fn a_run_killed_during_application_sql_completes_after_the_host_restarts()
-> anyhow::Result<()> {
    let application: Application =
        panic!("wamn-eulfr.12.10: no product controlled execution crash");
    let mut client = application.observer().await?;
    let input = json!([{"request_id":"queue-recovery","value":{
        "idempotency_key":IDEMPOTENCY_KEY,"purchase_order_id":ORDER,
        "receipt_reference":"QUEUE-RECOVERY","occurred_at":"2026-08-31T12:34:00.000000Z",
        "line":[{"purchase_order_line_id":LINE,"quantity":"9.0000",
            "location_id":"00000000-0000-0000-0000-000000000201"}]}}]);
    let start = async || -> anyhow::Result<String> {
        let answer = application.call("workflow.start", json!({"application":"receiving",
            "workflow_id":"receiving_record_receipt", "input_json":input,"idempotency_key":IDEMPOTENCY_KEY})).await?;
        answer["run_id"]
            .as_str()
            .map(str::to_owned)
            .context("workflow run id")
    };
    let run = start().await?;
    let (generation, backend, blocker): (i64, i32, i32) =
        panic!("wamn-eulfr.12.10: no product blocked execution fixture");
    panic!("wamn-eulfr.12.10: no product execution crash");
    let dead = sqlx::query(
        "SELECT r.status, q.lease_generation FROM wamn_run.runs r \
                   JOIN wamn_run.run_queue q USING (project_env_key, run_id) WHERE run_id = $1",
    )
    .bind(&run)
    .fetch_one(&mut client)
    .await?;
    ensure!(
        dead.get::<String, _>(0) == "running" && dead.get::<i64, _>(1) == generation,
        "the dead host's run stays running under its lease"
    );
    ensure!(
        receipts(&mut client).await? == 0,
        "the dead host committed no receipt"
    );

    panic!("wamn-eulfr.12.10: no product execution restart");
    let (recovered, recovered_backend) =
        blocked_attempt(&mut client, &run, blocker, generation).await?;
    ensure!(
        recovered_backend != backend,
        "recovery runs on a new database connection"
    );
    panic!("wamn-eulfr.12.10: no product transaction release");
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let row =
                sqlx::query("SELECT status, result_json FROM wamn_run.runs WHERE run_id = $1")
                    .bind(&run)
                    .fetch_one(&mut client)
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
    ensure!(
        outcome[0]["request_id"] == "queue-recovery"
            && outcome[0]["value"]["receipt_id"].is_string(),
        "the recovered command returns its committed receipt: {outcome}"
    );
    ensure!(
        receipts(&mut client).await? == 1,
        "the interrupted command commits exactly one receipt"
    );
    ensure!(
        sqlx::query("SELECT run_id FROM wamn_run.run_queue WHERE run_id = $1")
            .bind(&run)
            .fetch_optional(&mut client)
            .await?
            .is_none(),
        "the completed run leaves the durable queue"
    );
    ensure!(
        start().await? == run,
        "admission replay returns the completed run"
    );
    application.remove().await?;
    Ok(())
}

/// The receipts the queued command committed.
async fn receipts(client: &mut sqlx::PgConnection) -> anyhow::Result<i64> {
    Ok(sqlx::query(
        "SELECT count(*) FROM receiving.receipt WHERE purchase_order_id = $1::text::uuid",
    )
    .bind(&ORDER)
    .fetch_one(&mut *client)
    .await?
    .get(0))
}

/// Wait until an attempt of `run` with a lease generation above `after` runs
/// application SQL that `blocker` blocks, and return its generation and
/// backend.
async fn blocked_attempt(
    client: &mut sqlx::PgConnection,
    run: &str,
    blocker: i32,
    after: i64,
) -> anyhow::Result<(i64, i32)> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(row) = sqlx::query(
                "SELECT q.lease_generation, a.pid FROM wamn_run.run_queue q \
                       JOIN wamn_run.runs r USING (project_env_key, run_id) \
                       CROSS JOIN pg_stat_activity a \
                     WHERE q.run_id = $1 AND r.status = 'running' AND q.lease_generation > $2 \
                       AND $3 = ANY(pg_blocking_pids(a.pid)) AND a.query LIKE '%purchase_order%'",
            )
            .bind(&run)
            .bind(&after)
            .bind(&blocker)
            .fetch_optional(&mut *client)
            .await?
            {
                return Ok::<_, anyhow::Error>((row.get(0), row.get(1)));
            }
            let status: String = sqlx::query("SELECT status FROM wamn_run.runs WHERE run_id = $1")
                .bind(&run)
                .fetch_one(&mut *client)
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
