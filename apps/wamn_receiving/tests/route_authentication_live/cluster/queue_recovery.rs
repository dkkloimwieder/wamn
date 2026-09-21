//! Durable queue recovery after a combined host dies during application SQL.

use std::fs;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_control::enqueue_run::{EnqueueRun, enqueue};
use wamn_schema_control::BareSchemaName;
use wamn_test_infrastructure::workload;

use super::{
    ReceivingCluster, checked, deployment, install_host, kubectl, provision, route_cases, start,
};

#[tokio::test]
#[ignore = "requires: docker, kind, kubectl, helm, jq, curl"]
async fn interrupted_durable_queue_item_completes_after_host_restart() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "kubectl", "helm", "jq", "curl"]);
    let evidence = super::evidence_directory()?;
    Box::pin(super::with_signals(&evidence, async {
        let mut cluster = start(&evidence, false).await?;
        let result = exercise(&cluster).await;
        route_cases::finish(&mut cluster, result).await
    }))
    .await
}

async fn exercise(cluster: &ReceivingCluster) -> anyhow::Result<()> {
    let (route, carrier) = provision(&cluster.inputs, &cluster.artifacts).await?;
    let secrets = deployment::native_secrets(cluster)?;
    let (issuer, instance) = super::session_cluster::prepare_application(cluster, &carrier).await?;
    install_host(
        &cluster.resources,
        &cluster.inputs,
        &carrier,
        &super::HostBinding {
            replicas: 1,
            nats_url: &cluster.nats_url,
            native_nats_secrets: &secrets,
            source: &cluster.source,
            session: Some((&issuer, &instance)),
        },
    )
    .await?;
    let resources = &cluster.resources;
    let digest = workload::image_ready(
        &resources.lifecycle,
        &resources.name,
        &resources.work,
        &resources.host_image,
        &resources.source,
        "release",
        &resources.evidence,
    )
    .await?;
    let hosts = workload::hosts_ready(&workload::HostsReadyInput {
        lifecycle: &resources.lifecycle,
        cluster: &resources.name,
        work: &resources.work,
        namespace: &resources.name,
        image: &resources.host_image,
        runtime_digest: &digest,
        replicas: 1,
        evidence: &resources.evidence,
    })
    .await?;
    let pod = &hosts.pods["items"][0];
    let name = pod["metadata"]["name"]
        .as_str()
        .context("host pod has a name")?;
    let uid = pod["metadata"]["uid"]
        .as_str()
        .context("host pod has a UID")?;
    let restarts = pod["status"]["containerStatuses"][0]["restartCount"]
        .as_u64()
        .context("host has a restart count")?;
    let secret: Value =
        serde_json::from_slice(&fs::read(&cluster.inputs.route_caller_secret_output)?)?;
    let principal = secret["metadata"]["annotations"]["wamn.io/principal-id"]
        .as_str()
        .context("existing route caller has a service principal")?;
    let (mut client, connection) =
        tokio_postgres::connect(&route.database_url, tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    let (lock, lock_connection) =
        tokio_postgres::connect(&route.database_url, tokio_postgres::NoTls).await?;
    let lock_connection = tokio::spawn(lock_connection);
    let result = recover(cluster, &mut client, &lock, principal, name).await;
    drop(lock);
    lock_connection.abort();
    drop(client);
    connection.abort();
    result?;
    let warm: Value = serde_json::from_slice(
        &checked(kubectl(resources).args([
            "-n",
            &resources.name,
            "get",
            "pod",
            name,
            "-o",
            "json",
        ]))
        .await?,
    )?;
    fs::write(
        resources.evidence.join("queue-host-restarted.json"),
        serde_json::to_vec_pretty(&warm)?,
    )?;
    ensure!(
        warm["metadata"]["uid"] == uid
            && warm["status"]["containerStatuses"][0]["restartCount"]
                .as_u64()
                .is_some_and(|count| count > restarts),
        "the same host pod must restart before durable completion"
    );
    super::assert_source_unchanged(resources).await
}

async fn recover(
    cluster: &ReceivingCluster,
    client: &mut tokio_postgres::Client,
    lock: &tokio_postgres::Client,
    principal: &str,
    pod: &str,
) -> anyhow::Result<()> {
    let tenant = super::super::TENANT;
    client
        .query_one("SELECT set_config('app.tenant',$1,false)", &[&tenant])
        .await?;
    // HTTP binds the fixed route-caller role. Automation requires an explicit
    // service-role assignment, as in the existing local queue fixture.
    client.execute(
        "INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,$3)",
        &[&tenant, &principal, &super::super::ROUTE_CALLER_ROLE],
    ).await?;
    let caller =
        wamn_runtime::plugins::flow_http_routing::queued_service_caller(client, tenant, principal)
            .await?;
    ensure!(
        caller.permits(super::super::BASE_RECORD_RECEIPT),
        "queue fixture must authorize its receipt operation before admission"
    );
    lock.batch_execute("BEGIN").await?;
    lock.query_one("SELECT set_config('app.tenant',$1,true)", &[&tenant])
        .await?;
    lock.query_one("SELECT id FROM receiving.purchase_order WHERE id='00000000-0000-0000-0000-000000000304' FOR UPDATE", &[]).await?;
    let blocker: i32 = lock.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
    let request = EnqueueRun {
        tenant: tenant.to_owned(),
        environment: super::super::ENVIRONMENT.to_owned(),
        package_id: super::super::BASE_PACKAGE_ID.to_owned(),
        effective_release_id: super::super::RELEASE_ID,
        wiring_id: "receiving_record_receipt".to_owned(),
        wiring_version: 1,
        service_principal_id: principal.to_owned(),
        idempotency_key: "deployed-queue-recovery".to_owned(),
        input: json!([{"request_id":"queue-recovery","value":{
            "idempotency_key":"deployed-queue-recovery","purchase_order_id":"00000000-0000-0000-0000-000000000304",
            "receipt_reference":"QUEUE-RECOVERY","occurred_at":"2026-08-31T12:34:00.000000Z",
            "line":[{"purchase_order_line_id":"00000000-0000-0000-0000-000000000504","quantity":"9.0000",
                "location_id":"00000000-0000-0000-0000-000000000201"}]}}]),
    };
    let run = enqueue(client, &BareSchemaName::new("wamn_run")?, &request).await?;
    let first = blocked_attempt(client, &run, blocker, 0).await?;
    fs::write(
        cluster
            .resources
            .evidence
            .join("queue-before-interruption.json"),
        serde_json::to_vec_pretty(&json!({
            "run":run,"lease_generation":first.0,"backend_pid":first.1,"pod":pod,
        }))?,
    )?;
    // SIGKILL models abrupt process loss, so no graceful error settlement can replace recovery.
    let killed = kubectl(&cluster.resources)
        .args([
            "-n",
            &cluster.resources.name,
            "exec",
            pod,
            "-c",
            "host",
            "--",
            "/bin/sh",
            "-c",
            "kill -KILL 1",
        ])
        .output()
        .await?;
    fs::write(
        cluster.resources.evidence.join("queue-interruption.json"),
        serde_json::to_vec_pretty(&json!({
            "exit_code":killed.status.code(),"stdout":String::from_utf8_lossy(&killed.stdout),"stderr":String::from_utf8_lossy(&killed.stderr),
        }))?,
    )?;
    let recovered = blocked_attempt(client, &run, blocker, first.0).await?;
    ensure!(
        recovered.1 != first.1,
        "recovery must execute on a new database connection"
    );
    lock.batch_execute("ROLLBACK").await?;
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let row = client
                .query_one(
                    "SELECT status,result_json FROM wamn_run.runs WHERE run_id=$1",
                    &[&run],
                )
                .await?;
            let status: String = row.get(0);
            if status != "running" && status != "dispatched" {
                ensure!(status == "completed", "recovered run ended with {status}");
                return Ok::<Value, anyhow::Error>(row.get(1));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("recovered queue item did not complete within 30 seconds")??;
    ensure!(
        outcome[0]["request_id"] == "queue-recovery"
            && outcome[0]["value"]["receipt_id"].is_string(),
        "recovered command must return its committed receipt"
    );
    let count: i64 = client.query_one("SELECT count(*) FROM receiving.receipt WHERE idempotency_key='deployed-queue-recovery'", &[]).await?.get(0);
    ensure!(
        count == 1,
        "interrupted command must commit exactly one receipt"
    );
    ensure!(
        client
            .query_opt(
                "SELECT run_id FROM wamn_run.run_queue WHERE run_id=$1",
                &[&run]
            )
            .await?
            .is_none(),
        "completed run must leave the durable queue"
    );
    ensure!(
        enqueue(client, &BareSchemaName::new("wamn_run")?, &request).await? == run,
        "admission replay must return the completed run"
    );
    fs::write(
        cluster.resources.evidence.join("queue-recovery.json"),
        serde_json::to_vec_pretty(&json!({
            "source":cluster.resources.source,"run":run,"first_generation":first.0,"recovered_generation":recovered.0,
            "first_backend":first.1,"recovered_backend":recovered.1,"result":outcome,"receipt_count":count,"passed":true,
        }))?,
    )?;
    Ok(())
}

async fn blocked_attempt(
    client: &tokio_postgres::Client,
    run: &str,
    blocker: i32,
    after: i64,
) -> anyhow::Result<(i64, i32)> {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(row) = client.query_opt(
                "SELECT q.lease_generation,a.pid FROM wamn_run.run_queue q JOIN wamn_run.runs r USING (tenant_id,run_id) \
                 CROSS JOIN pg_stat_activity a WHERE q.run_id=$1 AND r.status='running' AND q.lease_generation>$2 \
                 AND $3=ANY(pg_blocking_pids(a.pid)) AND a.query LIKE '%purchase_order%'", &[&run,&after,&blocker],
            ).await? { return Ok::<_,anyhow::Error>((row.get(0),row.get(1))); }
            let status: String = client.query_one("SELECT status FROM wamn_run.runs WHERE run_id=$1", &[&run]).await?.get(0);
            ensure!(status == "running" || status == "dispatched", "queue item ended before interruption/recovery observation: {status}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.context("admitted queue item did not reach blocked application SQL within 120 seconds")?
}
