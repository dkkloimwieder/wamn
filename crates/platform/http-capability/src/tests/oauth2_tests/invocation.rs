//! Native invocation lifetime observations through a real HTTP/2 connection.

use std::task::{Context, Poll};

use hyper::body::{Body, Frame, SizeHint};

use super::*;

struct HeldBody;

impl Body for HeldBody {
    type Data = bytes::Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Pending
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(1)
    }
}

pub(super) struct Endpoint {
    pub base: String,
    pub transport: Arc<HttpTransport>,
    pub native_transport: Arc<wamn_http_capability::transport::HttpTransport>,
    pub arrived: tokio::sync::oneshot::Receiver<()>,
    pub closed: tokio::sync::oneshot::Receiver<()>,
    task: JoinHandle<()>,
}

impl Endpoint {
    pub async fn start() -> Self {
        let address = super::super::interface_address();
        let certificate = rcgen::generate_simple_self_signed(vec![address.to_string()]).unwrap();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut server = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(
                    certificate.signing_key.serialize_der(),
                )
                .into(),
            )
            .unwrap();
        server.alpn_protocols = vec![b"h2".to_vec()];
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate.cert.der().clone()).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let listener = TcpListener::bind((address, 0)).await.unwrap();
        let base = format!("https://{}/bound/", listener.local_addr().unwrap());
        let (notify, arrived) = tokio::sync::oneshot::channel();
        let (stopped, closed) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let socket = tokio_rustls::TlsAcceptor::from(Arc::new(server))
                .accept(socket)
                .await
                .unwrap();
            let notify = Mutex::new(Some(notify));
            let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(
                    TokioIo::new(socket),
                    service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        if let Some(notify) = notify.lock().unwrap().take() {
                            let _ = notify.send(());
                        }
                        async move {
                            request.into_body().collect().await.unwrap();
                            Ok::<_, Infallible>(hyper::Response::new(HeldBody))
                        }
                    }),
                )
                .await;
            let _ = stopped.send(());
        });
        Self {
            base,
            native_transport: Arc::new(
                wamn_http_capability::transport::HttpTransport::with_tls_config(client.clone()),
            ),
            transport: Arc::new(HttpTransport::with_tls_config(client)),
            arrived,
            closed,
            task,
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) fn guest(operation: &str) -> Vec<u8> {
    wat::parse_str(format!(r#"(component
        (type $context (record
            (field "wiring-id" string) (field "wiring-version" u32)
            (field "node-id" string) (field "delivery-id" string)
            (field "input-port" (option string)) (field "occurrence" u32)
            (field "traceparent" (option string)) (field "tracestate" (option string))
            (field "deadline-ms" (option u64)) (field "config" string)))
        (type $detail (record (field "message" string) (field "code" (option string))))
        (type $rate (record (field "detail" $detail) (field "retry-after-ms" (option u64))))
        (type $error (variant (case "retryable" $detail) (case "rate-limited" $rate)
            (case "terminal" $detail) (case "invalid-input" $detail) (case "cancelled")))
        (type $emission (record (field "payload" string) (field "port" (option string))))
        (import "wamn:connection/http@0.1.0" (instance $http
            (type $header' (record (field "name" string) (field "value" (list u8))))
            (export "header" (type $header (eq $header')))
            (type $request' (record (field "requirement" string) (field "method" string)
                (field "path-and-query" string) (field "headers" (list $header))
                (field "body" (option (list u8))) (field "idempotency-key" (option string))))
            (export "request" (type $request (eq $request')))
            (type $response' (record (field "external-system" (option string))
                (field "status" u16) (field "headers" (list $header)) (field "body" (list u8))))
            (export "response" (type $response (eq $response')))
            (type $error' (variant (case "unbound") (case "incompatible") (case "authority-denied")
                (case "attestation-invalid") (case "credential-unavailable") (case "timeout")
                (case "transport" string) (case "unauthorized")))
            (export "connection-error" (type $error (eq $error')))
            (export "send" (func async (param "request" $request) (result (result $response (error $error)))))))
        (core module $memory
            (memory (export "memory") 1)
            (data (i32.const 16) "handle") (data (i32.const 32) "POST") (data (i32.const 48) "/api")
            (global $heap (mut i32) (i32.const 4096))
            (func (export "realloc") (param i32 i32 i32) (param $len i32) (result i32)
                (local $ptr i32) global.get $heap local.tee $ptr
                local.get $len i32.add i32.const 7 i32.add i32.const -8 i32.and global.set $heap local.get $ptr))
        (core instance $memory (instantiate $memory))
        (core func $send (canon lower (func $http "send") (memory $memory "memory") (realloc (func $memory "realloc"))))
        (core module $code
            (import "host" "send" (func $send (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)))
            (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
            (func (export "run") (param i32) (result i32)
                i32.const 16 i32.const 6 i32.const 32 i32.const 4 i32.const 48 i32.const 4
                i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0
                i32.const 0 i32.const 0 i32.const 0 i32.const 256 call $send
                (loop br 0) unreachable))
        (core instance $code (instantiate $code
            (with "host" (instance (export "send" (func $send))))))
        (func $run async (param "context" $context) (param "input" string) (result (result $emission (error $error)))
            (canon lift (core func $code "run") (memory $memory "memory") (realloc (func $memory "realloc")) async (callback (func $code "callback"))))
        (instance $operation (export "node-context" (type $context))
            (export "error-detail" (type $detail)) (export "rate-limit-detail" (type $rate))
            (export "node-error" (type $error))
            (export "emission" (type $emission)) (export "run" (func $run)))
        (export "{operation}" (instance $operation)))"#)).unwrap()
}
