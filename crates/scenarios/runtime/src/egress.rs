//! The scenario egress recorder: the S6 egress *spy* generalized to
//! a recorder + trusted authorization + assertion surface.
//!
//! A [`RecordingEgress`] is a [`HostHandler`] the scenario worker uses instead
//! of production egress. It records every outbound request
//! (`{workload, method, authority, path}`) and, in *spy* mode, DENIES any that
//! fails either the host's trusted allowlist or the flow's trusted declaration
//! — recorded, never sent, a clean `HttpRequestDenied` the guest classifies
//! `egress-denied`. In *forward* mode it forwards everything (still recording)
//! — the prod-parity audit stance.
//! The audit read API ([`records`](RecordingEgress::records) /
//! [`denied`](RecordingEgress::denied) /
//! [`saw_authority`](RecordingEgress::saw_authority)) lets a scenario assert
//! exactly what egress a flow attempted.
//!
//! Assertions consume the recorded facts later; the recorder never receives
//! assertion inputs.

use std::sync::{Arc, Mutex};

use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{
    DefaultOutgoingHandler, HostHandler, OutgoingHandler as _, check_allowed_hosts,
};
use wasmtime_wasi_http::p2::HttpResult;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};

use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;

pub use wamn_scenario_model::EgressObservation;

/// Records every outbound request; optionally enforces trusted egress policy.
pub struct RecordingEgress {
    inner: DefaultOutgoingHandler,
    records: Mutex<Vec<EgressObservation>>,
    flow_policy: Arc<RunnerEgressPolicy>,
    /// When `true`, both trusted policy layers are enforced (spy mode). When
    /// `false`, everything is forwarded (audit-only / prod parity).
    enforce_policy: bool,
}

impl std::fmt::Debug for RecordingEgress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordingEgress")
            .field("enforce_policy", &self.enforce_policy)
            .finish_non_exhaustive()
    }
}

impl RecordingEgress {
    /// A forward-all recorder (audit only; nothing denied) — the prod egress
    /// analog that still records for the sameness/regression comparison.
    pub fn forwarding() -> Self {
        Self::new(false, Arc::new(RunnerEgressPolicy::default()))
    }

    /// A spy recorder sharing the trusted flowrunner declaration policy.
    pub fn spying(flow_policy: Arc<RunnerEgressPolicy>) -> Self {
        Self::new(true, flow_policy)
    }

    fn new(enforce_policy: bool, flow_policy: Arc<RunnerEgressPolicy>) -> Self {
        Self {
            inner: DefaultOutgoingHandler,
            records: Mutex::new(Vec::new()),
            flow_policy,
            enforce_policy,
        }
    }

    /// The full audit log, in order.
    pub fn records(&self) -> Vec<EgressObservation> {
        self.records.lock().expect("records lock poisoned").clone()
    }

    /// The recorded requests that were denied by trusted authorization.
    pub fn denied(&self) -> Vec<EgressObservation> {
        self.records().into_iter().filter(|r| !r.allowed).collect()
    }

    /// Whether any recorded request's authority contains `needle` — an
    /// assertion helper (the bench asserts it saw the echo / caught the planted
    /// metadata host).
    pub fn saw_authority(&self, needle: &str) -> bool {
        self.records().iter().any(|r| r.authority.contains(needle))
    }

    /// Clear the audit log between scenario phases.
    pub fn clear(&self) {
        self.records.lock().expect("records lock poisoned").clear();
    }

    /// The load-bearing decision: does a request pass both the trusted host
    /// allowlist and the current flow's trusted declaration? Either absent list
    /// denies all.
    pub fn is_allowed<B>(
        &self,
        flow: &str,
        request: &hyper::Request<B>,
        allowed_hosts: &[AllowedHost],
    ) -> bool {
        if !self.enforce_policy {
            return true;
        }
        if check_allowed_hosts(request, allowed_hosts).is_err() {
            return false;
        }
        let declared = self.flow_policy.declared(flow);
        check_allowed_hosts(request, declared.as_deref().unwrap_or(&[])).is_ok()
    }
}

#[async_trait::async_trait]
impl HostHandler for RecordingEgress {
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
        allowed_hosts: &[AllowedHost],
    ) -> HttpResult<HostFutureIncomingResponse> {
        let allowed = self.is_allowed(workload_id, &request, allowed_hosts);
        let uri = request.uri();
        let authority = uri.authority().map(|a| a.to_string()).unwrap_or_default();
        let path = uri.path().to_string();
        self.records
            .lock()
            .expect("records lock poisoned")
            .push(EgressObservation {
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

    fn allowed_host(value: &str) -> AllowedHost {
        value.parse().expect("test allowed-host parses")
    }

    fn request(authority: &str) -> hyper::Request<()> {
        hyper::Request::builder()
            .uri(format!("http://{authority}/notify"))
            .body(())
            .expect("test request")
    }

    #[test]
    fn outer_and_flow_authorized_request_is_allowed() {
        let policy = Arc::new(RunnerEgressPolicy::default());
        policy.set_declared("flow-a", &["echo.local:8080".into()]);
        let rec = RecordingEgress::spying(policy);

        assert!(rec.is_allowed(
            "flow-a",
            &request("echo.local:8080"),
            &[allowed_host("echo.local:8080")]
        ));
    }

    #[test]
    fn flow_declared_but_outer_unauthorized_request_is_denied() {
        let policy = Arc::new(RunnerEgressPolicy::default());
        policy.set_declared("flow-a", &["echo.local:8080".into()]);
        let rec = RecordingEgress::spying(policy);

        assert!(!rec.is_allowed("flow-a", &request("echo.local:8080"), &[]));
    }

    #[test]
    fn outer_authorization_cannot_bypass_missing_flow_declaration() {
        let rec = RecordingEgress::spying(Arc::new(RunnerEgressPolicy::default()));

        assert!(!rec.is_allowed(
            "flow-a",
            &request("echo.local:8080"),
            &[allowed_host("echo.local:8080")]
        ));
    }

    #[test]
    fn spy_observes_the_shared_flowrunner_policy() {
        let policy = Arc::new(RunnerEgressPolicy::default());
        let rec = RecordingEgress::spying(policy.clone());
        let request = request("echo.local:8080");
        let outer = [allowed_host("echo.local:8080")];

        assert!(!rec.is_allowed("flow-a", &request, &outer));
        policy.set_declared("flow-a", &["echo.local:8080".into()]);
        assert!(rec.is_allowed("flow-a", &request, &outer));
    }

    #[test]
    fn unconfigured_spy_denies_by_default() {
        let rec = RecordingEgress::spying(Arc::new(RunnerEgressPolicy::default()));

        assert!(!rec.is_allowed("flow-a", &request("echo.local:8080"), &[]));
    }

    #[test]
    fn forwarding_allows_everything() {
        let rec = RecordingEgress::forwarding();
        assert!(rec.is_allowed("any-flow", &request("anywhere.example"), &[]));
        assert!(rec.is_allowed("any-flow", &request("169.254.169.254"), &[]));
    }
}
