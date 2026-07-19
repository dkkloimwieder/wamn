//! `readerbench` — the stream-side assert step of the event-reader gate of
//! record (wamn-l5i9.10, D19 v3 §4).
//!
//! The in-cluster gate script provisions CDC, runs the REAL `wamn-host
//! event-reader` process against wamn-pg + evt-nats, drives psql writes and
//! the kill/sever drills — then calls this mode to prove what LANDED: exactly
//! the expected insert ids, in commit order, one message each (`Nats-Msg-Id`
//! dedupe held across restarts/redelivery), every envelope well-formed per
//! the wamn-event-wire draft. The local drills live in
//! `crates/wamn-host/tests/event_reader_live.rs`; this mode only asserts the
//! stream, so it stays reusable for the C-CDC (l5i9.14) and materializer
//! (l5i9.17) gates.

use anyhow::{Context as _, bail};
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use clap::Args;
use futures_util::StreamExt as _;
use std::time::{Duration, Instant};

use wamn_event_wire::{Envelope, Op, msg_id, subject};

#[derive(Debug, Args)]
pub struct ReaderBenchArgs {
    /// Data-plane NATS (JetStream) URL.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Org slug (subject root; default stream `EVT_<org>_<env>`).
    #[arg(long)]
    pub org: String,

    /// Project slug.
    #[arg(long)]
    pub project: String,

    /// Env slug.
    #[arg(long)]
    pub env: String,

    /// Stream to drain. Default: `EVT_<org>_<env>` (the registration default).
    #[arg(long)]
    pub stream: Option<String>,

    /// Entity every expected event carries (the gate table's name).
    #[arg(long, default_value = "receipts")]
    pub entity: String,

    /// The expected INSERT ids, comma-separated, in COMMIT ORDER — the whole
    /// write program of the gate run.
    #[arg(long, value_delimiter = ',')]
    pub expect_ids: Vec<String>,

    /// Seconds to wait for the stream to hold the full program.
    #[arg(long, default_value_t = 60)]
    pub wait_secs: u64,

    /// Delete the stream after the asserts pass — the gate's zero-residue
    /// teardown (the standing evt-nats keeps no gate streams behind).
    #[arg(long, default_value_t = false)]
    pub delete_stream: bool,
}

fn check(pass: &mut bool, name: &str, ok: bool) {
    println!("  [{}] {name}", if ok { "PASS" } else { "FAIL" });
    *pass &= ok;
}

pub async fn run(args: ReaderBenchArgs) -> anyhow::Result<()> {
    let stream_name = args
        .stream
        .clone()
        .unwrap_or_else(|| format!("EVT_{}_{}", args.org, args.env));
    let expect = args.expect_ids.len();
    println!(
        "readerbench — draining {stream_name} at {} (expect {expect} inserts on {})",
        args.nats_url, args.entity
    );
    if expect == 0 {
        bail!("--expect-ids is empty — nothing to assert");
    }

    let client = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect data-plane NATS at {}", args.nats_url))?;
    let js = jetstream::new(client);

    // Wait for the full program to land (the reader may still be catching up
    // after a drill), then insist the count is EXACT — no strays.
    let deadline = Instant::now() + Duration::from_secs(args.wait_secs);
    loop {
        let mut stream = js.get_stream(&stream_name).await.context("get stream")?;
        let have = stream.info().await.context("stream info")?.state.messages;
        if have >= expect as u64 {
            break;
        }
        if Instant::now() > deadline {
            bail!(
                "stream {stream_name} holds {have}/{expect} after {}s",
                args.wait_secs
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let stream = js.get_stream(&stream_name).await.context("get stream")?;
    let exact = stream
        .get_info()
        .await
        .context("stream info")?
        .state
        .messages;
    let consumer = stream
        .create_consumer(PullConfig {
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::Explicit,
            num_replicas: 1,
            memory_storage: true,
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow::anyhow!("create pull consumer: {e}"))?;

    let mut delivered: Vec<(String, String, Envelope)> = Vec::new();
    let drain_deadline = Instant::now() + Duration::from_secs(30);
    while delivered.len() < expect && Instant::now() < drain_deadline {
        let mut batch = consumer
            .fetch()
            .max_messages(expect - delivered.len())
            .messages()
            .await
            .context("fetch batch")?;
        let mut drained_any = false;
        while let Some(m) = batch.next().await {
            let m = m.map_err(|e| anyhow::anyhow!("consume message: {e}"))?;
            drained_any = true;
            let id = m
                .headers
                .as_ref()
                .and_then(|h| h.get(NATS_MESSAGE_ID))
                .map(|v| v.to_string())
                .unwrap_or_default();
            let envelope: Envelope = serde_json::from_slice(&m.payload)
                .map_err(|e| anyhow::anyhow!("envelope does not deserialize: {e}"))?;
            delivered.push((m.subject.to_string(), id, envelope));
            m.ack().await.map_err(|e| anyhow::anyhow!("ack: {e}"))?;
        }
        if !drained_any {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    let mut pass = true;
    check(
        &mut pass,
        &format!("stream holds EXACTLY the program ({exact} == {expect})"),
        exact == expect as u64,
    );
    let got_ids: Vec<String> = delivered
        .iter()
        .filter_map(|(_, _, e)| {
            e.new
                .as_ref()
                .and_then(|n| n.get("id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    check(
        &mut pass,
        "delivery order == commit order (the exact id program)",
        got_ids == args.expect_ids,
    );
    check(
        &mut pass,
        "every event is an insert on the gate entity",
        delivered
            .iter()
            .all(|(_, _, e)| e.op == Op::Insert && e.entity == args.entity),
    );
    check(
        &mut pass,
        "every subject is the v3 grammar",
        delivered
            .iter()
            .all(|(s, _, e)| s == &subject(&args.org, &args.project, &args.env, &e.entity, e.op)),
    );
    check(
        &mut pass,
        "every Nats-Msg-Id is <project>_<env>:<lsn>",
        delivered
            .iter()
            .all(|(_, id, e)| id == &msg_id(&args.project, &args.env, e.lsn)),
    );
    let unique: std::collections::BTreeSet<&String> =
        delivered.iter().map(|(_, id, _)| id).collect();
    check(
        &mut pass,
        "Nats-Msg-Ids unique (dedupe held across drills)",
        unique.len() == delivered.len(),
    );
    check(
        &mut pass,
        "commit_ts + txid stamped on every envelope",
        delivered
            .iter()
            .all(|(_, _, e)| e.txid > 0 && e.commit_ts.timestamp() > 0),
    );

    println!("\nreaderbench complete — overall PASS: {pass}");
    if !pass {
        bail!("a readerbench assert failed");
    }
    if args.delete_stream {
        js.delete_stream(&stream_name)
            .await
            .map_err(|e| anyhow::anyhow!("delete stream {stream_name}: {e}"))?;
        println!("stream {stream_name} deleted (zero residue)");
    }
    Ok(())
}
