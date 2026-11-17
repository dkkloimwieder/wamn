//! Register application platform paths on the wasmCloud ingress router.

use super::files::Files;
use super::workflow::Workflow;
use std::sync::Arc;
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::http::{
    HostHandler, IngressRoute, NormalizedPathPrefix, RouteScope, ServiceHttpJob,
};

pub(super) struct PlatformHttp {
    ingress: Arc<dyn HostHandler>,
    files: Arc<Files>,
    routing: Arc<FlowHttpRouting>,
    #[expect(dead_code, reason = "The workflow routes use the shared plugin in 7.4")]
    pub(super) workflow: Arc<Workflow>,
}

impl PlatformHttp {
    pub(super) fn new(
        ingress: Arc<dyn HostHandler>,
        files: Arc<Files>,
        routing: Arc<FlowHttpRouting>,
        workflow: Arc<Workflow>,
    ) -> Self {
        Self {
            ingress,
            files,
            routing,
            workflow,
        }
    }

    async fn bind(&self, id: &str, host: &str, prefix: &str) -> anyhow::Result<()> {
        let routes = ["/wamn_control", prefix].map(|prefix| IngressRoute {
            host: host.to_owned(),
            path_prefix: NormalizedPathPrefix::new(prefix),
            scope: RouteScope::Ingress,
        });
        let (sender, mut jobs) = tokio::sync::mpsc::channel::<ServiceHttpJob>(16);
        self.ingress
            .on_service_http_resolved(&format!("{id}/platform-http"), &routes, sender)
            .await?;
        let files = Arc::clone(&self.files);
        let routing = Arc::clone(&self.routing);
        let workload = id.to_owned();
        tokio::spawn(async move {
            while let Some(mut job) = jobs.recv().await {
                let files = Arc::clone(&files);
                let routing = Arc::clone(&routing);
                let workload = workload.clone();
                tokio::spawn(async move {
                    let request = async {
                        let routes = files.routes(&workload).await?;
                        Ok(match routes.files.http(&routing, job.req).await {
                            std::ops::ControlFlow::Break(response) => response,
                            std::ops::ControlFlow::Continue(request) => {
                                routes.application.http(&routing, request).await
                            }
                        })
                    };
                    tokio::select! {
                        () = job.resp_tx.closed() => {},
                        response = request => { let _ = job.resp_tx.send(response); }
                    }
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
            let components = workload.components();
            let components = components.read().await;
            let application = components
                .get(component_id)
                .and_then(|component| {
                    component
                        .local_resources()
                        .config
                        .get(wamn_catalog::application_config::APPLICATION_CONFIG)
                })
                .map(|config| {
                    serde_json::from_str::<wamn_catalog::application_config::ApplicationConfig>(
                        config,
                    )
                })
                .transpose()?;
            drop(components);
            if let Some(application) = application {
                let prefix = format!("/api/{}/files", application.application);
                self.files.register(workload.id(), application);
                self.bind(workload.id(), host, &prefix).await?;
            }
        }
        Ok(())
    }
    async fn on_workload_unbind(&self, workload_id: &str) -> anyhow::Result<()> {
        self.files.unregister(workload_id);
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
