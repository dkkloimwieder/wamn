//! Probe-only request transformation, not WAMN binding or invocation authority.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use opentelemetry::propagation::Injector;
use serde_json::{Value, json};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use wash_runtime::host::http::{DefaultOutgoingHandler, OutgoingHandler};
use wash_runtime::host::http_client::PooledClient;
use wash_runtime::host::http_p3::{P3Body, P3RequestErrorFuture, P3SendFuture};
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};
use wasmtime_wasi_http::p2::{HttpError, HttpResult};

use crate::server::HOST_CREDENTIAL;

pub struct ProbeHook {
    pub native: DefaultOutgoingHandler,
    pub aliases: BTreeMap<String, hyper::Uri>,
    pub calls: Arc<Mutex<Vec<Value>>>,
    pub grpc_selections: Arc<AtomicUsize>,
}

struct Headers<'a>(&'a mut hyper::HeaderMap);

impl Injector for Headers<'_> {
    fn set(&mut self, key: &str, value: String) {
        if value.is_empty() {
            return;
        }
        let name = hyper::header::HeaderName::from_bytes(key.as_bytes()).expect("W3C header name");
        if !self.0.contains_key(&name) {
            self.0
                .insert(name, value.parse().expect("W3C header value"));
        }
    }
}

impl OutgoingHandler for ProbeHook {
    fn send_request(
        &self,
        workload_id: &str,
        mut request: hyper::Request<HyperOutgoingBody>,
        mut config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        let span = tracing::info_span!("probe_only_hook", workload_id);
        let _entered = span.enter();
        let context = span.context();
        let original = request.uri().to_string();
        self.calls
            .lock()
            .expect("hook observation lock")
            .push(json!({"workload_id": workload_id, "uri": original}));
        if request.headers().contains_key("authorization")
            || !request.uri().path().starts_with("/allowed/")
        {
            return Err(HttpError::from(ErrorCode::HttpRequestDenied));
        }
        let alias = request
            .uri()
            .authority()
            .map(|authority| authority.as_str())
            .unwrap_or_default();
        let destination = self
            .aliases
            .get(alias)
            .ok_or_else(|| HttpError::from(ErrorCode::HttpRequestDenied))?;
        let path = request
            .uri()
            .path_and_query()
            .expect("guest request path")
            .clone();
        let uri = hyper::Uri::builder()
            .scheme(destination.scheme().expect("fixture scheme").clone())
            .authority(destination.authority().expect("fixture authority").clone())
            .path_and_query(path)
            .build()
            .map_err(|_| HttpError::from(ErrorCode::HttpRequestDenied))?;
        config.use_tls = uri.scheme_str() == Some("https");
        request.headers_mut().insert(
            hyper::header::HOST,
            uri.authority()
                .expect("fixture authority")
                .as_str()
                .parse()
                .expect("fixture host"),
        );
        *request.uri_mut() = uri;
        request.headers_mut().insert(
            hyper::header::AUTHORIZATION,
            HOST_CREDENTIAL.parse().expect("sentinel"),
        );
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&context, &mut Headers(request.headers_mut()));
        });
        self.native.send_request(workload_id, request, config)
    }

    fn send_request_p3(
        &self,
        workload_id: &str,
        request: hyper::Request<P3Body>,
        options: Option<wasmtime_wasi_http::p3::RequestOptions>,
        fut: P3RequestErrorFuture,
    ) -> P3SendFuture {
        // No P3 guest or production P3 adapter is claimed by this P2 probe.
        self.native
            .send_request_p3(workload_id, request, options, fut)
    }

    fn client_tls_config(&self) -> Option<Arc<rustls::ClientConfig>> {
        self.native.client_tls_config()
    }

    fn grpc_transport(&self, workload_id: &str) -> Option<PooledClient> {
        self.grpc_selections.fetch_add(1, Ordering::SeqCst);
        self.native.grpc_transport(workload_id)
    }
}
