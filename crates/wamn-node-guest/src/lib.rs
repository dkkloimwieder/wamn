//! # wamn-node-guest — custom-node componentization scaffolding (5.4)
//!
//! Makes the 5.13 "componentization is mechanical" promise literal: a custom
//! node implements the SAME [`wamn_node_sdk::Node`] trait the standard library
//! (5.3) uses, and one macro turns it into a `wamn:node/handler` export over
//! the FROZEN 0.1 contract (`docs/wamn-node.wit`):
//!
//! ```ignore
//! #[derive(Default)]
//! struct MyNode;
//! impl wamn_node_sdk::Node for MyNode { /* pure logic */ }
//! wamn_node_guest::export_node!(MyNode);
//! ```
//!
//! The bindgen target is the contract's minimal `world node` — export the
//! handler, import NOTHING — so a node built here is physically incapable of
//! I/O (design-note 7; the [`NoCapsCtx`] facade refuses every capability call
//! with `NotGranted`). Capability-bearing custom-node worlds wait for the
//! optional imports' host implementations (payloads 5.10, credentials 5.9,
//! control 5.12).
//!
//! Every WIT<->SDK conversion this crate performs is a pure function,
//! unit-tested here and gate-tested end-to-end through real wasm by the
//! `nodebench` sample mode (`components/sample-node`).

use serde_json::Value;
use wamn_node_sdk::{Emission, ErrorDetail, HttpCapError, HttpRequest, HttpResponse};

pub mod bindings {
    wit_bindgen::generate!({
        world: "node",
        path: "wit",
        generate_all,
        pub_export_macro: true,
        export_macro_name: "export_node_bindings",
    });
}

/// The WIT-shaped contract types (from the generated bindings) in one flat
/// namespace, plus the export `Guest` trait.
pub mod wit {
    pub use crate::bindings::exports::wamn::node::handler::Guest;
    pub use crate::bindings::wamn::node::types::{
        Emission, ErrorDetail, Framing, NodeError, Payload, PayloadRef, RateLimitDetail, RunContext,
    };
}
use wamn_node_sdk::{Node, NodeCtx, NodeError, PgCapError, PgRows, PgValue, RunContext};

// ---------------------------------------------------------------------------
// The zero-capability facade: `world node` imports nothing, so every
// capability call is NotGranted by construction.
// ---------------------------------------------------------------------------

/// The capability facade matching `world node`'s empty import set: every call
/// fails `NotGranted`. A node that declares capabilities cannot be exported
/// through [`export_node!`] and actually run — its grant check refuses first.
pub struct NoCapsCtx;

impl NodeCtx for NoCapsCtx {
    fn http(&mut self, _req: &HttpRequest) -> Result<HttpResponse, HttpCapError> {
        Err(HttpCapError::NotGranted)
    }
    fn pg_query(&mut self, _sql: &str, _params: &[PgValue]) -> Result<PgRows, PgCapError> {
        Err(PgCapError::NotGranted)
    }
    fn pg_execute(&mut self, _sql: &str, _params: &[PgValue]) -> Result<u64, PgCapError> {
        Err(PgCapError::NotGranted)
    }
    fn catalog_json(&mut self) -> Result<String, PgCapError> {
        Err(PgCapError::NotGranted)
    }
}

// ---------------------------------------------------------------------------
// WIT <-> SDK conversions (pure; the mutation-tested surface)
// ---------------------------------------------------------------------------

/// SDK error detail -> WIT.
fn detail_to_wit(d: ErrorDetail) -> wit::ErrorDetail {
    wit::ErrorDetail {
        message: d.message,
        code: d.code,
        data: d.data.map(|v| v.to_string()),
    }
}

/// SDK taxonomy -> WIT `node-error`, variant for variant. The engine folds
/// retry-vs-error-path-vs-fail mechanically from this — a swapped arm here
/// would silently change run semantics, so it is pinned by unit test AND the
/// nodebench sample gate.
pub fn error_to_wit(e: NodeError) -> wit::NodeError {
    match e {
        NodeError::Retryable(d) => wit::NodeError::Retryable(detail_to_wit(d)),
        NodeError::RateLimited(r) => wit::NodeError::RateLimited(wit::RateLimitDetail {
            detail: detail_to_wit(r.detail),
            retry_after_ms: r.retry_after_ms,
            target_host: r.target_host,
        }),
        NodeError::Terminal(d) => wit::NodeError::Terminal(detail_to_wit(d)),
        NodeError::InvalidInput(d) => wit::NodeError::InvalidInput(detail_to_wit(d)),
        NodeError::Cancelled => wit::NodeError::Cancelled,
    }
}

/// SDK emission -> WIT: the payload re-serializes to the `json` string form,
/// and the default `main` port travels as ABSENT (the frozen contract's
/// canonical spelling).
pub fn emission_to_wit(e: Emission) -> wit::Emission {
    wit::Emission {
        payload: wit::Payload::Inline(e.payload.to_string()),
        port: (e.port != wamn_node_sdk::MAIN_PORT).then_some(e.port),
    }
}

fn terminal(code: &str, message: impl Into<String>) -> wit::NodeError {
    wit::NodeError::Terminal(wit::ErrorDetail {
        message: message.into(),
        code: Some(code.to_string()),
        data: None,
    })
}

/// WIT payload -> SDK JSON value. Streamed payloads wait for the payload
/// store (5.10): until a host implements the `payloads` import, an SDK node
/// cannot read one, so the scaffolding refuses it up front.
pub fn payload_to_value(p: &wit::Payload) -> Result<Value, wit::NodeError> {
    match p {
        wit::Payload::Inline(s) => serde_json::from_str(s).map_err(|e| {
            wit::NodeError::InvalidInput(wit::ErrorDetail {
                message: format!("input payload is not valid JSON: {e}"),
                code: Some("INVALID_JSON".to_string()),
                data: None,
            })
        }),
        wit::Payload::Streamed(_) => Err(terminal(
            "streamed-payload-unsupported",
            "streamed payloads land with the payload store (5.10)",
        )),
    }
}

/// Drive an SDK node once over the WIT-shaped arguments. This is the whole
/// export glue as a pure, host-testable function; [`NodeComponent`] is just
/// this behind the generated `Guest` trait.
pub fn run_node<N: Node>(
    node: &N,
    caps: &mut dyn NodeCtx,
    ctx: &wit::RunContext,
    input: &wit::Payload,
) -> Result<wit::Emission, wit::NodeError> {
    let input = payload_to_value(input)?;
    // The contract validates config against the node's manifest schema before
    // dispatch; unparseable config reaching a node is a runner bug, not input.
    let config: Value = serde_json::from_str(&ctx.config)
        .map_err(|e| terminal("invalid-config", format!("config is not valid JSON: {e}")))?;
    let run = RunContext {
        run_id: &ctx.run_id,
        flow_id: &ctx.flow_id,
        flow_version: ctx.flow_version,
        node_id: &ctx.node_id,
        attempt: ctx.attempt,
        idempotency_key: &ctx.idempotency_key,
        deadline_ms: ctx.deadline_ms,
        traceparent: ctx.traceparent.as_deref(),
        tracestate: ctx.tracestate.as_deref(),
        config: &config,
    };
    node.run(caps, &run, &input)
        .map(emission_to_wit)
        .map_err(error_to_wit)
}

/// The exported component shell over any `Node + Default` implementation.
/// [`export_node!`] instantiates it; `world node` grants no capabilities, so
/// the facade is [`NoCapsCtx`].
pub struct NodeComponent<N>(core::marker::PhantomData<N>);

impl<N: Node + Default + 'static> wit::Guest for NodeComponent<N> {
    fn run(ctx: wit::RunContext, input: wit::Payload) -> Result<wit::Emission, wit::NodeError> {
        run_node(&N::default(), &mut NoCapsCtx, &ctx, &input)
    }
}

/// Export `$node` (a `wamn_node_sdk::Node + Default` type) as this
/// component's `wamn:node/handler`.
#[macro_export]
macro_rules! export_node {
    ($node:ty) => {
        // The generated export macro takes a bare identifier; alias the
        // generic shell to one.
        #[doc(hidden)]
        type __WamnNodeExport = $crate::NodeComponent<$node>;
        $crate::bindings::export_node_bindings!(
            __WamnNodeExport with_types_in $crate::bindings
        );
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_node_sdk::{NodeError, RateLimitDetail};

    struct Probe;
    impl Node for Probe {
        fn run(
            &self,
            _ctx: &mut dyn NodeCtx,
            run: &RunContext<'_>,
            input: &Value,
        ) -> Result<Emission, NodeError> {
            match run.config.get("port").and_then(Value::as_str) {
                Some(p) => Ok(Emission::on(input.clone(), p)),
                None => Ok(Emission::main(input.clone())),
            }
        }
    }

    fn wit_ctx(config: &str) -> wit::RunContext {
        wit::RunContext {
            run_id: "r".into(),
            flow_id: "f".into(),
            flow_version: 1,
            node_id: "n".into(),
            attempt: 0,
            idempotency_key: "r:n".into(),
            traceparent: None,
            tracestate: None,
            deadline_ms: None,
            config: config.into(),
        }
    }

    #[test]
    fn taxonomy_maps_variant_for_variant() {
        let d = || ErrorDetail::coded("C", "m");
        assert!(matches!(
            error_to_wit(NodeError::Retryable(d())),
            wit::NodeError::Retryable(w) if w.code.as_deref() == Some("C")
        ));
        assert!(matches!(
            error_to_wit(NodeError::Terminal(d())),
            wit::NodeError::Terminal(_)
        ));
        assert!(matches!(
            error_to_wit(NodeError::InvalidInput(d())),
            wit::NodeError::InvalidInput(_)
        ));
        assert!(matches!(
            error_to_wit(NodeError::Cancelled),
            wit::NodeError::Cancelled
        ));
        match error_to_wit(NodeError::RateLimited(RateLimitDetail {
            detail: d(),
            retry_after_ms: Some(1500),
            target_host: Some("api.example".into()),
        })) {
            wit::NodeError::RateLimited(r) => {
                assert_eq!(r.retry_after_ms, Some(1500));
                assert_eq!(r.target_host.as_deref(), Some("api.example"));
            }
            other => panic!("expected rate-limited, got {other:?}"),
        }
    }

    #[test]
    fn main_port_travels_absent_and_named_ports_travel_present() {
        let m = emission_to_wit(Emission::main(serde_json::json!({"a": 1})));
        assert_eq!(m.port, None);
        let b = emission_to_wit(Emission::on(Value::Null, "true"));
        assert_eq!(b.port.as_deref(), Some("true"));
    }

    #[test]
    fn streamed_input_is_refused_until_the_payload_store_lands() {
        let p = wit::Payload::Streamed(wit::PayloadRef {
            handle: "h".into(),
            framing: wit::Framing::Ndjson,
            size_hint: None,
        });
        match payload_to_value(&p) {
            Err(wit::NodeError::Terminal(d)) => {
                assert_eq!(d.code.as_deref(), Some("streamed-payload-unsupported"));
            }
            other => panic!("expected terminal, got {other:?}"),
        }
    }

    #[test]
    fn run_node_round_trips_ctx_payload_and_port() {
        let out = run_node(
            &Probe,
            &mut NoCapsCtx,
            &wit_ctx("{}"),
            &wit::Payload::Inline("{\"x\": 7}".into()),
        )
        .expect("probe succeeds");
        assert_eq!(out.port, None);
        match out.payload {
            wit::Payload::Inline(s) => {
                assert_eq!(serde_json::from_str::<Value>(&s).unwrap()["x"], 7)
            }
            wit::Payload::Streamed(_) => panic!("inline expected"),
        }
        let branched = run_node(
            &Probe,
            &mut NoCapsCtx,
            &wit_ctx("{\"port\": \"true\"}"),
            &wit::Payload::Inline("null".into()),
        )
        .expect("probe succeeds");
        assert_eq!(branched.port.as_deref(), Some("true"));
    }

    #[test]
    fn no_caps_ctx_refuses_everything() {
        let mut c = NoCapsCtx;
        assert!(matches!(
            c.http(&HttpRequest::default()),
            Err(HttpCapError::NotGranted)
        ));
        assert!(matches!(
            c.pg_query("select 1", &[]),
            Err(PgCapError::NotGranted)
        ));
        assert!(matches!(
            c.pg_execute("select 1", &[]),
            Err(PgCapError::NotGranted)
        ));
        assert!(matches!(c.catalog_json(), Err(PgCapError::NotGranted)));
        assert!(!c.raw_sql_enabled());
    }
}
