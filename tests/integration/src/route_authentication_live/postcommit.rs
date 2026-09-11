//! Receiving replay and bounded progress through its released materializer.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use async_nats::jetstream::{Context, message::StreamMessage, stream::Stream};
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio_postgres::Client;
use wamn_gate_harness::journey::PostcommitPhase;
use wamn_event_wire::{DeliveryAdvisory, DeliveryAdvisoryKind, Envelope, Op};
use wamn_runtime::plugins::wamn_jetstream::{
    RouterTapRecord, RouterTapRecordPhase, RouterTapSourceKind, router_tap_environment_filter,
};

use super::{
    BASE_PACKAGE_ID, ENVIRONMENT, JourneyDocument, MATERIALIZER_DURABLE, MATERIALIZER_STREAM,
    OVERLAY_PACKAGE_ID, PROJECT, TENANT, connect, connect_event_proof_client, overlay_route_path, secret_value,
};

const REGISTRATION: &str = "client_acme_receiving::quality.create_inspection";
const WIRING: &str = "quality_create_inspection";
const PROGRESS_BOUND: Duration = Duration::from_secs(90);

async fn kube(phase: &PostcommitPhase, arguments: &[&str]) -> anyhow::Result<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new("kubectl")
            .arg("--kubeconfig")
            .arg(&phase.kubeconfig)
            .arg("--context")
            .arg(&phase.context)
            .arg("--namespace")
            .arg(&phase.namespace)
            .arg("--request-timeout=10s")
            .args(arguments)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("the owned materializer command exceeded 15 seconds")??;
    ensure!(
        output.status.success(),
        "owned materializer command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("decode the owned materializer object")
}

async fn scale(phase: &PostcommitPhase, uid: &str, replicas: u64) -> anyhow::Result<()> {
    let patch = json!([
        {"op":"test", "path":"/metadata/uid", "value":uid},
        {"op":"replace", "path":"/spec/replicas", "value":replicas}
    ])
    .to_string();
    kube(
        phase,
        &[
            "patch",
            "workloaddeployment",
            &phase.materializer_workload,
            "--type=json",
            "-p",
            &patch,
            "-o",
            "json",
        ],
    )
    .await?;
    Ok(())
}

async fn owned_workloads(phase: &PostcommitPhase, replica_uid: &str) -> anyhow::Result<Vec<Value>> {
    let document = kube(phase, &["get", "workloads", "-o", "json"]).await?;
    Ok(document["items"]
        .as_array()
        .context("Workload list carries items")?
        .iter()
        .filter(|item| {
            item["metadata"]["ownerReferences"]
                .as_array()
                .is_some_and(|owners| owners.iter().any(|owner| owner["uid"] == replica_uid))
        })
        .cloned()
        .collect())
}

async fn ready(phase: &PostcommitPhase) -> anyhow::Result<Value> {
    let deadline = Instant::now() + PROGRESS_BOUND;
    loop {
        let deployment = kube(
            phase,
            &[
                "get",
                "workloaddeployment",
                &phase.materializer_workload,
                "-o",
                "json",
            ],
        )
        .await?;
        if deployment["spec"]["replicas"] == 1
            && deployment["status"]["conditions"]
                .as_array()
                .is_some_and(|conditions| {
                    conditions.iter().any(|condition| {
                        condition["type"] == "Ready" && condition["status"] == "True"
                    })
                })
        {
            return Ok(deployment);
        }
        ensure!(
            Instant::now() < deadline,
            "owned materializer did not become Ready within 90 seconds"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn request(
    http: &reqwest::Client,
    document: &JourneyDocument,
    phase: &PostcommitPhase,
    path: &str,
    body: Value,
) -> anyhow::Result<Value> {
    let token = secret_value(&document.route_caller_secret_output, "token")?;
    let response = http
        .post(format!(
            "{}{}",
            phase.route_endpoint.trim_end_matches('/'),
            path
        ))
        .header("Host", &document.route_host)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "released Receiving route returned {}",
        response.status()
    );
    let result: Value = response.json().await?;
    ensure!(
        result.as_array().is_some_and(|items| items.len() == 1)
            && result[0]["request_id"] == body[0]["request_id"]
            && result[0].get("value").is_some()
            && result[0].get("error").is_none(),
        "released Receiving command refused: {result}"
    );
    Ok(result[0]["value"].clone())
}

async fn inspection(project: &Client, receipt: &str) -> anyhow::Result<Value> {
    Ok(project
        .query_one(
            "SELECT COALESCE(jsonb_agg(to_jsonb(row)), '[]'::jsonb) \
         FROM receiving.quality_inspection AS row WHERE receipt_id = $1::text::uuid",
            &[&receipt],
        )
        .await?
        .get(0))
}

async fn source(stream: &mut Stream, receipt: &str) -> anyhow::Result<StreamMessage> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = stream.info().await?.state.clone();
        if state.messages > 0 {
            for sequence in state.first_sequence..=state.last_sequence {
                let message = stream.get_raw_message(sequence).await?;
                if let Ok(event) = serde_json::from_slice::<Envelope>(&message.payload)
                    && event.package_id == BASE_PACKAGE_ID
                    && event.entity == "receipt"
                    && event.op == Op::Insert
                    && event
                        .new
                        .as_ref()
                        .and_then(|row| row.get("id"))
                        .and_then(Value::as_str)
                        == Some(receipt)
                {
                    return Ok(message);
                }
            }
        }
        ensure!(
            Instant::now() < deadline,
            "real receipt {receipt} did not reach the event stream within 30 seconds"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn source_receipt(message: &StreamMessage) -> Value {
    json!({"subject":message.subject.as_str(), "sequence":message.sequence,
        "headers":message.headers, "body":serde_json::from_slice::<Value>(&message.payload).unwrap_or(Value::Null)})
}

fn consumer_receipt(info: &async_nats::jetstream::consumer::Info) -> Value {
    json!({"name":info.name, "config":info.config,
        "delivered":{"consumer_sequence":info.delivered.consumer_sequence,
            "stream_sequence":info.delivered.stream_sequence},
        "ack_floor":{"consumer_sequence":info.ack_floor.consumer_sequence,
            "stream_sequence":info.ack_floor.stream_sequence},
        "num_ack_pending":info.num_ack_pending,"num_pending":info.num_pending,
        "num_redelivered":info.num_redelivered})
}

async fn delivery_taps(
    subscriber: &mut async_nats::Subscriber,
    delivery_id: &str,
    expected_input: &Value,
    expected_outcome: &str,
) -> anyhow::Result<Vec<Value>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut observed = Vec::new();
    let mut accepted = false;
    let mut settled = false;
    while !accepted || !settled {
        let message = tokio::time::timeout_at(deadline, subscriber.next())
            .await
            .context(
                "the private handler did not produce both delivery boundaries within 30 seconds",
            )?
            .context("the router tap subscription ended")?;
        let record: RouterTapRecord = serde_json::from_slice(&message.payload)?;
        if record.delivery_id.as_ref() != delivery_id {
            continue;
        }
        record.validate()?;
        ensure!(
            record.source_id.as_ref() == REGISTRATION
                && record.source_kind == RouterTapSourceKind::Registration
                && record.wiring_id.as_ref() == WIRING
                && record.wiring_version == 1
                && !record.redacted
                && record.over_ceiling_bytes.is_none(),
            "the delivery tap does not identify the unchanged private handler"
        );
        match record.phase {
            RouterTapRecordPhase::Accepted => {
                ensure!(
                    !accepted && record.payload == *expected_input,
                    "duplicate handler input differs from the original event"
                );
                accepted = true;
            }
            RouterTapRecordPhase::Settled => {
                ensure!(
                    !settled && record.outcome.as_deref() == Some(expected_outcome),
                    "private handler settled with the wrong outcome"
                );
                settled = true;
            }
        }
        observed.push(json!({"subject":message.subject.as_str(), "record":record}));
    }
    Ok(observed)
}

async fn fixture(project: &Client, label: &str) -> anyhow::Result<(String, String)> {
    let order = uuid::Uuid::new_v4().to_string();
    let line = uuid::Uuid::new_v4().to_string();
    project.execute(
        "INSERT INTO receiving.purchase_order \
         (id,purchase_order_number,supplier_id,status,row_version,created_at,updated_at,acme_inspection_required,acme_quality_status) \
         VALUES ($1::text::uuid,$2,'00000000-0000-0000-0000-000000000404','open',1,now(),now(),true,'pending')",
        &[&order, &format!("POSTCOMMIT-{label}-{order}")],
    ).await?;
    project.execute(
        "INSERT INTO receiving.purchase_order_line (id,purchase_order_id,line_number,item_id,ordered_quantity,received_quantity) \
         VALUES ($1::text::uuid,$2::text::uuid,1,'00000000-0000-0000-0000-000000000101',1.0000,0.0000)",
        &[&line, &order],
    ).await?;
    Ok((order, line))
}

async fn receipt(
    http: &reqwest::Client,
    document: &JourneyDocument,
    phase: &PostcommitPhase,
    fixture: &(String, String),
    label: &str,
) -> anyhow::Result<String> {
    let value = request(http, document, phase, overlay_route_path("receiving_record_receipt"), json!([{
        "request_id":label, "value":{"idempotency_key":format!("{label}-{}",fixture.0),
        "purchase_order_id":fixture.0, "receipt_reference":format!("{label}-{}",fixture.0),
        "occurred_at":"2026-09-10T12:00:00.000000Z", "line":[{"purchase_order_line_id":fixture.1,
            "quantity":"1.0000", "location_id":"00000000-0000-0000-0000-000000000201"}]}
    }])).await?;
    Ok(value["receipt_id"]
        .as_str()
        .context("the real receipt command returned an identity")?
        .to_owned())
}

async fn matching_delivery_advisory(
    jetstream: &Context,
    sequence: u64,
) -> anyhow::Result<Option<(String, DeliveryAdvisory)>> {
    let mut stream = jetstream
        .get_stream(wamn_event_wire::DELIVERY_ADVISORY_STREAM)
        .await?;
    let state = stream.info().await?.state.clone();
    if state.messages == 0 {
        return Ok(None);
    }
    for index in state.first_sequence..=state.last_sequence {
        let message = stream.get_raw_message(index).await?;
        let advisory = DeliveryAdvisory::from_slice(&message.payload)?;
        if advisory.stream == MATERIALIZER_STREAM
            && advisory.consumer == MATERIALIZER_DURABLE
            && advisory.stream_seq == sequence
        {
            return Ok(Some((message.subject.to_string(), advisory)));
        }
    }
    Ok(None)
}

async fn prove(
    document: &JourneyDocument,
    phase: &PostcommitPhase,
    project: &Client,
    lock: &Client,
    evidence: &mut Value,
) -> anyhow::Result<()> {
    let materializer = document
        .materializer
        .as_ref()
        .context("the causal materializer baseline must run first")?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(60))
        .build()?;
    let nats = connect_event_proof_client(
        &materializer.nats_url,
        "WAMN_EVT_NATS_USERNAME",
        "WAMN_EVT_NATS_PASSWORD_FILE",
    ).await?;
    let jetstream = async_nats::jetstream::new(nats.clone());
    let mut events = jetstream.get_stream(MATERIALIZER_STREAM).await?;
    let mut taps = nats
        .subscribe(
            router_tap_environment_filter(TENANT, PROJECT, ENVIRONMENT)
                .context("the journey identifies a router tap subject")?,
        )
        .await?;
    nats.flush().await?;
    let graph: Value = project.query_one(
        "SELECT graph_json FROM catalog.wirings WHERE tenant_id=$1 AND package_id=$2 AND wiring_id=$3 AND version=1",
        &[&TENANT, &OVERLAY_PACKAGE_ID, &WIRING],
    ).await?.get(0);
    ensure!(
        graph["nodes"]
            .as_object()
            .is_some_and(|nodes| nodes.len() == 1)
            && graph["entry"] == "operation"
            && graph["nodes"]["operation"]["operation"]
                == "client-acme-receiving:quality/create-inspection@3.0.0"
            && graph["nodes"]["operation"]["config"].get("retry").is_none(),
        "the proof requires the unchanged one-node private handler and default retry policy"
    );
    evidence["wiring"] = graph;
    let registration: Value = project.query_one(
        "SELECT registration FROM catalog.event_registrations WHERE tenant_id=$1 AND package_id=$2 AND registration_id='quality.create_inspection'",
        &[&TENANT, &OVERLAY_PACKAGE_ID],
    ).await?.get(0);
    evidence["registration"] = registration;
    let original = source(&mut events, &materializer.receipt_id).await?;
    let before_consumer = events.consumer_info(MATERIALIZER_DURABLE).await?;
    ensure!(
        before_consumer.num_ack_pending == 0
            && before_consumer.num_pending == 0
            && before_consumer.ack_floor.stream_sequence >= original.sequence,
        "the first handler must finish and acknowledge before replay"
    );
    evidence["original_source"] = source_receipt(&original);
    evidence["consumer_before"] = consumer_receipt(&before_consumer);
    let approved = request(&http, document, phase, overlay_route_path("quality_approve_inspection"), json!([{
        "request_id":"postcommit-approve", "receipt_id":materializer.receipt_id, "expected_row_version":"1"
    }])).await?;
    let approved_state = inspection(project, &materializer.receipt_id).await?;
    ensure!(
        approved_state
            .as_array()
            .is_some_and(|rows| rows.len() == 1)
            && approved_state[0]["status"] == "approved"
            && approved_state[0]["row_version"] == 2,
        "the real approval command did not establish a distinguishable replay state: {approved}"
    );
    evidence["approved_state"] = approved_state.clone();
    let dedup = events.info().await?.config.duplicate_window;
    ensure!(
        dedup > Duration::ZERO && dedup <= Duration::from_secs(120),
        "event deduplication horizon exceeds the 150-second replay budget"
    );
    let wait = dedup + Duration::from_secs(2);
    evidence["dedup_wait_ms"] = json!(wait.as_millis());
    tokio::time::sleep(wait).await;
    let replay_publisher = async_nats::jetstream::new(connect_event_proof_client(
        &materializer.nats_url,
        "WAMN_EVT_NATS_REPLAY_USERNAME",
        "WAMN_EVT_NATS_REPLAY_PASSWORD_FILE",
    ).await?);
    let replay = replay_publisher
        .publish_with_headers(
            original.subject.clone(),
            original.headers.clone(),
            original.payload.clone(),
        )
        .await?
        .await?;
    ensure!(
        !replay.duplicate
            && replay.stream == MATERIALIZER_STREAM
            && replay.sequence > original.sequence,
        "broker deduplication prevented the required second handler delivery"
    );
    let envelope: Envelope = serde_json::from_slice(&original.payload)?;
    let source_id = wamn_event_wire::msg_id(PROJECT, ENVIRONMENT, envelope.lsn);
    let expected_input = json!({"event":"insert", "new":envelope.new});
    let replay_id = format!("{REGISTRATION}:event:{}:{source_id}", replay.sequence);
    evidence["replay_taps"] =
        json!(delivery_taps(&mut taps, &replay_id, &expected_input, "discard").await?);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let consumer = events.consumer_info(MATERIALIZER_DURABLE).await?;
        if consumer.ack_floor.stream_sequence >= replay.sequence && consumer.num_ack_pending == 0 {
            ensure!(
                consumer.ack_floor.consumer_sequence
                    == before_consumer.ack_floor.consumer_sequence + 1,
                "replay did not acknowledge exactly one further registered delivery"
            );
            evidence["consumer_after_replay"] = consumer_receipt(&consumer);
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "the replay did not acknowledge within 30 seconds"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ensure!(
        inspection(project, &materializer.receipt_id).await? == approved_state,
        "duplicate delivery reset the approved inspection or created another row"
    );
    evidence["replay_sequence"] = json!(replay.sequence);

    let original_deployment = kube(
        phase,
        &[
            "get",
            "workloaddeployment",
            &phase.materializer_workload,
            "-o",
            "json",
        ],
    )
    .await?;
    ensure!(
        original_deployment["spec"]["replicas"] == 1,
        "the owned materializer must start with one replica"
    );
    let uid = original_deployment["metadata"]["uid"]
        .as_str()
        .context("the deployment has a UID")?;
    let replica_name = original_deployment["status"]["currentReplicaSet"]["name"]
        .as_str()
        .context("the materializer has a native ReplicaSet")?;
    let replica = kube(
        phase,
        &["get", "workloadreplicaset", replica_name, "-o", "json"],
    )
    .await?;
    let replica_uid = replica["metadata"]["uid"]
        .as_str()
        .context("the ReplicaSet has a UID")?;
    let old_workloads = owned_workloads(phase, replica_uid).await?;
    ensure!(
        old_workloads.len() == 1,
        "the owned materializer must have exactly one native Workload"
    );
    evidence["deployment_before"] = original_deployment.clone();
    evidence["workloads_before"] = json!(old_workloads);
    let poison_fixture = fixture(project, "poison").await?;
    let valid_fixture = fixture(project, "independent").await?;
    // Every exit from this scope rolls back the observer lock and restores the replica.
    let result: anyhow::Result<()> = async {
        scale(phase, uid, 0).await?;
        let deadline = Instant::now() + PROGRESS_BOUND;
        loop {
            if owned_workloads(phase, replica_uid).await?.is_empty() { break; }
            ensure!(Instant::now() < deadline, "the owned materializer Workload did not stop within 90 seconds");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        evidence["stopped_workload_uid"] = old_workloads[0]["metadata"]["uid"].clone();
        let poison_receipt = receipt(&http, document, phase, &poison_fixture, "postcommit-poison").await?;
        let poison_source = source(&mut events, &poison_receipt).await?;
        ensure!(inspection(project, &poison_receipt).await? == json!([]), "poison handler ran before the controlled lock");
        lock.batch_execute("BEGIN").await?;
        lock.query_one("SELECT id FROM receiving.receipt WHERE id=$1::text::uuid FOR UPDATE", &[&poison_receipt]).await?;
        let blocker: i32 = lock.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
        evidence["poison_source"] = source_receipt(&poison_source);
        evidence["blocker_pid"] = json!(blocker);
        let started = Instant::now();
        let deadline = started + PROGRESS_BOUND;
        scale(phase, uid, 1).await?;
        let mut attempts = BTreeSet::new();
        let mut waits = Vec::new();
        let observed = async {
        loop {
            for row in project.query(
                "SELECT pid, query_start::text, query, wait_event, \
                 EXTRACT(epoch FROM clock_timestamp()-query_start)::float8 \
                 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) \
                 AND wait_event_type='Lock' AND query ILIKE '%INSERT INTO quality_inspection%'",
                &[&blocker],
            ).await? {
                let pid: i32 = row.get(0);
                let query_start: String = row.get(1);
                if attempts.insert((pid, query_start.clone())) {
                    waits.push(json!({"pid":pid,"query_start":query_start,"query":row.get::<_,String>(2),
                        "wait_event":row.get::<_,String>(3),"observed_seconds":row.get::<_,f64>(4)}));
                    evidence["blocked_handler_attempts"] = json!(waits);
                }
            }
            if let Some(advisory) = matching_delivery_advisory(&jetstream, poison_source.sequence).await? { return Ok::<_, anyhow::Error>(advisory); }
            ensure!(Instant::now() < deadline, "poison did not produce a retained broker termination advisory within 90 seconds");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        };
        let (valid_receipt, (subject, advisory)) = tokio::try_join!(
            receipt(&http, document, phase, &valid_fixture, "postcommit-independent"), observed,
        )?;
        let resumed = ready(phase).await?;
        ensure!(resumed["spec"]["template"] == original_deployment["spec"]["template"], "materializer restart changed the deployed artifacts or configuration");
        evidence["deployment_resumed"] = resumed;
        ensure!(attempts.len() == 3, "expected three real blocked handler attempts, observed {}", attempts.len());
        ensure!(subject == format!("$JS.EVENT.ADVISORY.CONSUMER.MSG_TERMINATED.{MATERIALIZER_STREAM}.{MATERIALIZER_DURABLE}")
            && advisory.kind == DeliveryAdvisoryKind::Terminated
            && advisory.deliveries == 1
            && advisory.stream_seq == poison_source.sequence,
            "broker advisory does not identify the actual terminated poison delivery: {advisory:?}");
        evidence["delivery_advisory"] = json!({"subject":subject,"record":advisory});
        loop {
            let state = inspection(project, &valid_receipt).await?;
            if state.as_array().is_some_and(|rows| rows.len() == 1)
                && state[0]["status"] == "pending" && state[0]["row_version"] == 1
            {
                evidence["independent_inspection"] = state;
                break;
            }
            ensure!(Instant::now() < deadline, "valid independent receipt did not progress within 90 seconds");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ensure!(inspection(project, &poison_receipt).await? == json!([]), "failed poison committed an inspection");
        ensure!(inspection(project, &materializer.receipt_id).await? == approved_state, "poison handling changed the approved replay state");
        let valid_source = source(&mut events, &valid_receipt).await?;
        ensure!(valid_source.sequence > poison_source.sequence, "independent valid event did not follow poison");
        evidence["independent_source"] = source_receipt(&valid_source);
        loop {
            let consumer = events.consumer_info(MATERIALIZER_DURABLE).await?;
            if consumer.ack_floor.stream_sequence >= valid_source.sequence
                && consumer.num_ack_pending == 0 && consumer.num_pending == 0
            {
                evidence["consumer_after_poison"] = consumer_receipt(&consumer);
                break;
            }
            ensure!(Instant::now() < deadline, "poison and valid deliveries did not settle within 90 seconds");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let progress_elapsed = started.elapsed();
        evidence["progress_elapsed_ms"] = json!(progress_elapsed.as_millis());
        ensure!(progress_elapsed <= PROGRESS_BOUND,
            "poison and independent receipt exceeded the 90-second progress bound: {} ms",
            progress_elapsed.as_millis());
        evidence["retry_policy"] = json!({"handler_attempts":3,"backoff_ms":[100,200],
            "statement_timeout_ms":phase.statement_timeout_ms,"materializer_max_deliver":5,
            "broker_delivered":1,"progress_bound_seconds":90});
        Ok(())
    }.await;
    let rollback = lock
        .batch_execute("ROLLBACK")
        .await
        .context("release the exact poison receipt lock");
    let restoration = async {
        scale(phase, uid, 1).await?;
        ready(phase).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    evidence["cleanup"] =
        json!({"lock_released":rollback.is_ok(),"materializer_restored":restoration.is_ok()});
    rollback?;
    restoration?;
    result?;
    let registration_after: Value = project.query_one(
        "SELECT registration FROM catalog.event_registrations WHERE tenant_id=$1 AND package_id=$2 AND registration_id='quality.create_inspection'",
        &[&TENANT, &OVERLAY_PACKAGE_ID],
    ).await?.get(0);
    ensure!(
        registration_after == evidence["registration"],
        "post-commit proof changed its registration"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires the disposable Receiving journey after its causal materializer baseline"]
async fn production_materializer_preserves_replay_and_progress() -> anyhow::Result<()> {
    let document = JourneyDocument::required()?;
    let phase = document
        .postcommit
        .as_ref()
        .context("the journey omitted the armed postcommit phase")?;
    ensure!(
        phase.context == "kind-wamn-receiving-postcommit"
            && phase.namespace == "wamn-receiving-postcommit"
            && phase.materializer_workload == "receiving-materializer"
            && phase.kubeconfig.is_file(),
        "post-commit proof requires the exact owned disposable Receiving materializer"
    );
    ensure!(
        phase.source_commit.len() == 40
            && phase
                .source_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            && phase.statement_timeout_ms > 0
            && phase.statement_timeout_ms <= 10_000,
        "post-commit proof requires source identity and a bounded deployed statement timeout"
    );
    ensure!(
        !phase.evidence_file.exists(),
        "post-commit evidence must use a fresh output file"
    );
    let materializer = document
        .materializer
        .as_ref()
        .context("the journey omitted its materializer baseline")?;
    let (project, project_task) = connect(&materializer.project_pg_url).await?;
    let (lock, lock_task) = connect(&materializer.project_pg_url).await?;
    let mut evidence = json!({"schema":"wamn-receiving-postcommit/v0.1", "source":phase.source_commit,
        "verdict":"fail", "invariants":["REC-EVENT-REPLAY","REC-POSTCOMMIT-PROGRESS"], "recovery_assumptions":"The owned database, broker, host, and materializer remain available. The poison lock lasts through retry exhaustion."});
    let result = prove(
        &document,
        phase,
        project.as_ref(),
        lock.as_ref(),
        &mut evidence,
    )
    .await;
    project_task.abort();
    lock_task.abort();
    match &result {
        Ok(()) => evidence["verdict"] = json!("pass"),
        Err(error) => evidence["failure"] = json!(format!("{error:#}")),
    }
    std::fs::write(&phase.evidence_file, serde_json::to_vec_pretty(&evidence)?)?;
    result?;
    println!(
        "RECEIVING_POSTCOMMIT_PASS replay_deliveries=2 blocked_handler_attempts=3 termination_advisories=1 independent_inspections=1"
    );
    Ok(())
}
