//! The scenario egress policy adapter.
//!
//! [`ScenarioEgress`] is the [`HostHandler`] used by the scenario worker. It
//! denies requests that fail either the host's trusted allowlist or the flow's
//! trusted declaration, then forwards allowed requests.

use std::sync::Arc;

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

/// Enforces the trusted host/flow policy intersection for outbound requests.
pub struct ScenarioEgress {
    inner: DefaultOutgoingHandler,
    flow_policy: Arc<RunnerEgressPolicy>,
}

impl std::fmt::Debug for ScenarioEgress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScenarioEgress")
            .finish_non_exhaustive()
    }
}

impl ScenarioEgress {
    /// Build an adapter sharing the flowrunner's trusted declaration policy.
    pub fn enforcing(flow_policy: Arc<RunnerEgressPolicy>) -> Self {
        Self {
            inner: DefaultOutgoingHandler,
            flow_policy,
        }
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
        if check_allowed_hosts(request, allowed_hosts).is_err() {
            return false;
        }
        let declared = self.flow_policy.declared(flow);
        check_allowed_hosts(request, declared.as_deref().unwrap_or(&[])).is_ok()
    }
}

#[async_trait::async_trait]
impl HostHandler for ScenarioEgress {
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
        if !allowed {
            // A clean HttpRequestDenied (not a trap) keeps the instance alive.
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
        let egress = ScenarioEgress::enforcing(policy);

        assert!(egress.is_allowed(
            "flow-a",
            &request("echo.local:8080"),
            &[allowed_host("echo.local:8080")]
        ));
    }

    #[test]
    fn flow_declared_but_outer_unauthorized_request_is_denied() {
        let policy = Arc::new(RunnerEgressPolicy::default());
        policy.set_declared("flow-a", &["echo.local:8080".into()]);
        let egress = ScenarioEgress::enforcing(policy);

        assert!(!egress.is_allowed("flow-a", &request("echo.local:8080"), &[]));
    }

    #[test]
    fn outer_authorization_cannot_bypass_missing_flow_declaration() {
        let egress = ScenarioEgress::enforcing(Arc::new(RunnerEgressPolicy::default()));

        assert!(!egress.is_allowed(
            "flow-a",
            &request("echo.local:8080"),
            &[allowed_host("echo.local:8080")]
        ));
    }

    #[test]
    fn adapter_observes_the_shared_flowrunner_policy() {
        let policy = Arc::new(RunnerEgressPolicy::default());
        let egress = ScenarioEgress::enforcing(policy.clone());
        let request = request("echo.local:8080");
        let outer = [allowed_host("echo.local:8080")];

        assert!(!egress.is_allowed("flow-a", &request, &outer));
        policy.set_declared("flow-a", &["echo.local:8080".into()]);
        assert!(egress.is_allowed("flow-a", &request, &outer));
    }

    #[test]
    fn unconfigured_adapter_denies_by_default() {
        let egress = ScenarioEgress::enforcing(Arc::new(RunnerEgressPolicy::default()));

        assert!(!egress.is_allowed("flow-a", &request("echo.local:8080"), &[]));
    }
}
