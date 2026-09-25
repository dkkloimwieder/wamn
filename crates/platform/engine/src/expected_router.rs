//! Native HTTP routing with a bounded refusal for hosts in the verified release.
//!
//! During serving, an unbound explicit release hostname reports unavailable.
//! Shutdown refuses further requests, including on existing connections.
//! Native parsing, workload selection, and outgoing policy remain unchanged.
//! Operator-created aliases and wildcard expansion are outside this projection.
//! A selected workload without an ingress handle still receives native 404.

use std::collections::HashSet;

use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{DynamicRouter, IngressRoute, RouteError, Router};
use wasmtime_wasi_http::{RequestOptions, WasiBody};

use crate::flow_http_routing::expected_http_hostnames;
use crate::release_manifest::LoadedRelease;

/// A native router whose unbound release hostnames report temporary unavailability.
pub struct ExpectedHostRouter {
    inner: DynamicRouter,
    expected_hosts: HashSet<String>,
    stopping: tokio::sync::watch::Receiver<bool>,
}

impl std::fmt::Debug for ExpectedHostRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExpectedHostRouter")
            .field("expected_hosts", &self.expected_hosts)
            .finish_non_exhaustive()
    }
}

/// Project explicit hostnames once from the host's verified release.
pub fn expected_host_router(
    release: Option<&LoadedRelease>,
    stopping: tokio::sync::watch::Receiver<bool>,
) -> ExpectedHostRouter {
    ExpectedHostRouter {
        inner: DynamicRouter::default(),
        expected_hosts: release
            .map(|release| expected_http_hostnames(release.manifest()))
            .unwrap_or_default(),
        stopping,
    }
}

#[async_trait::async_trait]
impl Router for ExpectedHostRouter {
    async fn on_workload_resolved(
        &self,
        resolved_handle: &ResolvedWorkload,
        component_id: &str,
    ) -> anyhow::Result<()> {
        self.inner
            .on_workload_resolved(resolved_handle, component_id)
            .await
    }

    async fn on_workload_unbind(&self, workload_id: &str) -> anyhow::Result<()> {
        self.inner.on_workload_unbind(workload_id).await
    }

    async fn on_service_http_resolved(
        &self,
        workload_id: &str,
        routes: &[IngressRoute],
    ) -> anyhow::Result<()> {
        self.inner
            .on_service_http_resolved(workload_id, routes)
            .await
    }

    fn allow_outgoing_request(
        &self,
        workload_id: &str,
        request: &hyper::Request<WasiBody>,
        options: Option<RequestOptions>,
        allowed_hosts: &[AllowedHost],
    ) -> anyhow::Result<()> {
        self.inner
            .allow_outgoing_request(workload_id, request, options, allowed_hosts)
    }

    fn route_incoming_request(
        &self,
        request: &hyper::Request<hyper::body::Incoming>,
    ) -> Result<String, RouteError> {
        if *self.stopping.borrow() {
            return Err(RouteError::Unavailable);
        }
        match self.inner.route_incoming_request(request) {
            Err(RouteError::NoWorkloadForHost(host)) if self.expected_hosts.contains(&host) => {
                Err(RouteError::Unavailable)
            }
            result => result,
        }
    }

    fn route_local_egress(
        &self,
        uri: &hyper::Uri,
        can_serve: &mut dyn FnMut(&str) -> bool,
    ) -> Option<String> {
        self.inner.route_local_egress(uri, can_serve)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use wash_runtime::host::http::{HostHandler, Ingress, ServiceHttpJob};

    use super::*;

    const HOST: &str = "api.example.test";

    fn release() -> LoadedRelease {
        let hash = wamn_catalog::DefinitionHash::parse(format!("sha256:{}", "a".repeat(64)))
            .expect("fixture definition hash");
        let manifest = wamn_catalog::ServingManifest::new(
            wamn_catalog::ServingRelease {
                tenant_id: "tenant".into(),
                effective_release_id: wamn_catalog::EffectiveReleaseId::new(1).unwrap(),
                environment: "test".into(),
                packages: BTreeSet::from([
                    wamn_catalog::PackageCoordinate::new("app", "1.0.0").unwrap()
                ]),
            },
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeSet::from([wamn_catalog::ServingWiring {
                package_id: "app".into(),
                wiring_id: "route".into(),
                wiring_version: 1,
                graph_hash: hash.clone(),
            }]),
            BTreeMap::from([(
                "route".into(),
                wamn_catalog::ServingAttachment {
                    kind: wamn_catalog::AttachmentKind::Http,
                    package_id: "app".into(),
                    target: wamn_catalog::AttachmentTarget::Wiring {
                        wiring_id: "route".into(),
                        wiring_version: 1,
                    },
                    definition_hash: hash,
                    definition: json!({"route": {"host": HOST, "path": "/", "method": "GET"}}),
                    auth_policy: json!({"modes": ["none"]}),
                    registered_operation: None,
                },
            )]),
            BTreeMap::new(),
        )
        .expect("fixture manifest");
        LoadedRelease::load_canonical_bytes(&manifest.canonical_bytes(), "expected-router-test")
            .expect("fixture release passes the production reader")
    }

    async fn request(
        ingress: &Ingress<ExpectedHostRouter>,
        host: Option<&str>,
        path: &str,
    ) -> (u16, String) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut connection = tokio::net::TcpStream::connect(ingress.addr())
                .await
                .expect("connect to native ingress");
            let host = host
                .map(|host| format!("Host: {host}\r\n"))
                .unwrap_or_default();
            connection
                .write_all(
                    format!("GET {path} HTTP/1.0\r\n{host}Connection: close\r\n\r\n").as_bytes(),
                )
                .await
                .expect("write HTTP request");
            let mut bytes = Vec::new();
            connection
                .read_to_end(&mut bytes)
                .await
                .expect("read HTTP response");
            let response = String::from_utf8(bytes).expect("response is text");
            let status = response
                .split_whitespace()
                .nth(1)
                .expect("HTTP status")
                .parse()
                .unwrap();
            (status, response)
        })
        .await
        .expect("native ingress responds within the test deadline")
    }

    #[tokio::test]
    async fn native_ingress_preserves_refusals_bind_transitions_and_application_404() {
        let loaded_release = release();
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let ingress = Ingress::new(
            expected_host_router(Some(&loaded_release), stopping),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await
        .expect("bind native ingress");
        ingress.start().await.expect("start native ingress");
        let dispatched = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&dispatched);
        let (sender, mut jobs) = tokio::sync::mpsc::channel::<ServiceHttpJob>(2);
        let handler = tokio::spawn(async move {
            while let Some(job) = jobs.recv().await {
                calls.fetch_add(1, Ordering::SeqCst);
                let status = if job.req.uri().path() == "/missing" {
                    404
                } else {
                    204
                };
                job.resp_tx
                    .send(Ok(hyper::Response::builder()
                        .status(status)
                        .header("x-application-response", "preserved")
                        .body(WasiBody::default())
                        .unwrap()))
                    .expect("native ingress receives the application response");
            }
        });

        let (status, refusal) = request(&ingress, Some(HOST), "/").await;
        assert_eq!(
            status, 503,
            "an expected host refuses before its first bind"
        );
        assert!(!refusal.to_ascii_lowercase().contains("retry-after:"));
        assert_eq!(
            request(&ingress, Some("api.example.test:8080"), "/")
                .await
                .0,
            503
        );
        assert_eq!(
            request(&ingress, Some("unknown.example.test"), "/").await.0,
            404
        );
        assert_eq!(request(&ingress, None, "/").await.0, 400);
        assert_eq!(dispatched.load(Ordering::SeqCst), 0);

        ingress
            .on_service_http_resolved("service", &[IngressRoute::ingress(HOST)], sender.clone())
            .await
            .expect("bind the native service handler");
        assert_eq!(request(&ingress, Some(HOST), "/").await.0, 204);
        let (status, response) = request(&ingress, Some(HOST), "/missing").await;
        assert_eq!(status, 404, "an application refusal retains its status");
        assert!(response.contains("x-application-response: preserved"));
        assert_eq!(
            request(&ingress, Some("unknown.example.test"), "/").await.0,
            404
        );
        assert_eq!(dispatched.load(Ordering::SeqCst), 2);
        ingress
            .on_workload_unbind("service")
            .await
            .expect("unbind the native workload");
        assert_eq!(request(&ingress, Some(HOST), "/").await.0, 503);
        assert_eq!(
            dispatched.load(Ordering::SeqCst),
            2,
            "a routing refusal never dispatches"
        );
        ingress
            .on_service_http_resolved(
                "replacement",
                &[IngressRoute::ingress(HOST)],
                sender.clone(),
            )
            .await
            .expect("bind a replacement service");
        assert_eq!(request(&ingress, Some(HOST), "/").await.0, 204);
        stop.send(true).expect("router stop receiver");
        assert_eq!(request(&ingress, Some(HOST), "/").await.0, 503);
        assert_eq!(
            dispatched.load(Ordering::SeqCst),
            3,
            "shutdown gate refuses before bound service dispatch"
        );
        ingress
            .on_service_http_unbind("replacement")
            .await
            .expect("unbind the service");
        assert_eq!(request(&ingress, Some(HOST), "/").await.0, 503);
        assert_eq!(dispatched.load(Ordering::SeqCst), 3);

        ingress.stop().await.expect("stop native ingress");
        tokio::time::timeout(Duration::from_secs(5), ingress.stopped())
            .await
            .expect("ingress stops");
        drop(sender);
        handler.await.expect("application handler stops");
    }

    #[tokio::test]
    async fn a_missing_native_handle_still_returns_404() {
        let loaded_release = release();
        let router =
            expected_host_router(Some(&loaded_release), tokio::sync::watch::channel(false).1);
        router
            .on_service_http_resolved("missing-handle", &[IngressRoute::ingress(HOST)])
            .await
            .expect("register a route before its native handle exists");
        let ingress = Ingress::new(router, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        ingress.start().await.unwrap();
        assert_eq!(request(&ingress, Some(HOST), "/").await.0, 404);
        ingress.stop().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), ingress.stopped())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn no_release_keeps_native_unknown_host_behavior() {
        let ingress = Ingress::new(
            expected_host_router(None, tokio::sync::watch::channel(false).1),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await
        .unwrap();
        ingress.start().await.unwrap();
        assert_eq!(request(&ingress, Some(HOST), "/").await.0, 404);
        ingress.stop().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), ingress.stopped())
            .await
            .unwrap();
    }

    #[test]
    fn outgoing_http_keeps_native_host_policy() {
        let router = expected_host_router(None, tokio::sync::watch::channel(false).1);
        let request = hyper::Request::builder()
            .uri("https://allowed.example.test/path")
            .body(WasiBody::default())
            .unwrap();
        let options = RequestOptions {
            connect_timeout: Some(Duration::from_secs(1)),
            first_byte_timeout: Some(Duration::from_secs(1)),
            between_bytes_timeout: Some(Duration::from_secs(1)),
        };
        for (policy, allowed) in [
            (Vec::new(), false),
            (
                vec!["https://other.example.test".parse::<AllowedHost>().unwrap()],
                false,
            ),
            (
                vec![
                    "https://allowed.example.test"
                        .parse::<AllowedHost>()
                        .unwrap(),
                ],
                true,
            ),
        ] {
            for options in [None, Some(options)] {
                assert_eq!(
                    router
                        .allow_outgoing_request("service", &request, options, &policy)
                        .is_ok(),
                    allowed
                );
            }
        }
    }
}
