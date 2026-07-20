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

    /// The gate table's physical name. UNMAPPED programs (no
    /// `--expect-entity-id`) assert every envelope carries this `table` with
    /// `entity` ABSENT — the wamn-l5i9.11 unmapped marker.
    #[arg(long, default_value = "receipts")]
    pub entity: String,

    /// Expected stable catalog entity id (wamn-l5i9.11): assert EVERY event's
    /// envelope `entity` equals it — across renames, where the physical table
    /// name changes mid-program. Omit for an unmapped-table program.
    #[arg(long)]
    pub expect_entity_id: Option<String>,

    /// Column the expected-id program reads from `new` (the floor's managed
    /// `id` is a random uuid, so catalog-entity drills use their own column).
    #[arg(long, default_value = "id")]
    pub id_field: String,

    /// Drain only this entity SEGMENT's subjects (a filtered consumer) instead
    /// of the whole stream — for asserting one entity's program on a stream
    /// that also carries other tables' events (e.g. the rename drill sharing
    /// the stream with the platform-table noise).
    #[arg(long)]
    pub filter_entity: Option<String>,

    /// If set, assert EVERY delivered envelope carries a causation stamp whose
    /// `run` equals this value (wamn-l5i9.12) — the in-cluster proof that a
    /// transactional `wamn.causation` message stitched through to the stream.
    #[arg(long)]
    pub expect_causation_run: Option<String>,

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
        .unwrap_or_else(|| wamn_event_wire::stream_name(&args.org, &args.env));
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

    // The consumer's subject scope: the whole stream, or (filtered mode) one
    // entity segment's subjects.
    let filter_subject = args
        .filter_entity
        .as_ref()
        .map(|seg| format!("evt.{}.{}.{}.{}.>", args.org, args.project, args.env, seg));

    // Wait for the full program to land (the reader may still be catching up
    // after a drill), then insist the count is EXACT — no strays. In filtered
    // mode the stream count is not the program count; the drain deadline below
    // does the waiting and a final empty fetch proves no strays in-filter.
    let deadline = Instant::now() + Duration::from_secs(args.wait_secs);
    if filter_subject.is_none() {
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
    }

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
            filter_subject: filter_subject.clone().unwrap_or_default(),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow::anyhow!("create pull consumer: {e}"))?;

    let mut delivered: Vec<(String, String, Envelope)> = Vec::new();
    let drain_deadline = Instant::now() + Duration::from_secs(args.wait_secs.max(30));
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
    if let Some(fs) = &filter_subject {
        // Filtered mode: exactness within the filter — one more fetch after
        // the full program must come back empty.
        let mut strays = consumer
            .fetch()
            .max_messages(1)
            .expires(Duration::from_secs(2))
            .messages()
            .await
            .context("stray fetch")?;
        let mut stray = 0;
        while let Some(m) = strays.next().await {
            let _ = m.map_err(|e| anyhow::anyhow!("stray consume: {e}"))?;
            stray += 1;
        }
        check(
            &mut pass,
            &format!("filter {fs} holds EXACTLY the program ({expect}, 0 strays)"),
            delivered.len() == expect && stray == 0,
        );
    } else {
        check(
            &mut pass,
            &format!("stream holds EXACTLY the program ({exact} == {expect})"),
            exact == expect as u64,
        );
    }
    let got_ids: Vec<String> = delivered
        .iter()
        .filter_map(|(_, _, e)| {
            e.new
                .as_ref()
                .and_then(|n| n.get(&args.id_field))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    check(
        &mut pass,
        "delivery order == commit order (the exact id program)",
        got_ids == args.expect_ids,
    );
    match &args.expect_entity_id {
        // The wamn-l5i9.11 rename drill: EVERY envelope carries the stable
        // catalog entity id — even where the physical table name changed
        // mid-program (the tables observed are reported for the log).
        Some(id) => {
            let tables: std::collections::BTreeSet<&str> =
                delivered.iter().map(|(_, _, e)| e.table.as_str()).collect();
            check(
                &mut pass,
                &format!(
                    "every event is an insert with stable entity id {id:?} (tables seen: {tables:?})"
                ),
                delivered
                    .iter()
                    .all(|(_, _, e)| e.op == Op::Insert && e.entity.as_deref() == Some(id)),
            );
        }
        // Unmapped program: `entity` ABSENT (the marker) + the table name.
        None => check(
            &mut pass,
            "every event is an insert on the gate table, entity ABSENT (unmapped marker)",
            delivered.iter().all(|(_, _, e)| {
                e.op == Op::Insert && e.entity.is_none() && e.table == args.entity
            }),
        ),
    }
    check(
        &mut pass,
        "every subject is the v3 grammar (keyed by the entity segment)",
        delivered.iter().all(|(s, _, e)| {
            s == &subject(
                &args.org,
                &args.project,
                &args.env,
                e.entity_segment(),
                e.op,
            )
        }),
    );
    check(
        &mut pass,
        "every Nats-Msg-Id is <project>_<env>:<lsn>",
        delivered
            .iter()
            .all(|(_, id, e)| id == &msg_id(&args.project, &args.env, e.lsn)),
    );
    if let Some(run) = &args.expect_causation_run {
        // wamn-l5i9.12: the reader stitched a transactional wamn.causation
        // message onto every one of the txn's row envelopes.
        check(
            &mut pass,
            &format!("every envelope carries causation run {run:?}"),
            delivered.iter().all(|(_, _, e)| {
                e.causation.as_ref().map(|c| c.run.as_str()) == Some(run.as_str())
            }),
        );
    }
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
