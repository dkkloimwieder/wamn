//! S4 Rust bench node — implements the minimal `wamn:node` handler
//! (docs/wamn-node.wit) with three workload modes, driven entirely by the
//! JSON `config` on the run-context. This is the "composed"/native arm's node
//! (also usable dynamically, as the Rust-dynamic reference column). Its JS twin
//! is `components/samples/node-ts`.
//!
//! Modes (config `{"mode": ..., "wait_ns": N, "iters": N}`):
//!   noop    — return the input unchanged. Used for the HTTP-hop gate so the
//!             measured round trip is (hop + ~0 compute).
//!   io      — wait `wait_ns` via the host `wait-ns` import (a real async host
//!             sleep). Models an I/O-bound node with an identical floor across
//!             guest languages, so the interpreted-vs-composed gap here is pure
//!             framework overhead.
//!   compute — a bounded FNV-1a hashing loop of `iters` rounds over the input.
//!             CPU-bound; this is where the JS-vs-native gap is expected to be
//!             large (design-note: frozen flows' post-GA slot).
//!
//! Every run self-times its serde_json config parse and returns `parse_ns` in
//! the output payload — that is the design-note 9b probe (config-JSON-parse
//! share of cold dispatch), read back by the `nodebench` harness.

wit_bindgen::generate!({
    world: "node-bench",
    path: "wit",
    generate_all,
});

use exports::wamn::node::handler::Guest;
use wamn::nodebench::host::wait_ns;

// The contract types live in their defining interface.
use wamn::node::types::{Emission, ErrorDetail, NodeError, Payload, RunContext};

struct Component;

#[derive(serde::Deserialize)]
struct Config {
    #[serde(default)]
    mode: String,
    /// Nanoseconds to wait in `io` mode.
    #[serde(default)]
    wait_ns: u64,
    /// Hashing rounds in `compute` mode.
    #[serde(default)]
    iters: u64,
}

fn terminal(msg: String) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: msg,
        code: None,
        data: None,
    })
}

fn invalid(msg: String) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: msg,
        code: Some("SCHEMA_MISMATCH".to_string()),
        data: None,
    })
}

/// FNV-1a with per-round feedback so the optimizer cannot fold the loop away.
fn compute(bytes: &[u8], iters: u64) -> u64 {
    let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
    for i in 0..iters {
        acc ^= i;
        for &b in bytes {
            acc = (acc ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        acc = acc.rotate_left(5);
    }
    acc
}

impl Guest for Component {
    fn run(ctx: RunContext, input: Payload) -> Result<Emission, NodeError> {
        let inline = match input {
            Payload::Inline(s) => s,
            Payload::Streamed(_) => {
                return Err(terminal(
                    "streamed payloads out of scope for S4".to_string(),
                ));
            }
        };

        // --- design-note 9b probe: self-time the config JSON parse ---
        let t0 = std::time::Instant::now();
        let cfg: Config =
            serde_json::from_str(&ctx.config).map_err(|e| invalid(format!("bad config: {e}")))?;
        let parse_ns = t0.elapsed().as_nanos() as u64;

        let mut acc: u64 = 0;
        match cfg.mode.as_str() {
            "noop" => {}
            "io" => wait_ns(cfg.wait_ns),
            "compute" => acc = compute(inline.as_bytes(), cfg.iters),
            other => return Err(invalid(format!("unknown mode {other:?}"))),
        }

        // Echo enough that nothing is dead-code-eliminated, and hand the
        // harness the self-timed parse cost.
        let out = format!(
            "{{\"parse_ns\":{parse_ns},\"acc\":{acc},\"n\":{},\"mode\":\"{}\"}}",
            inline.len(),
            cfg.mode
        );
        // Frozen 0.1 (5.4): run returns an emission; absent port = "main".
        Ok(Emission {
            payload: Payload::Inline(out),
            port: None,
        })
    }
}

export!(Component);
