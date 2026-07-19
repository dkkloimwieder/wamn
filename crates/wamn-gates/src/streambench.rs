//! The `streambench` gate: the in-cluster gate of record for the event-plane
//! DATA-PLANE NATS (D19 v3 §5/§7 Phase 1; wamn-l5i9.7 [EVT-NATS]).
//!
//! Unlike the ceiling campaigns (walbench/outboxbench/queuebench ceiling), this
//! is a pass/fail GATE: it proves the JetStream substrate the CDC reader
//! (l5i9.10) publishes onto and the materializer (l5i9.17) consumes from behaves
//! to the v3 contract, on the dedicated data-plane cluster (deploy/nats-
//! jetstream.yaml), leaving the control-plane/doorbell NATS untouched.
//!
//! It asserts the four load-bearing claims of the stand-up:
//!   * **publish → the EVT_ stream** (subjects `evt.<org>.<project>.<env>.
//!     <entity>.<op>`, R3 file storage) stores exactly what was published;
//!   * **Nats-Msg-Id dedupe** — re-publishing an event with the same
//!     `<project_env>:<lsn>` id inside the duplicate window is a no-op
//!     (`PubAck.duplicate`, stream count unchanged) — the fast-path half of the
//!     exactly-once guarantee (§5);
//!   * **consume in commit order** — a pull consumer drains every message, each
//!     carrying its Nats-Msg-Id header, delivered in stream order == the LSN
//!     order they were published (stronger than the outbox's per-project seq);
//!   * **R3 survives node loss** — proven two ways: a self-contained RAFT
//!     leader step-down + re-election (`--mode all`), and a physical pod
//!     deletion (the two-step `publish` → `kubectl delete pod` → `heal`
//!     runbook, deploy/gates/streambench-job.yaml).
//!
//! Accounts: this stand-up runs on the single shared (default) account — the
//! per-org account model + replication credentials are the wamn-4xw seam
//! (§11); the subject namespace already reserves per-org isolation.
//!
//! Pure NATS client (no wasm, no Postgres): the substrate is a NATS mechanism.
//! `async-nats` 0.47 is already a workspace dep (queuebench/dispatcher doorbell).

use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use async_nats::jetstream::stream::{Config as StreamConfig, RetentionPolicy, StorageType};
use async_nats::{Client, HeaderMap};
use bytes::Bytes;
use clap::{Args, ValueEnum};
use futures_util::StreamExt as _;
use wamn_gate_harness::check;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Create a fresh EVT_ stream, publish N events, prove dedupe. Leaves the
    /// stream populated (the first half of the physical node-loss runbook).
    Publish,
    /// Drain the stream through a pull consumer; assert count / headers / order.
    Consume,
    /// Focused Nats-Msg-Id dedupe check.
    Dedupe,
    /// Verify the stream survived a node deletion (the second half of the
    /// runbook): stream + all messages present, R3 config intact, drainable.
    Heal,
    /// publish + consume + dedupe + a self-contained R3 leader-stepdown proof.
    All,
}

#[derive(Debug, Args)]
pub struct StreamBenchArgs {
    /// Data-plane NATS URL (deploy/infra/nats-jetstream.yaml Service `evt-nats`).
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,

    /// Which check to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Org slug — names the stream `EVT_<org>_<env>` and the subject root.
    #[arg(long, default_value = "demo")]
    pub org: String,

    /// Project slug (subject segment; part of the `<project>_<env>` dedupe id).
    #[arg(long, default_value = "receiving")]
    pub project: String,

    /// Env slug (`prod`/`dev`; names the stream + subject).
    #[arg(long, default_value = "prod")]
    pub env: String,

    /// Number of events to publish / expect.
    #[arg(long, short = 'n', default_value_t = 500)]
    pub messages: usize,

    /// Stream replication factor. 3 = R3 (in-cluster / a 3-node local cluster);
    /// 1 = single-node local iteration (the R3 stepdown proof is skipped).
    #[arg(long, default_value_t = 3)]
    pub replicas: usize,

    /// JetStream duplicate window (Nats-Msg-Id dedupe horizon), seconds.
    #[arg(long, default_value_t = 120)]
    pub dup_window_secs: u64,

    /// heal mode: how many messages must have survived the node loss
    /// (defaults to --messages).
    #[arg(long)]
    pub expect_messages: Option<usize>,
}

impl StreamBenchArgs {
    fn stream_name(&self) -> String {
        format!("EVT_{}_{}", self.org, self.env)
    }
    /// The stream binds every project's events for this org+env.
    fn stream_subjects(&self) -> String {
        format!("evt.{}.*.{}.>", self.org, self.env)
    }
    /// `<project>_<env>` — the Nats-Msg-Id prefix the reader keys dedupe on.
    fn project_env(&self) -> String {
        format!("{}_{}", self.project, self.env)
    }
}

/// A CDC-shaped envelope (a stand-in for the reader's real pgoutput event) plus
/// the fields the gate asserts on.
#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    op: String,
    entity: String,
    lsn: u64,
    project_env: String,
    seq: usize,
}

const ENTITIES: [&str; 3] = ["receipts", "receipt_lines", "quality_holds"];
const OPS: [&str; 3] = ["insert", "update", "delete"];
/// A fixed synthetic base LSN so the ids look like real confirmed positions.
const LSN_BASE: u64 = 0x0100_0000;

/// The i-th event's subject, envelope, LSN, and Nats-Msg-Id.
fn event(args: &StreamBenchArgs, i: usize) -> (String, u64, String, Bytes) {
    let entity = ENTITIES[i % ENTITIES.len()];
    let op = OPS[i % OPS.len()];
    let lsn = LSN_BASE + i as u64;
    let subject = format!(
        "evt.{}.{}.{}.{}.{}",
        args.org, args.project, args.env, entity, op
    );
    let msg_id = format!("{}:{lsn}", args.project_env());
    let env = Envelope {
        op: op.to_string(),
        entity: entity.to_string(),
        lsn,
        project_env: args.project_env(),
        seq: i,
    };
    let payload = Bytes::from(serde_json::to_vec(&env).expect("serialize envelope"));
    (subject, lsn, msg_id, payload)
}

/// Connect with a bounded retry — a data-plane node may be mid-restart when the
/// heal mode runs (that is the point).
async fn connect(url: &str) -> anyhow::Result<Client> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match async_nats::connect(url).await {
            Ok(c) => return Ok(c),
            Err(e) if Instant::now() < deadline => {
                println!("(waiting for data-plane NATS at {url}: {e})");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => bail!("connect to data-plane NATS at {url}: {e}"),
        }
    }
}

pub async fn run(args: StreamBenchArgs) -> anyhow::Result<()> {
    println!(
        "streambench — data-plane JetStream at {} (stream {}, R{}, subjects {})",
        args.nats_url,
        args.stream_name(),
        args.replicas,
        args.stream_subjects(),
    );

    let client = connect(&args.nats_url).await?;
    let mut js = async_nats::jetstream::new(client);
    // A degraded (post-node-loss) cluster can take several seconds to
    // re-stabilize its meta/stream RAFT groups; the default 5 s API timeout is
    // too tight for the heal path.
    js.set_timeout(Duration::from_secs(20));

    let mut pass = true;
    match args.mode {
        Mode::Publish => {
            recreate_stream(&js, &args).await?;
            pass &= publish_phase(&js, &args).await?;
        }
        Mode::Consume => {
            pass &= consume_phase(&js, &args, args.messages).await?;
        }
        Mode::Dedupe => {
            recreate_stream(&js, &args).await?;
            pass &= dedupe_phase(&js, &args).await?;
        }
        Mode::Heal => {
            pass &= heal_phase(&js, &args).await?;
        }
        Mode::All => {
            recreate_stream(&js, &args).await?;
            pass &= publish_phase(&js, &args).await?;
            pass &= dedupe_phase(&js, &args).await?;
            pass &= consume_phase(&js, &args, args.messages).await?;
            pass &= stepdown_phase(&js, &args).await?;
        }
    }

    println!("\nstreambench complete — overall PASS: {pass}");
    if !pass {
        bail!("an EVT-NATS streambench assert failed");
    }
    Ok(())
}

/// Delete + recreate the EVT_ stream so a run starts from a known state.
async fn recreate_stream(
    js: &async_nats::jetstream::Context,
    args: &StreamBenchArgs,
) -> anyhow::Result<()> {
    let name = args.stream_name();
    // Ignore "stream not found" on first run.
    let _ = js.delete_stream(&name).await;
    js.create_stream(StreamConfig {
        name: name.clone(),
        subjects: vec![args.stream_subjects()],
        storage: StorageType::File,
        num_replicas: args.replicas,
        retention: RetentionPolicy::Limits,
        duplicate_window: Duration::from_secs(args.dup_window_secs),
        ..Default::default()
    })
    .await
    .with_context(|| format!("create stream {name} (R{})", args.replicas))?;
    Ok(())
}

/// Print the stream's cluster state and return (message_count, leader, peers).
async fn stream_state(
    js: &async_nats::jetstream::Context,
    name: &str,
) -> anyhow::Result<(u64, Option<String>, usize, usize)> {
    let stream = js.get_stream(name).await.context("get stream")?;
    let info = stream.get_info().await.context("stream info")?;
    let (leader, peers) = match &info.cluster {
        Some(c) => (c.leader.clone(), c.replicas.len()),
        None => (None, 0), // single-node (no clustering)
    };
    println!(
        "  stream {name}: messages={} num_replicas={} leader={:?} peers={}",
        info.state.messages, info.config.num_replicas, leader, peers
    );
    Ok((info.state.messages, leader, peers, info.config.num_replicas))
}

/// Publish N events, then re-publish them all (same Nats-Msg-Ids) and prove the
/// stream did not grow — dedupe holds across the batch.
async fn publish_phase(
    js: &async_nats::jetstream::Context,
    args: &StreamBenchArgs,
) -> anyhow::Result<bool> {
    let n = args.messages;
    println!("\n## publish — {n} events → {}", args.stream_name());
    for i in 0..n {
        let (subject, _lsn, msg_id, payload) = event(args, i);
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, msg_id.as_str());
        let ack = js
            .publish_with_headers(subject, headers, payload)
            .await
            .context("publish")?
            .await
            .context("publish ack")?;
        if ack.duplicate {
            bail!("event {i} unexpectedly deduped on first publish (id {msg_id})");
        }
    }
    let (stored, leader, _peers, num_replicas) = stream_state(js, &args.stream_name()).await?;

    let mut pass = true;
    check(
        &mut pass,
        &format!("stored == {n} after publish"),
        stored as usize == n,
    );
    check(
        &mut pass,
        &format!("stream is R{}", args.replicas),
        num_replicas == args.replicas,
    );
    if args.replicas > 1 {
        check(&mut pass, "a leader is elected", leader.is_some());
    }

    // Re-publish the identical batch: dedupe must keep the count at n.
    let mut dup_acks = 0usize;
    for i in 0..n {
        let (subject, _lsn, msg_id, payload) = event(args, i);
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, msg_id.as_str());
        let ack = js
            .publish_with_headers(subject, headers, payload)
            .await?
            .await?;
        if ack.duplicate {
            dup_acks += 1;
        }
    }
    let (stored_again, _l, _p, _r) = stream_state(js, &args.stream_name()).await?;
    check(
        &mut pass,
        &format!("re-publish deduped {n}/{n} acks"),
        dup_acks == n,
    );
    check(
        &mut pass,
        &format!("stored still {n} after re-publish (no growth)"),
        stored_again as usize == n,
    );
    Ok(pass)
}

/// Focused dedupe: one id published twice ⇒ one stored, second ack.duplicate.
async fn dedupe_phase(
    js: &async_nats::jetstream::Context,
    args: &StreamBenchArgs,
) -> anyhow::Result<bool> {
    println!("\n## dedupe — same Nats-Msg-Id twice ⇒ one stored");
    let name = args.stream_name();
    let (before, _l, _p, _r) = stream_state(js, &name).await?;

    let subject = format!(
        "evt.{}.{}.{}.receipts.insert",
        args.org, args.project, args.env
    );
    let msg_id = format!("{}:{}", args.project_env(), 0xDED0_u64);
    let payload = Bytes::from_static(b"{\"dedupe\":true}");

    let mut h1 = HeaderMap::new();
    h1.insert(NATS_MESSAGE_ID, msg_id.as_str());
    let a1 = js
        .publish_with_headers(subject.clone(), h1, payload.clone())
        .await?
        .await?;

    let mut h2 = HeaderMap::new();
    h2.insert(NATS_MESSAGE_ID, msg_id.as_str());
    let a2 = js.publish_with_headers(subject, h2, payload).await?.await?;

    let (after, _l2, _p2, _r2) = stream_state(js, &name).await?;

    let mut pass = true;
    check(&mut pass, "first publish is not a duplicate", !a1.duplicate);
    check(&mut pass, "second publish IS a duplicate", a2.duplicate);
    check(
        &mut pass,
        "stream grew by exactly 1 (dedupe held)",
        after == before + 1,
    );
    Ok(pass)
}

/// Drain the stream through a pull consumer; assert count, headers, and that
/// delivery order == commit order (strictly increasing LSN).
async fn consume_phase(
    js: &async_nats::jetstream::Context,
    args: &StreamBenchArgs,
    expect: usize,
) -> anyhow::Result<bool> {
    println!("\n## consume — drain {expect} in commit order (Nats-Msg-Id preserved)");
    let stream = js
        .get_stream(args.stream_name())
        .await
        .context("get stream")?;
    // Ephemeral pull consumer, DeliverPolicy::All — deterministic full drain
    // from the start every run. (The materializer uses a durable per flow.)
    // R1 in-memory: the consumer is transient bookkeeping; the durability
    // guarantee lives on the R3 stream. Forcing R1 lets the heal-mode drain
    // succeed while a node is still down (a fresh R3 consumer can't place 3
    // replicas during the outage — and it need not, to prove the data survived).
    let cfg = PullConfig {
        deliver_policy: DeliverPolicy::All,
        ack_policy: AckPolicy::Explicit,
        num_replicas: 1,
        memory_storage: true,
        ..Default::default()
    };
    // Retry: on a degraded cluster the first create may race the meta-group
    // re-stabilizing after a node loss.
    let consumer = {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match stream.create_consumer(cfg.clone()).await {
                Ok(c) => break c,
                Err(e) if Instant::now() < deadline => {
                    println!("(waiting to create consumer: {e})");
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                }
                Err(e) => return Err(anyhow::anyhow!("create pull consumer: {e}")),
            }
        }
    };

    let mut got = 0usize;
    let mut last_lsn: Option<u64> = None;
    let mut ordered = true;
    let mut all_have_id = true;
    let mut all_well_formed = true;
    let deadline = Instant::now() + Duration::from_secs(20);
    while got < expect && Instant::now() < deadline {
        let mut batch = consumer
            .fetch()
            .max_messages(expect - got)
            .messages()
            .await
            .context("fetch batch")?;
        let mut drained_any = false;
        while let Some(msg) = batch.next().await {
            let msg = msg.map_err(|e| anyhow::anyhow!("consume message: {e}"))?;
            drained_any = true;
            got += 1;

            if !msg.subject.starts_with(&format!("evt.{}.", args.org)) {
                all_well_formed = false;
            }
            let id_ok = msg
                .headers
                .as_ref()
                .and_then(|h| h.get(NATS_MESSAGE_ID))
                .map(|v| v.as_str().starts_with(&format!("{}:", args.project_env())))
                .unwrap_or(false);
            if !id_ok {
                all_have_id = false;
            }
            if let Ok(env) = serde_json::from_slice::<Envelope>(&msg.payload) {
                if let Some(prev) = last_lsn
                    && env.lsn <= prev
                {
                    ordered = false;
                }
                last_lsn = Some(env.lsn);
            } else {
                all_well_formed = false;
            }
            msg.ack().await.map_err(|e| anyhow::anyhow!("ack: {e}"))?;
        }
        if !drained_any {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    let mut pass = true;
    check(
        &mut pass,
        &format!("consumed {got}/{expect}"),
        got == expect,
    );
    check(
        &mut pass,
        "every message carried its Nats-Msg-Id",
        all_have_id,
    );
    check(&mut pass, "every subject was evt.<org>.…", all_well_formed);
    check(
        &mut pass,
        "delivery order == commit order (LSN increasing)",
        ordered,
    );
    Ok(pass)
}

/// Self-contained R3 durability: force a RAFT leader step-down, wait for
/// re-election, and prove the stream + all messages survived. No k8s needed.
async fn stepdown_phase(
    js: &async_nats::jetstream::Context,
    args: &StreamBenchArgs,
) -> anyhow::Result<bool> {
    println!("\n## R3 durability — leader step-down + re-election");
    let mut pass = true;
    if args.replicas < 3 {
        println!(
            "  (single/low-replica cluster: skipping RAFT stepdown — R3 heal is the in-cluster gate)"
        );
        return Ok(pass);
    }
    let name = args.stream_name();
    let (before, leader_before, _p, _r) = stream_state(js, &name).await?;

    // $JS.API.STREAM.LEADER.STEPDOWN.<name> — the Context prefixes $JS.API.
    let resp: serde_json::Value = js
        .request(
            format!("STREAM.LEADER.STEPDOWN.{name}"),
            &serde_json::json!({}),
        )
        .await
        .context("stream leader stepdown")?;
    check(
        &mut pass,
        "stepdown accepted",
        resp.get("success").and_then(|v| v.as_bool()) == Some(true),
    );

    // Wait for a new leader to settle.
    let deadline = Instant::now() + Duration::from_secs(20);
    let (msgs_after, leader_after) = loop {
        let (m, l, _p, _r) = stream_state(js, &name).await?;
        if l.is_some() || Instant::now() >= deadline {
            break (m, l);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    check(
        &mut pass,
        "a leader is re-elected after stepdown",
        leader_after.is_some(),
    );
    check(
        &mut pass,
        &format!("all {before} messages survived the re-election"),
        msgs_after == before,
    );
    // A brand-new leader is the strongest signal the raft group moved.
    if leader_before.is_some() && leader_after.is_some() {
        println!(
            "  leader {:?} → {:?} (moved={})",
            leader_before,
            leader_after,
            leader_before != leader_after
        );
    }
    Ok(pass)
}

/// The second half of the physical node-loss runbook: after `kubectl delete pod
/// evt-nats-<n>`, prove the stream + all messages are still there, R3 config is
/// intact, a leader is serving, and a fresh consumer can still drain everything.
async fn heal_phase(
    js: &async_nats::jetstream::Context,
    args: &StreamBenchArgs,
) -> anyhow::Result<bool> {
    let expect = args.expect_messages.unwrap_or(args.messages);
    println!("\n## heal — stream survived a node deletion (expect {expect} messages)");
    let name = args.stream_name();
    let (stored, leader, _peers, num_replicas) = stream_state(js, &name).await?;

    let mut pass = true;
    check(&mut pass, "the EVT_ stream still exists", true); // get succeeded above
    check(
        &mut pass,
        &format!("stream is still R{}", args.replicas),
        num_replicas == args.replicas,
    );
    check(
        &mut pass,
        "a leader is serving the surviving nodes",
        leader.is_some(),
    );
    check(
        &mut pass,
        &format!("all {expect} messages survived ({stored} present)"),
        stored as usize == expect,
    );
    // And they are still consumable end-to-end.
    pass &= consume_phase(js, args, expect).await?;
    Ok(pass)
}
