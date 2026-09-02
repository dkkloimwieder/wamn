//! Where a stream goes.
//!
//! The trait is the seam; exactly one target is implemented.
//!
//! **There is deliberately no JetStream sink.** `docs/poc/wms-prep-spec.md` §1a
//! holds it at trait-only until app 2 defines the external ingress contract,
//! and the platform would refuse a fabricated one anyway: post-`.4.5` identity
//! rules (`wamn_materializer`'s `verified_source_event_id` /
//! `verified_derived_source_event_id`) derive an envelope's identity from a
//! real WAL LSN or a host-side digest, so a simulator cannot manufacture an
//! internal WAMN CDC/event envelope that survives the materializer.
//! `tests/integration/src/streambench.rs` looks like a template for one and is
//! not: it asserts at the JetStream substrate and never passes the
//! materializer, which is the only reason its synthetic LSNs work.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value};

use wamn_execution_contract::canonical_json_bytes;

use crate::Event;

/// The array envelope's hard ceiling, mirroring the guest-side authority in
/// `components/data/receiving-data/src/operation.rs`. Batching is not pacing:
/// [`Profile::rate`](crate::Profile::rate) never feeds this.
pub const MAX_ENVELOPE_ITEMS: usize = 100;

/// One item's outcome, correlated back by `request_id`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ItemOutcome {
    /// Echoes the request item's `request_id`.
    pub request_id: String,
    /// Present when the item succeeded.
    #[serde(default)]
    pub value: Option<Value>,
    /// Present when the item was refused.
    #[serde(default)]
    pub error: Option<Value>,
}

/// A destination a generated stream can be driven at.
///
/// Implementations drive a real route or a real consumer. None of them writes
/// the database — see the crate docs.
#[async_trait]
pub trait EmissionTarget {
    /// Emit `events` and return one outcome per item, in request order.
    async fn emit(&self, events: &[Event]) -> anyhow::Result<Vec<ItemOutcome>>;
}

/// Drives a published route over HTTP with the array envelope and a PAT.
///
/// The protocol structure here is not an API contract (`wms-prep-spec.md` §1a):
/// it mirrors the guest-side envelope authority and the header recipe the
/// route-authentication proof uses, and it is expected to move when they do.
/// It is a fresh client rather than a lift of
/// `tests/integration/src/route_authentication_live.rs` because nothing there
/// is `pub` — the whole module is `#[cfg(test)]` — and because that file is an
/// in-process `wasi-http` lowering, not a network client.
#[derive(Clone, Debug)]
pub struct HttpRouteTarget {
    client: reqwest::Client,
    route_url: String,
    pat: String,
}

impl HttpRouteTarget {
    /// Bind a target to one published route URL and the PAT that may call it.
    ///
    /// Mint the PAT with `wamn_platform_identity::issue_pat` against a subject
    /// from `route_caller_subject`; do not hand-assemble a token.
    #[must_use]
    pub fn new(
        client: reqwest::Client,
        route_url: impl Into<String>,
        pat: impl Into<String>,
    ) -> Self {
        Self {
            client,
            route_url: route_url.into(),
            pat: pat.into(),
        }
    }

    /// The request body for one batch: an array of objects, each carrying the
    /// event's id as its `request_id`.
    ///
    /// A duplicated event keeps its original `event_id`, so a redelivery
    /// arrives as the same `request_id` — which is exactly the idempotency
    /// question a duplicate-rate knob exists to ask.
    fn envelope(events: &[Event]) -> Value {
        let items: Vec<Value> = events
            .iter()
            .map(|event| {
                let mut item = match &event.body {
                    Value::Object(fields) => fields.clone(),
                    other => {
                        let mut wrapper = Map::new();
                        wrapper.insert("body".to_owned(), other.clone());
                        wrapper
                    }
                };
                item.insert(
                    "request_id".to_owned(),
                    Value::String(event.event_id.clone()),
                );
                Value::Object(item)
            })
            .collect();
        Value::Array(items)
    }
}

#[async_trait]
impl EmissionTarget for HttpRouteTarget {
    async fn emit(&self, events: &[Event]) -> anyhow::Result<Vec<ItemOutcome>> {
        let mut outcomes = Vec::with_capacity(events.len());
        for batch in events.chunks(MAX_ENVELOPE_ITEMS) {
            let response = self
                .client
                .post(&self.route_url)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", self.pat))
                .body(canonical_json_bytes(&Self::envelope(batch)))
                .send()
                .await?;
            let status = response.status();
            let body = response.text().await?;
            if !status.is_success() {
                let route = &self.route_url;
                let items = batch.len();
                anyhow::bail!(
                    "route {route} refused a {items}-item envelope with {status}: {body}"
                );
            }
            outcomes.extend(serde_json::from_str::<Vec<ItemOutcome>>(&body)?);
        }
        Ok(outcomes)
    }
}
