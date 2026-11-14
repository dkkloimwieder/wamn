//! Bind native platform endpoints to the application's ingress host.

use std::sync::Arc;
use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::http::{
    HostHandler, IngressRoute, NormalizedPathPrefix, RouteScope, ServiceHttpJob,
};

use crate::HostRouteDelivery;
use wamn_engine::flow_http_routing::FlowHttpRouting;

/// Native handlers use the same ingress host as their application workload.
pub struct PlatformHttp {
    ingress: Arc<dyn HostHandler>,
    handlers: Arc<HostRouteDelivery>,
    routing: Arc<FlowHttpRouting>,
}
impl std::fmt::Debug for PlatformHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformHttp").finish_non_exhaustive()
    }
}
impl PlatformHttp {
    /// Use the process's native ingress and authentication.
    pub fn new(
        ingress: Arc<dyn HostHandler>,
        handlers: Arc<HostRouteDelivery>,
        routing: Arc<FlowHttpRouting>,
    ) -> Self {
        Self {
            ingress,
            handlers,
            routing,
        }
    }
    /// Control endpoints bind directly, since control runs no guest workload.
    pub async fn bind(&self, id: &str, host: String) -> anyhow::Result<()> {
        let route = IngressRoute {
            host,
            path_prefix: NormalizedPathPrefix::new("/wamn_control/"),
            scope: RouteScope::Ingress,
        };
        let (sender, mut jobs) = tokio::sync::mpsc::channel::<ServiceHttpJob>(16);
        self.ingress
            .on_service_http_resolved(&format!("{id}/platform-http"), &[route], sender)
            .await?;
        let handlers = Arc::clone(&self.handlers);
        let routing = Arc::clone(&self.routing);
        tokio::spawn(async move {
            while let Some(job) = jobs.recv().await {
                let handlers = Arc::clone(&handlers);
                let routing = Arc::clone(&routing);
                tokio::spawn(async move {
                    let _ = job.resp_tx.send(Ok(handlers.http(&routing, job.req).await));
                });
            }
        });
        Ok(())
    }
}
#[async_trait::async_trait]
impl HostHandler for PlatformHttp {
    async fn inject_meters(&self, meters: &wash_runtime::observability::Meters) {
        self.ingress.inject_meters(meters).await;
    }
    async fn start(&self) -> anyhow::Result<()> {
        self.ingress.start().await
    }
    async fn stop(&self) -> anyhow::Result<()> {
        self.ingress.stop().await
    }
    async fn stopped(&self) {
        self.ingress.stopped().await;
    }
    fn port(&self) -> u16 {
        self.ingress.port()
    }
    async fn on_workload_resolved(
        &self,
        workload: &ResolvedWorkload,
        component_id: &str,
    ) -> anyhow::Result<()> {
        self.ingress
            .on_workload_resolved(workload, component_id)
            .await?;
        if let Some(host) = workload
            .host_interfaces()
            .iter()
            .find(|interface| interface.is_incoming_http_handler())
            .and_then(|interface| interface.config.get("host"))
        {
            self.bind(workload.id(), host.to_owned()).await?;
        }
        Ok(())
    }
    async fn on_workload_unbind(&self, workload_id: &str) -> anyhow::Result<()> {
        let platform = self
            .ingress
            .on_service_http_unbind(&format!("{workload_id}/platform-http"))
            .await;
        let application = self.ingress.on_workload_unbind(workload_id).await;
        platform.and(application)
    }
    async fn on_service_http_resolved(
        &self,
        workload_id: &str,
        routes: &[IngressRoute],
        sender: tokio::sync::mpsc::Sender<ServiceHttpJob>,
    ) -> anyhow::Result<()> {
        self.ingress
            .on_service_http_resolved(workload_id, routes, sender)
            .await
    }
    async fn on_service_http_unbind(&self, workload_id: &str) -> anyhow::Result<()> {
        self.ingress.on_service_http_unbind(workload_id).await
    }
    async fn on_trigger_service_messaging_resolved(
        &self,
        workload_id: &str,
        sender: tokio::sync::mpsc::Sender<wash_runtime::host::trigger_service::MessagingJob>,
    ) -> anyhow::Result<()> {
        self.ingress
            .on_trigger_service_messaging_resolved(workload_id, sender)
            .await
    }
    async fn on_trigger_service_messaging_unbind(&self, workload_id: &str) -> anyhow::Result<()> {
        self.ingress
            .on_trigger_service_messaging_unbind(workload_id)
            .await
    }
    async fn deliver_trigger_service_message(
        &self,
        workload_id: &str,
        message: wash_runtime::host::trigger_service::BrokerMessage,
        attributes: Arc<[opentelemetry::KeyValue]>,
    ) -> anyhow::Result<Result<(), String>> {
        self.ingress
            .deliver_trigger_service_message(workload_id, message, attributes)
            .await
    }
    async fn has_trigger_service_messaging(&self, workload_id: &str) -> bool {
        self.ingress
            .has_trigger_service_messaging(workload_id)
            .await
    }
    fn outgoing_request(
        &self,
        workload_id: &str,
        request: hyper::Request<wasmtime_wasi_http::WasiBody>,
        options: Option<wasmtime_wasi_http::RequestOptions>,
        future: wash_runtime::host::http::RequestIoFuture,
        allowed_hosts: &[wash_runtime::host::allowed_hosts::AllowedHost],
    ) -> wash_runtime::host::http::SendFuture {
        self.ingress
            .outgoing_request(workload_id, request, options, future, allowed_hosts)
    }
}
