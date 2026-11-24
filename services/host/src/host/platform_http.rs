//! Register application platform paths on the wasmCloud ingress router.

use super::files::Files;
use super::workflow::Workflow;
use std::sync::Arc;
use wamn_engine::ingress::IngressRouting;
use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::http::{
    HostHandler, IngressRoute, NormalizedPathPrefix, RouteScope, ServiceHttpJob,
};

pub(super) struct PlatformHttp {
    ingress: Arc<dyn HostHandler>,
    files: Arc<Files>,
    routing: Arc<IngressRouting>,
    workflow: Arc<Workflow>,
}

impl PlatformHttp {
    pub(super) fn new(
        ingress: Arc<dyn HostHandler>,
        files: Arc<Files>,
        routing: Arc<IngressRouting>,
        workflow: Arc<Workflow>,
    ) -> Self {
        Self {
            ingress,
            files,
            routing,
            workflow,
        }
    }

    async fn bind(
        &self,
        id: &str,
        host: &str,
        files_prefix: &str,
        flow_prefix: String,
    ) -> anyhow::Result<()> {
        let routes = ["/wamn_control", files_prefix, &flow_prefix].map(|prefix| IngressRoute {
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
        let workflow = Arc::clone(&self.workflow);
        let flow_prefix: Arc<str> = Arc::from(flow_prefix);
        tokio::spawn(async move {
            while let Some(mut job) = jobs.recv().await {
                let files = Arc::clone(&files);
                let routing = Arc::clone(&routing);
                let workload = workload.clone();
                let workflow = Arc::clone(&workflow);
                let flow_prefix = Arc::clone(&flow_prefix);
                tokio::spawn(async move {
                    let request = async {
                        let path = job.req.uri().path();
                        if path
                            .strip_prefix(flow_prefix.as_ref())
                            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
                        {
                            return Ok(workflow
                                .http(&routing, &workload, &flow_prefix, job.req)
                                .await);
                        }
                        let routes = files.routes(&workload).await?;
                        Ok(match routes.files.http(&routing, job.req).await {
                            std::ops::ControlFlow::Break(response) => response,
                            std::ops::ControlFlow::Continue(request) => {
                                routes
                                    .application
                                    .http(&routing, &workflow, &workload, request)
                                    .await
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
    fn install_egress_gate(&self, gate: wash_runtime::host::egress_policy::EgressAddressGate) {
        self.ingress.install_egress_gate(gate);
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
            && let Some(application) = self.routing.application_config(component_id)
        {
            let files_prefix = format!("/api/{}/files", application.application);
            let flow_prefix = wamn_routes::workflow_http_prefix(&application.application);
            self.files.register(workload.id(), application);
            self.bind(workload.id(), host, &files_prefix, flow_prefix)
                .await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use wash_runtime::host::allowed_hosts::AllowedHost;

    /// The host's address check reaches the ingress behind the platform
    /// wrapper, so an `allowedHosts: ["*"]` grant does not reach an address
    /// the socket policy refuses. `0.0.0.0` is not loopback by name, so only
    /// the address check refuses it.
    #[tokio::test]
    async fn the_platform_handler_checks_egress_addresses() {
        let pools = Arc::new(
            wamn_postgres::WamnPostgres::from_host_config(std::collections::BTreeMap::new())
                .expect("empty pools"),
        );
        let database = Arc::clone(&pools);
        let authorization = Arc::new(wamn_authentication::authorization::Authorization::new(
            Arc::clone(&pools),
            database,
        ));
        let postgres = Arc::new(wamn_plugin::OperationHost::new(Arc::clone(&pools), pools, None));
        let blobstore = Arc::new(wamn_blobstore::plugin::WamnBlobstore::new(Arc::clone(
            &postgres,
        )));
        let files = Arc::new(Files::new(&postgres, blobstore, authorization));
        let routing = Arc::new(IngressRouting::new(Arc::clone(&postgres.scopes)));
        let operations = Arc::new(wamn_engine::operation::WamnOperation::new(
            Arc::clone(&postgres) as Arc<dyn wash_runtime::plugin::HostPlugin>,
            Arc::clone(&postgres.scopes),
        ));
        let workflow = Arc::new(Workflow::new(
            postgres,
            operations,
            "nats://nats.example.com:4222".to_owned(),
            false,
        ));
        let (_stop, stopping) = tokio::sync::watch::channel(false);
        let ingress = wash_runtime::host::http::Ingress::builder(
            wamn_engine::expected_router::expected_host_router(None, stopping),
            "127.0.0.1:0".parse().expect("loopback address"),
        )
        .build()
        .await
        .expect("ingress");
        let handler: Arc<dyn HostHandler> = Arc::new(PlatformHttp::new(
            Arc::new(ingress),
            files,
            routing,
            workflow,
        ));
        let _host = wash_runtime::host::Host::builder()
            .with_engine(wamn_engine::engine::build_engine(&[]).expect("production engine"))
            .with_http_handler(Arc::clone(&handler))
            .build()
            .expect("host");
        // A port nothing listens on: without the address check the kernel
        // refuses the connection, not the host.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("free port")
            .port();
        let request = hyper::Request::builder()
            .uri(format!("http://0.0.0.0:{port}/"))
            .body(wasmtime_wasi_http::WasiBody::default())
            .expect("request");
        let allowed: [AllowedHost; 1] = ["*".parse().expect("allow any")];

        let sent = Box::into_pin(handler.outgoing_request(
            "workload",
            request,
            None,
            Box::new(async { Ok(()) }),
            &allowed,
        ))
        .await;

        assert!(
            matches!(sent, Err(wasmtime_wasi_http::Error::DestinationIpProhibited)),
            "got {:?}",
            sent.err()
        );
    }
}
