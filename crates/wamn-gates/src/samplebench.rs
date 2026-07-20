//! The `samplebench` subcommand: the E10-e2e (wamn-l5i9.57) component-driven
//! `wamn:jetstream` gate. Where `matbench` drives the materializer (consume →
//! Postgres), this drives the `js-sample` guest — the FIRST `producer` importer
//! — through the WHOLE package: bind a durable pull consumer, fetch, for each
//! event PUBLISH a derived message carrying a deterministic `Nats-Msg-Id`, then
//! ack.
//!
//! The harness shape is matbench's (CommandPre + the REAL `WamnJetstream`
//! plugin), but with NO Postgres: the sample imports only jetstream. Everything
//! rides a throwaway JetStream (two streams on a local/CI NATS — an input stream
//! the guest drains and an output stream the derived messages land on).
//!
//! Phases (each is a fresh guest run to completion — `WAMN_SAMPLE_MAX_EMPTY`
//! bounds it):
//!   1. forward   — publish N input envelopes, run the guest, assert: all N
//!      fetched+acked, N derived messages stored on the output subject with
//!      server acks (and carrying a Nats-Msg-Id), zero duplicates.
//!   2. no-redeliver — rerun the guest WITHOUT touching the durable: phase 1
//!      advanced the ack floor, so this run fetches nothing (the ack floor
//!      really moved — the durable resumes where it left off).
//!   3. dedupe    — delete the input durable server-side and rerun: the whole
//!      input redelivers, the guest re-publishes the SAME Nats-Msg-Ids, and
//!      JetStream dedupes every one — the publishes come back as successful acks
//!      with `duplicate = true`, and the output stream count does NOT move.
//!   4. reject    — run the guest against an output subject NO stream covers:
//!      the server rejects each publish and `publish-rejected` surfaces to the
//!      guest as a `js-error` (the guest reports it, stores nothing).
//!
//! Needs `--nats-url` (JetStream enabled). Recipe: docs/build-and-test.md
//! [E10-E2E].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use futures_util::StreamExt as _;

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_jetstream::{
    self, WAMN_JETSTREAM_ID, WamnJetstream, WamnJetstreamConfig,
};

#[derive(Debug, Args)]
pub struct SampleBenchArgs {
    /// The compiled js-sample component.
    #[arg(long, default_value = "/bench/js-sample.wasm")]
    pub component: PathBuf,

    /// JetStream-enabled NATS (the throwaway input + output streams ride it).
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    pub nats_url: String,

    /// How many input envelopes to publish and drain.
    #[arg(long, default_value_t = 16)]
    pub events: usize,
}

const BENCH_ID: &str = "samplebench";
const IN_STREAM: &str = "WAMN_SAMPLEBENCH_IN";
const OUT_STREAM: &str = "WAMN_SAMPLEBENCH_OUT";
const IN_SUBJECT: &str = "evt.sample.receipts.insert";
const OUT_SUBJECT: &str = "derived.sample.out";
/// Covered by NO stream — the producer error path (`publish-rejected`).
const BAD_SUBJECT: &str = "nostream.sample.out";
const DURABLE: &str = "js_sample_bench";
/// A fresh durable for the reject phase, so it re-reads the input from the top.
const ERR_DURABLE: &str = "js_sample_bench_err";
const MSGID_PREFIX: &str = "sbench";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: CommandPre<SharedCtx>,
    js: Arc<WamnJetstream>,
    report_dir: PathBuf,
}

impl Harness {
    fn plugin_map(
        &self,
    ) -> std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> {
        let mut m: std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> =
            std::collections::HashMap::new();
        m.insert(WAMN_JETSTREAM_ID, self.js.clone());
        m
    }

    /// One guest run to completion under a deadline with a fresh store; returns
    /// the parsed counters report. `durable` + `out_subject` vary per phase.
    async fn run_guest(
        &self,
        durable: &str,
        out_subject: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let report_path = self.report_dir.join("counters.json");
        let _ = std::fs::remove_file(&report_path);

        let mut wasi = WasiCtxBuilder::new();
        wasi.args(&["js-sample.wasm"])
            .inherit_stdout()
            .inherit_stderr()
            .envs(&[
                ("WAMN_SAMPLE_IN_STREAM", IN_STREAM),
                ("WAMN_SAMPLE_DURABLE", durable),
                ("WAMN_SAMPLE_FILTER", IN_SUBJECT),
                ("WAMN_SAMPLE_OUT_SUBJECT", out_subject),
                ("WAMN_SAMPLE_MSGID_PREFIX", MSGID_PREFIX),
                ("WAMN_SAMPLE_BATCH", "64"),
                ("WAMN_SAMPLE_FETCH_MS", "800"),
                ("WAMN_SAMPLE_ACK_WAIT_MS", "30000"),
                ("WAMN_SAMPLE_MAX_EMPTY", "2"),
                ("WAMN_SAMPLE_REPORT_PATH", "/report/counters.json"),
            ])
            .preopened_dir(
                &self.report_dir,
                "/report",
                DirPerms::all(),
                FilePerms::all(),
            )
            .map_err(|e| anyhow::anyhow!("preopen report dir: {e}"))?;

        let ctx = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
            .with_plugins(self.plugin_map())
            .with_wasi_ctx(wasi.build())
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);

        let cmd = self
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| anyhow::anyhow!("instantiate js-sample: {e}"))?;
        let outcome = tokio::time::timeout(
            Duration::from_secs(120),
            cmd.wasi_cli_run().call_run(&mut store),
        )
        .await
        .context("js-sample run deadline (120s) exceeded")?
        .map_err(|e| anyhow::anyhow!("js-sample run trapped: {e}"))?;
        if outcome.is_err() {
            bail!("js-sample exited with error status");
        }

        let raw = std::fs::read_to_string(&report_path)
            .with_context(|| format!("read guest report {}", report_path.display()))?;
        serde_json::from_str(&raw).context("parse guest report")
    }
}

fn counter(report: &serde_json::Value, key: &str) -> i64 {
    report.get(key).and_then(|v| v.as_i64()).unwrap_or(-1)
}

/// Current stored-message count of a stream (fresh handle — `info` needs `&mut`).
async fn stream_messages(js: &async_nats::jetstream::Context, name: &str) -> anyhow::Result<u64> {
    let mut stream = js
        .get_stream(name)
        .await
        .with_context(|| format!("get stream {name}"))?;
    Ok(stream.info().await.context("stream info")?.state.messages)
}

pub async fn run(args: SampleBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates samplebench (l5i9.57 E10-e2e wamn:jetstream sample)");
    let n = args.events;

    let guest = std::fs::read(&args.component)
        .with_context(|| format!("read {}", args.component.display()))?;

    // --- NATS: two throwaway streams (input the guest drains, output the
    // derived messages land on — both with a dedupe window so phase 3 dedupes).
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats.clone());
    for (name, subjects) in [
        (IN_STREAM, "evt.sample.>"),
        (OUT_STREAM, "derived.sample.>"),
    ] {
        let _ = js.delete_stream(name).await;
        js.create_stream(async_nats::jetstream::stream::Config {
            name: name.into(),
            subjects: vec![subjects.into()],
            storage: async_nats::jetstream::stream::StorageType::File,
            num_replicas: 1,
            duplicate_window: Duration::from_secs(120),
            ..Default::default()
        })
        .await
        .with_context(|| format!("create throwaway stream {name}"))?;
    }

    // The input tape: N envelopes on the input subject. Each carries its own
    // Nats-Msg-Id (input-stream dedupe); the guest keys the OUTPUT id off the
    // input stream_seq, not this one.
    for i in 1..=n {
        let mut headers = async_nats::HeaderMap::new();
        headers.append("Nats-Msg-Id", format!("sbench-in:{i}").as_str());
        let body = format!("{{\"id\":{i},\"kind\":\"receipt\"}}");
        js.publish_with_headers(IN_SUBJECT, headers, body.into())
            .await
            .context("input publish send")?
            .await
            .context("input publish ack")?;
    }
    println!("published {n} input envelopes on {IN_SUBJECT} (stream seqs 1..={n})");

    // --- Plugin + engine + guest --------------------------------------------
    // No doorbell, no Postgres: the sample imports only consumer + producer.
    let jsp = Arc::new(WamnJetstream::new(WamnJetstreamConfig {
        nats_url: Some(args.nats_url.clone()),
    }));

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let raw: &RawEngine = engine.inner();
    let component =
        WasmtimeComponent::new(raw, &guest).map_err(|e| anyhow::anyhow!("compile guest: {e}"))?;
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wamn_jetstream::add_to_linker(&mut linker)?;
    let pre = CommandPre::new(linker.instantiate_pre(&component)?)?;

    let report_dir = std::env::temp_dir().join(format!("wamn-samplebench-{}", std::process::id()));
    std::fs::create_dir_all(&report_dir).context("create report dir")?;

    let harness = Harness {
        engine,
        pre,
        js: jsp,
        report_dir: report_dir.clone(),
    };

    let mut pass = true;
    let mut check = |name: &str, ok: bool| {
        println!("PASS({name}): {ok}");
        if !ok {
            pass = false;
        }
    };

    // --- Phase 1: forward -----------------------------------------------------
    let t0 = Instant::now();
    let r1 = harness.run_guest(DURABLE, OUT_SUBJECT).await?;
    println!(
        "phase 1 (forward) guest run: {:?}; report: {r1}",
        t0.elapsed()
    );
    check(
        "all N input events fetched",
        counter(&r1, "fetched") == n as i64,
    );
    check(
        "all N input events acked",
        counter(&r1, "acked") == n as i64,
    );
    check(
        "N derived messages published with server acks",
        counter(&r1, "published") == n as i64,
    );
    check(
        "no duplicates on the first pass",
        counter(&r1, "duplicates") == 0,
    );
    check(
        "no publish rejections on the happy path",
        counter(&r1, "publish-rejected") == 0,
    );
    check(
        "output stream stored exactly N derived messages",
        stream_messages(&js, OUT_STREAM).await? == n as u64,
    );

    // The derived messages really landed on the output subject carrying a
    // Nats-Msg-Id — bind a throwaway consumer and inspect (does not consume from
    // the stream store, so phase 3's count assert is unaffected).
    {
        let out = js.get_stream(OUT_STREAM).await.context("get out stream")?;
        let pull = async_nats::jetstream::consumer::pull::Config {
            durable_name: Some("verify".into()),
            filter_subject: OUT_SUBJECT.into(),
            ..Default::default()
        };
        let consumer = out
            .get_or_create_consumer("verify", pull)
            .await
            .context("bind verify consumer")?;
        let mut batch = consumer
            .fetch()
            .max_messages(n)
            .expires(Duration::from_secs(2))
            .messages()
            .await
            .context("verify fetch")?;
        let mut seen = 0usize;
        let mut all_tagged = true;
        let mut all_on_subject = true;
        while let Some(item) = batch.next().await {
            let msg = item.map_err(|e| anyhow::anyhow!("verify message: {e}"))?;
            seen += 1;
            if msg.subject.as_str() != OUT_SUBJECT {
                all_on_subject = false;
            }
            let tagged = msg
                .headers
                .as_ref()
                .and_then(|h| h.get("Nats-Msg-Id"))
                .is_some_and(|v| v.as_str().starts_with(MSGID_PREFIX));
            if !tagged {
                all_tagged = false;
            }
        }
        check("verify consumer saw all N derived messages", seen == n);
        check(
            "every derived message is on the output subject",
            all_on_subject,
        );
        check("every derived message carries a Nats-Msg-Id", all_tagged);
    }

    // --- Phase 2: no-redeliver (the ack floor advanced) ----------------------
    let r2 = harness.run_guest(DURABLE, OUT_SUBJECT).await?;
    println!("phase 2 (no-redeliver) report: {r2}");
    check(
        "rebind fetches nothing — the ack floor advanced past the whole tape",
        counter(&r2, "fetched") == 0,
    );
    check(
        "nothing re-published on rebind",
        counter(&r2, "published") == 0,
    );

    // --- Phase 3: dedupe (delete the durable → full redelivery) --------------
    let in_stream = js.get_stream(IN_STREAM).await.context("get in stream")?;
    in_stream
        .delete_consumer(DURABLE)
        .await
        .with_context(|| format!("delete durable {DURABLE} (must exist after phase 1)"))?;
    let out_before = stream_messages(&js, OUT_STREAM).await?;
    let r3 = harness.run_guest(DURABLE, OUT_SUBJECT).await?;
    println!("phase 3 (dedupe) report: {r3}");
    let out_after = stream_messages(&js, OUT_STREAM).await?;
    check(
        "the whole input redelivered after the durable was deleted",
        counter(&r3, "fetched") == n as i64,
    );
    check(
        "every re-publish deduped (successful acks with duplicate = true)",
        counter(&r3, "published") == n as i64 && counter(&r3, "duplicates") == n as i64,
    );
    check(
        "output stream count did NOT move — dedupe stored nothing new",
        out_after == out_before && out_after == n as u64,
    );

    // --- Phase 4: reject (producer error path surfaces as js-error) ----------
    // A fresh durable re-reads the input from the top; publishing to a subject
    // no stream covers rejects every publish.
    let r4 = harness.run_guest(ERR_DURABLE, BAD_SUBJECT).await?;
    println!("phase 4 (reject) report: {r4}");
    check(
        "the error-path guest still fetched the whole input",
        counter(&r4, "fetched") == n as i64,
    );
    check(
        "publish to an uncovered subject surfaces as publish-rejected",
        counter(&r4, "publish-rejected") == n as i64,
    );
    check(
        "no derived messages stored on the reject path",
        counter(&r4, "published") == 0,
    );

    // --- Teardown -------------------------------------------------------------
    let _ = js.delete_stream(IN_STREAM).await;
    let _ = js.delete_stream(OUT_STREAM).await;
    let _ = std::fs::remove_dir_all(&report_dir);
    ticker.abort();

    println!("\nsamplebench complete — overall PASS: {pass}");
    if !pass {
        bail!("l5i9.57 samplebench gate failed");
    }
    Ok(())
}
