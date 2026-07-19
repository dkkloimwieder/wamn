//! S5 logging-capture bench guest (`components/fixtures/logspewer`).
//!
//! Imports `wasi:logging/logging` and exports two entry points the `logbench`
//! harness drives (docs/archive/p0-exit-criteria.md S5):
//!
//!   * `overhead(n)` self-times each `log()` call with the guest's own
//!     `std::time::Instant` (which works on wasm32-wasip2 — proven in S3/S4) and
//!     returns the per-call latencies in nanoseconds. The <50 µs gate is a
//!     *guest-observed* cost, so the guest must hold the stopwatch. Because the
//!     host plugin's `log()` only enriches + enqueues (non-blocking), this
//!     measures the boundary + enrichment + enqueue, not the OTLP export.
//!   * `emit-batch(count, seq_base, run_label, flow, run, node)` emits `count`
//!     info logs whose `context` is a small JSON object
//!     `{flow,run,node,seq,run_label}`. The plugin parses flow/run/node from it
//!     (enrichment) and the harness uses `seq`/`run_label` to count exactly what
//!     reached Loki. `tenant`/`project` are NOT here — they are host-injected.
//!
//! The context JSON is hand-formatted (no serde) to keep `log()` cheap in the
//! overhead path; the bench identifiers never contain JSON metacharacters.

wit_bindgen::generate!({
    world: "log-bench",
    path: "wit",
    generate_all,
});

use wasi::logging::logging::{Level, log};

struct Component;

/// `{"flow":"..","run":"..","node":"..","seq":N,"run_label":".."}`
fn context(flow: &str, run: &str, node: &str, seq: u64, run_label: &str) -> String {
    format!(
        "{{\"flow\":\"{flow}\",\"run\":\"{run}\",\"node\":\"{node}\",\"seq\":{seq},\"run_label\":\"{run_label}\"}}"
    )
}

impl Guest for Component {
    fn overhead(n: u32) -> Vec<u64> {
        let ctx = context("f-overhead", "r-overhead", "n0", 0, "overhead");
        let mut samples = Vec::with_capacity(n as usize);
        for i in 0..n {
            let msg = format!("overhead probe line {i}");
            let t0 = std::time::Instant::now();
            log(Level::Info, &ctx, &msg);
            samples.push(t0.elapsed().as_nanos() as u64);
        }
        samples
    }

    fn emit_batch(count: u32, seq_base: u64, run_label: String, flow: String, run: String, node: String) {
        for i in 0..count as u64 {
            let seq = seq_base + i;
            let ctx = context(&flow, &run, &node, seq, &run_label);
            let msg = format!("line {seq}");
            log(Level::Info, &ctx, &msg);
        }
    }
}

export!(Component);
