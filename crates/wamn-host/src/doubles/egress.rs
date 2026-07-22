//! The egress recorder (production delta 3): the S6 egress *spy* generalized to
//! a recorder + per-flow allowlist + assertion surface.
//!
//! A [`EgressRecorder`] is a [`HostHandler`] the test host swaps in for the
//! production egress handler. It RECORDS every outbound request
//! (`{workload, method, authority, path}`) and, in *spy* mode, DENIES any whose
//! authority is not on its flow's expectation list — recorded, never sent, a
//! clean `HttpRequestDenied` the guest classifies `egress-denied`. In *forward*
//! mode it forwards everything (still recording) — the prod-parity audit stance.
//! The audit read API ([`records`](EgressRecorder::records) /
//! [`denied`](EgressRecorder::denied) / [`saw_authority`](EgressRecorder::saw_authority))
//! lets a gate or test assert exactly what egress a flow attempted.
//!
//! Expectation lists are keyed by the store's *workload id* (the declaring
//! component/flow), so one recorder can hold per-flow policy across many flows —
//! the generalization over the bench's single global `expected` set.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, RwLock};

use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{DefaultOutgoingHandler, HostHandler, OutgoingHandler as _};
use wasmtime_wasi_http::p2::HttpResult;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};

/// One recorded outbound request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressRecord {
    /// The declaring flow/component (the store's workload id).
    pub workload_id: String,
    /// The request method (`GET`, `POST`, …).
    pub method: String,
    /// The target authority (`host[:port]`) — the allow/deny key.
    pub authority: String,
    /// The request path.
    pub path: String,
    /// Whether the recorder forwarded it (`true`) or denied it as unexpected
    /// (`false`).
    pub allowed: bool,
}

/// Records every outbound request; optionally denies any not on its flow's
/// per-flow expectation list.
pub struct EgressRecorder {
    inner: DefaultOutgoingHandler,
    records: Mutex<Vec<EgressRecord>>,
    /// flow (workload id) → the authorities it may reach.
    expectations: RwLock<HashMap<String, HashSet<String>>>,
    /// When `true`, an authority not on its flow's expectation list is denied
    /// (spy mode). When `false`, everything is forwarded (audit-only / prod
    /// parity).
    deny_unexpected: bool,
}

impl EgressRecorder {
    /// A forward-all recorder (audit only; nothing denied) — the prod egress
    /// analog that still records for the sameness/regression comparison.
    pub fn forwarding() -> Self {
        Self::new(false)
    }

    /// A spy recorder: authorities absent from a flow's expectation list are
    /// denied. Declare each flow's list with [`expect`](Self::expect).
    pub fn spying() -> Self {
        Self::new(true)
    }

    fn new(deny_unexpected: bool) -> Self {
        Self {
            inner: DefaultOutgoingHandler,
            records: Mutex::new(Vec::new()),
            expectations: RwLock::new(HashMap::new()),
            deny_unexpected,
        }
    }

    /// Declare (replacing any prior list) the authorities `flow` may reach.
    pub fn expect<I, S>(&self, flow: &str, authorities: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let set = authorities.into_iter().map(Into::into).collect();
        self.expectations
            .write()
            .expect("expectations lock poisoned")
            .insert(flow.to_string(), set);
    }

    /// The full audit log, in order.
    pub fn records(&self) -> Vec<EgressRecord> {
        self.records.lock().expect("records lock poisoned").clone()
    }

    /// The recorded requests that were denied (unexpected authorities).
    pub fn denied(&self) -> Vec<EgressRecord> {
        self.records().into_iter().filter(|r| !r.allowed).collect()
    }

    /// Whether any recorded request's authority contains `needle` — an
    /// assertion helper (the bench asserts it saw the echo / caught the planted
    /// metadata host).
    pub fn saw_authority(&self, needle: &str) -> bool {
        self.records().iter().any(|r| r.authority.contains(needle))
    }

    /// Clear the audit log (between test cases / phases).
    pub fn clear(&self) {
        self.records.lock().expect("records lock poisoned").clear();
    }

    /// The load-bearing decision: is a request from `flow` to `authority`
    /// allowed out? In spy mode the authority MUST be on the flow's expectation
    /// list; in forward mode everything is allowed. Extracted as a pure fn so
    /// the allow/deny rule is unit-testable without driving real HTTP.
    pub fn is_allowed(&self, flow: &str, authority: &str) -> bool {
        if !self.deny_unexpected {
            return true;
        }
        self.expectations
            .read()
            .expect("expectations lock poisoned")
            .get(flow)
            .is_some_and(|set| set.contains(authority))
    }
}

#[async_trait::async_trait]
impl HostHandler for EgressRecorder {
    async fn start(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn stop(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn port(&self) -> u16 {
        0
    }
    async fn on_workload_resolved(
        &self,
        _resolved: &ResolvedWorkload,
        _component_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn on_workload_unbind(&self, _workload_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn outgoing_request(
        &self,
        workload_id: &str,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
        _allowed_hosts: &[AllowedHost],
    ) -> HttpResult<HostFutureIncomingResponse> {
        let uri = request.uri();
        let authority = uri.authority().map(|a| a.to_string()).unwrap_or_default();
        let path = uri.path().to_string();
        let allowed = self.is_allowed(workload_id, &authority);
        self.records
            .lock()
            .expect("records lock poisoned")
            .push(EgressRecord {
                workload_id: workload_id.to_string(),
                method: request.method().to_string(),
                authority,
                path,
                allowed,
            });
        if !allowed {
            // Recorded, never sent: a clean HttpRequestDenied (not a trap) the
            // node classifies egress-denied (terminal); the instance lives.
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(
                ErrorCode::HttpRequestDenied,
            ))));
        }
        self.inner.send_request(workload_id, request, config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mutation target (delta 3): the deny check. A mutant that inverts
    // `set.contains(authority)` (allow the unexpected, deny the expected) fails
    // this NAMED test.
    #[test]
    fn spy_allows_expected_and_denies_unexpected() {
        let rec = EgressRecorder::spying();
        rec.expect("flow-a", ["echo.local:8080"]);
        assert!(
            rec.is_allowed("flow-a", "echo.local:8080"),
            "an expected authority must be allowed"
        );
        assert!(
            !rec.is_allowed("flow-a", "169.254.169.254"),
            "an unexpected authority must be denied"
        );
        // A flow with NO declared list denies everything (deny-by-default).
        assert!(!rec.is_allowed("flow-unknown", "echo.local:8080"));
    }

    #[test]
    fn forwarding_allows_everything() {
        let rec = EgressRecorder::forwarding();
        assert!(rec.is_allowed("any-flow", "anywhere.example"));
        assert!(rec.is_allowed("any-flow", "169.254.169.254"));
    }

    #[test]
    fn expectations_are_per_flow_and_replace() {
        let rec = EgressRecorder::spying();
        rec.expect("flow-a", ["a.example"]);
        rec.expect("flow-b", ["b.example"]);
        assert!(rec.is_allowed("flow-a", "a.example"));
        assert!(
            !rec.is_allowed("flow-a", "b.example"),
            "flow-a's list is its own"
        );
        assert!(rec.is_allowed("flow-b", "b.example"));

        // A later declaration REPLACES the flow's list (a narrower next run).
        rec.expect("flow-a", Vec::<String>::new());
        assert!(
            !rec.is_allowed("flow-a", "a.example"),
            "empty list = deny-all"
        );
    }
}
