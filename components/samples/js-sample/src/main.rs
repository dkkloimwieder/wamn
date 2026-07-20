//! `wamn:jetstream` consumer + producer SAMPLE (E10-e2e, wamn-l5i9.57) — the
//! adopter template a new importer copies. It is deliberately the SMALLEST
//! thing that drives BOTH sides of the frozen `wamn:jetstream@0.1.0` package:
//!
//!   bind a durable pull consumer  →  fetch a bounded batch  →  for each event
//!   PUBLISH a derived message (carrying a deterministic `Nats-Msg-Id`)  →  ack.
//!
//! WHY IT EXISTS. The materializer (l5i9.17) was the first `wamn:jetstream`
//! importer, but it only CONSUMES (types + consumer + doorbell) and writes runs
//! to Postgres — nothing had ever imported `producer`. This sample is the first
//! `producer` importer, so it is also where the publish side (server-ack wait,
//! `Nats-Msg-Id` dedupe participation, the `publish-rejected` error surface)
//! gets exercised component-to-host end-to-end.
//!
//! DEDUPE PARTICIPATION. The derived `Nats-Msg-Id` is minted DETERMINISTICALLY
//! from the input event's `stream-seq` (`<prefix>:<seq>`). Re-draining the same
//! input (e.g. after the durable consumer is deleted and rebound) re-publishes
//! the SAME ids, so JetStream dedupes them inside the output stream's duplicate
//! window: the publish still returns a successful `publish-ack`, but with
//! `duplicate = true` and the message is NOT re-stored. A duplicate is a
//! SUCCESS, never an error — the exactly-once forwarding guarantee a real
//! event-bridge relies on.
//!
//! DISPOSITION ORDER. Publish-THEN-ack: the derived message must be durably
//! stored before the input is consumed (at-least-once forwarding). On a
//! persistent publish rejection (a misconfigured output subject no stream
//! covers) the sample TERMINATES the input so it stops redelivering and the
//! process can exit — the materializer's poison→term idiom. A production
//! forwarder would instead NACK a *transient* failure to retry and alert on a
//! persistent one; the sample keeps it simple so the gate always terminates.
//!
//! Config is host/deploy-injected via `wasi:cli` env (`WAMN_SAMPLE_*`); the NATS
//! connection itself is the host plugin's, never named by the guest.

wit_bindgen::generate!({
    world: "sample",
    path: "wit",
    generate_all,
});

use wamn::jetstream::consumer::{self, ConsumerConfig};
use wamn::jetstream::producer;
use wamn::jetstream::types::{Header, JsError};

// ---------------------------------------------------------------------------
// Config (wasi:cli env — deploy sets these on the workload spec / the gate sets
// them on the WasiCtx). Only what the guest must know to SUBSCRIBE + PUBLISH;
// the connection is host-injected.
// ---------------------------------------------------------------------------

struct Config {
    /// The stream to bind the durable pull consumer against (provisioned
    /// out-of-band; a guest binds by name, never creates).
    in_stream: String,
    /// Durable consumer name — persists server-side; the ack floor + redelivery
    /// track against it, so rebinding the same name resumes where it left off.
    durable: String,
    /// Server-side subject filter (empty = the whole stream).
    filter: String,
    /// Subject the derived message is published to (must be covered by some
    /// stream, or the publish is rejected — the error-path the gate asserts).
    out_subject: String,
    /// `Nats-Msg-Id` prefix; the id is `<prefix>:<input-stream-seq>`, so a
    /// redelivered input re-publishes an identical id and dedupes.
    msgid_prefix: String,
    /// Fetch batch bound per pull.
    batch: u32,
    /// Long-poll window per fetch, ms (how long the server waits for at least
    /// one message before returning an empty batch).
    fetch_ms: u64,
    /// Server ack-wait for the durable consumer, ms.
    ack_wait_ms: u64,
    /// Exit after this many CONSECUTIVE empty fetches (the drain-complete
    /// signal — the input is bounded, so empties mean "nothing left").
    max_empty: u32,
    /// Optional counters report path (a preopened dir in the gate; a mounted
    /// volume in cluster).
    report_path: Option<String>,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("missing required env {name}"))
}

impl Config {
    fn from_env() -> Result<Config, String> {
        Ok(Config {
            in_stream: required("WAMN_SAMPLE_IN_STREAM")?,
            durable: required("WAMN_SAMPLE_DURABLE")?,
            filter: env_or("WAMN_SAMPLE_FILTER", ""),
            out_subject: required("WAMN_SAMPLE_OUT_SUBJECT")?,
            msgid_prefix: env_or("WAMN_SAMPLE_MSGID_PREFIX", "sample"),
            batch: env_or("WAMN_SAMPLE_BATCH", "64")
                .parse()
                .map_err(|e| format!("WAMN_SAMPLE_BATCH: {e}"))?,
            fetch_ms: env_or("WAMN_SAMPLE_FETCH_MS", "2000")
                .parse()
                .map_err(|e| format!("WAMN_SAMPLE_FETCH_MS: {e}"))?,
            ack_wait_ms: env_or("WAMN_SAMPLE_ACK_WAIT_MS", "30000")
                .parse()
                .map_err(|e| format!("WAMN_SAMPLE_ACK_WAIT_MS: {e}"))?,
            max_empty: env_or("WAMN_SAMPLE_MAX_EMPTY", "2")
                .parse()
                .map_err(|e| format!("WAMN_SAMPLE_MAX_EMPTY: {e}"))?,
            report_path: std::env::var("WAMN_SAMPLE_REPORT_PATH").ok(),
        })
    }
}

// ---------------------------------------------------------------------------
// Counters — the gate's report AND a real adopter's minimal observability.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counters {
    /// Input messages pulled off the durable consumer.
    fetched: u64,
    /// Input messages positively acknowledged (ack floor advances).
    acked: u64,
    /// Derived messages the server confirmed stored OR deduped (both are
    /// successful publish-acks).
    published: u64,
    /// Of `published`, the ones JetStream deduped (`publish-ack.duplicate`) —
    /// proof the `Nats-Msg-Id` dedupe fired.
    duplicates: u64,
    /// Publishes rejected because no stream covers the output subject (the
    /// `js-error::publish-rejected` surface); the input is terminated.
    publish_rejected: u64,
    /// Any other publish error (e.g. connection-unavailable); also terminates.
    publish_error: u64,
    /// Consecutive empty fetches at exit (drain-complete evidence).
    empty_fetches: u64,
}

impl Counters {
    fn to_json(&self) -> String {
        format!(
            "{{\"fetched\":{},\"acked\":{},\"published\":{},\"duplicates\":{},\
             \"publish-rejected\":{},\"publish-error\":{},\"empty-fetches\":{}}}",
            self.fetched,
            self.acked,
            self.published,
            self.duplicates,
            self.publish_rejected,
            self.publish_error,
            self.empty_fetches,
        )
    }
}

// ---------------------------------------------------------------------------
// The derived message
// ---------------------------------------------------------------------------

/// A tiny JSON body derived from the input event. Deterministic in the input
/// `stream-seq` so the (seq-keyed) `Nats-Msg-Id` and the payload agree across a
/// redelivery. A real bridge would transform the CDC envelope here.
fn derived_body(seq: u64, subject: &str, input_len: usize) -> String {
    format!(
        "{{\"source\":\"wamn:js-sample\",\"input-seq\":{seq},\
         \"input-subject\":\"{subject}\",\"input-bytes\":{input_len}}}"
    )
}

/// The publish disposition for one fetched message. Returns whether the caller
/// should keep draining (`true`) — a publish rejection is persistent (a bad
/// output subject), so it terminates the input rather than nack-looping.
fn forward(cfg: &Config, msg: &consumer::Message, counters: &mut Counters) {
    counters.fetched += 1;
    let meta = msg.metadata();
    let subject = msg.subject();
    let body = msg.body();

    // The dedupe key: deterministic in the input stream-seq, so a redelivered
    // input re-publishes an identical id and JetStream dedupes it.
    let msg_id = format!("{}:{}", cfg.msgid_prefix, meta.stream_seq);
    let headers = vec![Header {
        name: "Nats-Msg-Id".to_string(),
        value: msg_id,
    }];
    let payload = derived_body(meta.stream_seq, &subject, body.len());

    // Publish waits for the SERVER ACK — the only delivery truth. A deduped
    // publish comes back Ok with duplicate = true (a success, not an error).
    match producer::publish(&cfg.out_subject, &headers, payload.as_bytes()) {
        Ok(ack) => {
            counters.published += 1;
            if ack.duplicate {
                counters.duplicates += 1;
            }
            // Publish-then-ack: the derived message is durable before the input
            // is consumed (at-least-once forwarding).
            if let Err(e) = msg.ack() {
                // A failed ack just means a redelivery (the deterministic
                // Nats-Msg-Id makes the re-publish a dedupe, not a double-store).
                eprintln!(
                    "wamn::js-sample ack failed for stream_seq={}: {e:?} — will redeliver (dedupe absorbs the re-publish)",
                    meta.stream_seq
                );
            } else {
                counters.acked += 1;
            }
        }
        Err(JsError::PublishRejected(why)) => {
            // No stream covers the output subject (a misconfiguration) — a
            // persistent failure. Terminate so the input stops redelivering and
            // the process can exit; a production forwarder would alert here.
            counters.publish_rejected += 1;
            eprintln!(
                "wamn::js-sample publish REJECTED for stream_seq={} to {:?}: {why} — terminating input (no stream covers the subject)",
                meta.stream_seq, cfg.out_subject
            );
            let _ = msg.term();
        }
        Err(e) => {
            counters.publish_error += 1;
            eprintln!(
                "wamn::js-sample publish error for stream_seq={}: {e:?} — terminating input",
                meta.stream_seq
            );
            let _ = msg.term();
        }
    }
}

fn write_report(cfg: &Config, counters: &Counters) {
    if let Some(path) = &cfg.report_path
        && let Err(e) = std::fs::write(path, counters.to_json())
    {
        eprintln!("wamn::js-sample report write failed ({path}): {e}");
    }
}

fn main() {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wamn::js-sample config error: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "wamn::js-sample up: in_stream={} durable={} filter={:?} out_subject={} msgid_prefix={} batch={} max_empty={}",
        cfg.in_stream,
        cfg.durable,
        cfg.filter,
        cfg.out_subject,
        cfg.msgid_prefix,
        cfg.batch,
        cfg.max_empty
    );

    // Bind (or rebind) the durable pull consumer over the existing stream.
    // Idempotent on the durable name: a rebind resumes from the ack floor.
    let bound = match consumer::bind(&ConsumerConfig {
        stream_name: cfg.in_stream.clone(),
        durable: cfg.durable.clone(),
        filter_subject: cfg.filter.clone(),
        ack_wait_ms: cfg.ack_wait_ms,
        max_deliver: 0,
    }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "wamn::js-sample bind failed (stream {}): {e:?}",
                cfg.in_stream
            );
            std::process::exit(1);
        }
    };

    let mut counters = Counters::default();
    let mut consecutive_empty = 0u32;
    // Drain loop: pull a bounded batch, forward each, and stop once the input is
    // exhausted (max_empty consecutive empty fetches).
    loop {
        match bound.fetch(cfg.batch, cfg.fetch_ms) {
            Ok(msgs) if msgs.is_empty() => {
                consecutive_empty += 1;
                counters.empty_fetches = consecutive_empty as u64;
                if consecutive_empty >= cfg.max_empty {
                    break;
                }
            }
            Ok(msgs) => {
                consecutive_empty = 0;
                for msg in &msgs {
                    forward(&cfg, msg, &mut counters);
                }
            }
            Err(e) => {
                // Transient (connection-unavailable) — retry the fetch. A tight
                // spin is fine for the sample; a real service would back off.
                eprintln!("wamn::js-sample fetch failed: {e:?} — retrying");
                consecutive_empty += 1;
                if consecutive_empty >= cfg.max_empty {
                    break;
                }
            }
        }
        write_report(&cfg, &counters);
    }

    write_report(&cfg, &counters);
    println!("wamn::js-sample done: {}", counters.to_json());
}
