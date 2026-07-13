//! S4 composed-arm driver. Walks a linear `hops`-node flow by calling the
//! imported `wamn:node` handler once per hop, threading each node's output into
//! the next node's input. `wac plug` binds the handler import to the Rust node
//! (components/node-rs) to produce a single frozen `flow-composed.wasm`.

wit_bindgen::generate!({
    world: "flow-driver",
    path: "wit",
    generate_all,
});

use wamn::node::handler::run as node_run;
use wamn::node::types::{Payload, RunContext};

struct Component;

impl Guest for Component {
    fn run_flow(input: String, mode: String, wait_ns: u64, iters: u64, hops: u32) -> String {
        // Config is identical for every hop; build it once.
        let config = format!("{{\"mode\":\"{mode}\",\"wait_ns\":{wait_ns},\"iters\":{iters}}}");
        let mut cur = Payload::Inline(input);
        for h in 0..hops {
            let ctx = RunContext {
                run_id: "composed".to_string(),
                flow_id: "s4-flow".to_string(),
                flow_version: 1,
                node_id: format!("n{h}"),
                attempt: 0,
                idempotency_key: format!("composed-{h}"),
                traceparent: None,
                tracestate: None,
                deadline_ms: None,
                config: config.clone(),
            };
            cur = match node_run(&ctx, &cur) {
                // Frozen 0.1 (5.4): run returns an emission; the linear bench
                // flow routes only the payload (absent port = "main").
                Ok(e) => e.payload,
                // The bench nodes never error on these inputs; surface it plainly.
                Err(_) => return "{\"error\":\"node failed\"}".to_string(),
            };
        }
        match cur {
            Payload::Inline(s) => s,
            Payload::Streamed(_) => "{\"error\":\"streamed\"}".to_string(),
        }
    }
}

export!(Component);
