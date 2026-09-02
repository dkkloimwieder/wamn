//! Typed HTTP client for the production authoring Gate.

use std::error::Error;
use std::fmt;

use tokio::time::{Instant, timeout_at};
use url::Url;
use wamn_authoring_model::{
    AuthoringCommand, AuthoringDocument, AuthoringOutcome, AuthoringRequest,
    AuthoringRequestEnvelope, AuthoringResponseEnvelope, AuthoringSuccess, CommandRefusal, Gate,
    GateReceipt, GateRefusal, SCHEMA_VERSION, decode_document,
};

/// Stable category of an authoring Gate client failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateClientErrorKind {
    InvalidEndpoint,
    RequestEncoding,
    Transport,
    DeadlineExceeded,
    HttpStatus,
    MalformedResponse,
}

impl GateClientErrorKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "invalid-endpoint",
            Self::RequestEncoding => "request-encoding",
            Self::Transport => "transport",
            Self::DeadlineExceeded => "deadline-exceeded",
            Self::HttpStatus => "http-status",
            Self::MalformedResponse => "malformed-response",
        }
    }
}

/// Contextual failure from the production authoring Gate boundary.
#[derive(Debug)]
pub struct GateClientError {
    kind: GateClientErrorKind,
    gate_url: Box<str>,
    status: Option<u16>,
    detail: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl GateClientError {
    fn new(kind: GateClientErrorKind, gate_url: impl Into<Box<str>>, detail: &'static str) -> Self {
        Self {
            kind,
            gate_url: gate_url.into(),
            status: None,
            detail,
            source: None,
        }
    }

    fn with_source(
        kind: GateClientErrorKind,
        gate_url: impl Into<Box<str>>,
        detail: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            gate_url: gate_url.into(),
            status: None,
            detail,
            source: Some(Box::new(source)),
        }
    }

    fn status(gate_url: impl Into<Box<str>>, status: u16) -> Self {
        Self {
            kind: GateClientErrorKind::HttpStatus,
            gate_url: gate_url.into(),
            status: Some(status),
            detail: "Gate returned an untyped HTTP failure",
            source: None,
        }
    }

    /// Stable failure category.
    pub const fn kind(&self) -> GateClientErrorKind {
        self.kind
    }

    /// Credential-free Gate endpoint named by this failure.
    pub fn gate_url(&self) -> &str {
        &self.gate_url
    }

    /// HTTP status code for an untyped HTTP failure.
    pub const fn status_code(&self) -> Option<u16> {
        self.status
    }
}

impl fmt::Display for GateClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "authoring-gate-{} at {}",
            self.kind.as_str(),
            self.gate_url
        )?;
        if let Some(status) = self.status {
            write!(formatter, " (HTTP {status})")?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl Error for GateClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Authenticated client for one configured production authoring Gate endpoint.
pub struct GateClient {
    http: reqwest::Client,
    gate_url: Url,
    sanitized_gate_url: Box<str>,
    bearer_token: Box<str>,
}

impl fmt::Debug for GateClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GateClient")
            .field("gate_url", &self.sanitized_gate_url)
            .field("bearer_token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl GateClient {
    /// Build a client for the configured `/authoring` endpoint and bearer token.
    pub fn new(gate_url: &str, bearer_token: &str) -> Result<Self, GateClientError> {
        let parsed = Url::parse(gate_url).map_err(|source| {
            GateClientError::with_source(
                GateClientErrorKind::InvalidEndpoint,
                "<malformed>",
                "configured Gate URL is malformed",
                source,
            )
        })?;
        let sanitized_gate_url = sanitized_url(&parsed).into_boxed_str();
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(GateClientError::new(
                GateClientErrorKind::InvalidEndpoint,
                sanitized_gate_url,
                "configured Gate URL must use HTTP or HTTPS and name a host",
            ));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| {
                GateClientError::with_source(
                    GateClientErrorKind::Transport,
                    sanitized_gate_url.clone(),
                    "build the Gate HTTP client",
                    source.without_url(),
                )
            })?;
        Ok(Self {
            http,
            gate_url: parsed,
            sanitized_gate_url,
            bearer_token: bearer_token.into(),
        })
    }

    /// Submit one exact Gate command before the caller's shared endpoint deadline.
    pub async fn submit(
        &self,
        command_id: &str,
        gate: Gate,
        deadline: Instant,
    ) -> Result<Result<GateReceipt, GateRefusal>, GateClientError> {
        let request = AuthoringDocument::Request(Box::new(AuthoringRequestEnvelope::Command(
            AuthoringRequest {
                schema_version: SCHEMA_VERSION.to_owned(),
                command_id: command_id.to_owned(),
                command: AuthoringCommand::Gate(gate),
            },
        )));
        let body = serde_json::to_vec(&request).map_err(|source| {
            GateClientError::with_source(
                GateClientErrorKind::RequestEncoding,
                self.sanitized_gate_url.clone(),
                "encode the Gate request contract",
                source,
            )
        })?;

        let exchange = async {
            let response = self
                .http
                .post(self.gate_url.clone())
                .bearer_auth(&self.bearer_token)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await
                .map_err(|source| self.transport("send the Gate request", source))?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|source| self.transport("read the Gate response", source))?;
            Ok::<_, GateClientError>((status, body))
        };
        let (status, body) = timeout_at(deadline, exchange).await.map_err(|_| {
            GateClientError::new(
                GateClientErrorKind::DeadlineExceeded,
                self.sanitized_gate_url.clone(),
                "Gate HTTP exchange exceeded its deadline",
            )
        })??;

        if !status.is_success() {
            return Err(GateClientError::status(
                self.sanitized_gate_url.clone(),
                status.as_u16(),
            ));
        }
        decode_gate_response(&body, command_id, &self.sanitized_gate_url)
    }

    fn transport(&self, detail: &'static str, source: reqwest::Error) -> GateClientError {
        GateClientError::with_source(
            GateClientErrorKind::Transport,
            self.sanitized_gate_url.clone(),
            detail,
            source.without_url(),
        )
    }
}

fn decode_gate_response(
    body: &[u8],
    command_id: &str,
    gate_url: &str,
) -> Result<Result<GateReceipt, GateRefusal>, GateClientError> {
    let text = std::str::from_utf8(body).map_err(|source| {
        GateClientError::with_source(
            GateClientErrorKind::MalformedResponse,
            gate_url,
            "Gate response is not UTF-8 JSON",
            source,
        )
    })?;
    let document = decode_document(text).map_err(|source| {
        GateClientError::with_source(
            GateClientErrorKind::MalformedResponse,
            gate_url,
            "Gate response is not a supported authoring contract document",
            source,
        )
    })?;
    let AuthoringDocument::Response(response) = document else {
        return Err(malformed(gate_url, "Gate returned a request document"));
    };
    let AuthoringResponseEnvelope::Command(response) = response.as_ref() else {
        return Err(malformed(gate_url, "Gate returned a query response"));
    };
    if response.command_id != command_id {
        return Err(malformed(
            gate_url,
            "Gate response command identity does not match the request",
        ));
    }
    match &response.outcome {
        AuthoringOutcome::Completed(success) => match success.as_ref() {
            AuthoringSuccess::Gate(receipt) => Ok(Ok(receipt.clone())),
            AuthoringSuccess::Publish(_) => {
                Err(malformed(gate_url, "Gate returned a publish receipt"))
            }
        },
        AuthoringOutcome::Refused(refusal) => match refusal {
            CommandRefusal::Gate(refusal) => Ok(Err(refusal.clone())),
            CommandRefusal::Publish(_) => {
                Err(malformed(gate_url, "Gate returned a publish refusal"))
            }
        },
    }
}

fn malformed(gate_url: &str, detail: &'static str) -> GateClientError {
    GateClientError::new(GateClientErrorKind::MalformedResponse, gate_url, detail)
}

fn sanitized_url(url: &Url) -> String {
    let Some(host) = url.host_str() else {
        return "<malformed>".to_owned();
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = url
        .port_or_known_default()
        .map_or(String::new(), |port| format!(":{port}"));
    format!("{}://{host}{port}{}", url.scheme(), url.path())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;
    use wamn_authoring_model::{AuthoringScope, ValidatedDraftRef};

    use super::*;

    #[derive(Debug)]
    struct CapturedRequest {
        request_line: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }

    fn gate() -> Gate {
        Gate {
            scope: AuthoringScope {
                project_id: "receiving".to_owned(),
                environment: "dev".to_owned(),
            },
            package_id: "receiving".to_owned(),
            package_version: "1.2.3".to_owned(),
            document: json!({
                "wiring-id": "receiving",
                "entry": "receive",
                "nodes": [{"id": "receive", "component": "sha256:abc"}],
            }),
        }
    }

    async fn loopback(
        status: &'static str,
        response_body: &'static [u8],
    ) -> (String, JoinHandle<CapturedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback server");
        let address = listener.local_addr().expect("read loopback address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept Gate request");
            let request = read_request(&mut stream).await;
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            stream
                .write_all(head.as_bytes())
                .await
                .expect("write response head");
            stream
                .write_all(response_body)
                .await
                .expect("write response body");
            request
        });
        (format!("http://{address}/authoring"), task)
    }

    async fn read_request(stream: &mut TcpStream) -> CapturedRequest {
        let mut bytes = Vec::new();
        let (header_end, content_length) = loop {
            let mut chunk = [0_u8; 1024];
            let count = stream.read(&mut chunk).await.expect("read Gate request");
            assert_ne!(count, 0, "request ended before its complete body");
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let head =
                    std::str::from_utf8(&bytes[..header_end]).expect("request head is UTF-8");
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length").then(|| {
                            value
                                .trim()
                                .parse::<usize>()
                                .expect("numeric content length")
                        })
                    })
                    .expect("request carries a content length");
                if bytes.len() >= header_end + 4 + content_length {
                    break (header_end, content_length);
                }
            }
        };
        let head = std::str::from_utf8(&bytes[..header_end]).expect("request head is UTF-8");
        let mut lines = head.lines();
        let request_line = lines.next().expect("request line").to_owned();
        let headers = lines
            .map(|line| {
                let (name, value) = line.split_once(':').expect("header has a colon");
                (name.to_ascii_lowercase(), value.trim().to_owned())
            })
            .collect();
        CapturedRequest {
            request_line,
            headers,
            body: bytes[header_end + 4..header_end + 4 + content_length].to_vec(),
        }
    }

    #[tokio::test]
    async fn posts_the_exact_gate_contract_with_bearer_auth() {
        let response = br#"{"document":"response","body":{"schema-version":"0.1","command-id":"gate-command-1","outcome":{"status":"completed","value":{"command":"gate","result":{"report-id":"sha256:abc","validated-draft":{"validated-draft-id":"sha256:abc"}}}}}}"#;
        let (url, server) = loopback("200 OK", response).await;
        let client = GateClient::new(&url, "gate-super-secret").expect("build Gate client");

        let outcome = client
            .submit(
                "gate-command-1",
                gate(),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("Gate exchange succeeds");

        assert_eq!(
            outcome,
            Ok(GateReceipt {
                report_id: "sha256:abc".to_owned(),
                validated_draft: ValidatedDraftRef {
                    validated_draft_id: "sha256:abc".to_owned(),
                },
            })
        );
        let request = server.await.expect("join loopback server");
        assert_eq!(request.request_line, "POST /authoring HTTP/1.1");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer gate-super-secret")
        );
        assert_eq!(
            request.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(
            request.body,
            br#"{"document":"request","body":{"schema-version":"0.1","command-id":"gate-command-1","command":{"kind":"gate","input":{"scope":{"project-id":"receiving","environment":"dev"},"package-id":"receiving","package-version":"1.2.3","document":{"entry":"receive","nodes":[{"component":"sha256:abc","id":"receive"}],"wiring-id":"receiving"}}}}}"#
        );
    }

    #[tokio::test]
    async fn returns_the_contracts_gate_refusal_without_redefining_it() {
        let response = br#"{"document":"response","body":{"schema-version":"0.1","command-id":"gate-command-2","outcome":{"status":"refused","value":{"command":"gate","reason":{"kind":"invalid-document","detail":"bad wiring"}}}}}"#;
        let (url, server) = loopback("200 OK", response).await;
        let client = GateClient::new(&url, "gate-secret").expect("build Gate client");

        let outcome = client
            .submit(
                "gate-command-2",
                gate(),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("typed refusal is a completed exchange");

        assert_eq!(
            outcome,
            Err(GateRefusal::InvalidDocument {
                detail: "bad wiring".to_owned(),
            })
        );
        server.await.expect("join loopback server");
    }

    #[tokio::test]
    async fn keeps_a_pre_dispatch_http_refusal_distinct_from_a_gate_judgment() {
        let (url, server) = loopback("403 Forbidden", br#"{"kind":"authorization-denied"}"#).await;
        let client = GateClient::new(&url, "rejected-secret").expect("build Gate client");

        let error = client
            .submit(
                "gate-command-authorization",
                gate(),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect_err("pre-dispatch refusal is not a Gate judgment");

        assert_eq!(error.kind(), GateClientErrorKind::HttpStatus);
        assert_eq!(error.status_code(), Some(403));
        assert!(!format!("{error:?} {error}").contains("rejected-secret"));
        server.await.expect("join loopback server");
    }

    #[tokio::test]
    async fn rejects_a_malformed_gate_response() {
        let (url, server) = loopback("200 OK", br#"{"status":"completed"}"#).await;
        let client = GateClient::new(&url, "gate-secret").expect("build Gate client");

        let error = client
            .submit(
                "gate-command-3",
                gate(),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect_err("non-contract response must fail");

        assert_eq!(error.kind(), GateClientErrorKind::MalformedResponse);
        assert_eq!(error.gate_url(), url);
        server.await.expect("join loopback server");
    }

    #[tokio::test]
    async fn rejects_a_wrong_command_echo_or_non_gate_outcome() {
        for response in [
            br#"{"document":"response","body":{"schema-version":"0.1","command-id":"other-command","outcome":{"status":"completed","value":{"command":"gate","result":{"report-id":"sha256:abc","validated-draft":{"validated-draft-id":"sha256:abc"}}}}}}"#.as_slice(),
            br#"{"document":"response","body":{"schema-version":"0.1","command-id":"gate-command-exactness","outcome":{"status":"completed","value":{"command":"publish","result":{"wiring-id":"receiving","version":1,"artifact-hash":"sha256:abc"}}}}}"#.as_slice(),
        ] {
            let (url, server) = loopback("200 OK", response).await;
            let client = GateClient::new(&url, "gate-secret").expect("build Gate client");

            let error = client
                .submit(
                    "gate-command-exactness",
                    gate(),
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect_err("response must echo this Gate command exactly");

            assert_eq!(error.kind(), GateClientErrorKind::MalformedResponse);
            server.await.expect("join loopback server");
        }
    }

    #[tokio::test]
    async fn one_deadline_bounds_the_whole_http_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stalled server");
        let address = listener.local_addr().expect("read stalled address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept Gate request");
            let _request = read_request(&mut stream).await;
            std::future::pending::<()>().await;
        });
        let url = format!("http://{address}/authoring");
        let client = GateClient::new(&url, "deadline-secret").expect("build Gate client");

        let error = client
            .submit(
                "gate-command-4",
                gate(),
                Instant::now() + Duration::from_millis(25),
            )
            .await
            .expect_err("stalled Gate must time out");

        assert_eq!(error.kind(), GateClientErrorKind::DeadlineExceeded);
        assert_eq!(error.gate_url(), url);
        assert!(!format!("{error:?} {error}").contains("deadline-secret"));
        server.abort();
    }

    #[tokio::test]
    async fn transport_diagnostics_sanitize_the_url_and_never_leak_the_token() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve unused address");
        let address = listener.local_addr().expect("read unused address");
        drop(listener);
        let raw_url = format!(
            "http://url-user:url-secret@{address}/authoring?credential=query-secret#fragment-secret"
        );
        let sanitized = format!("http://{address}/authoring");
        let client = GateClient::new(&raw_url, "bearer-super-secret").expect("build Gate client");

        let error = client
            .submit(
                "gate-command-5",
                gate(),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect_err("closed loopback port must refuse transport");

        assert_eq!(error.kind(), GateClientErrorKind::Transport);
        assert_eq!(error.gate_url(), sanitized);
        let diagnostic = format!("{error:?} {error}");
        for secret in [
            "bearer-super-secret",
            "url-user",
            "url-secret",
            "query-secret",
            "fragment-secret",
        ] {
            assert!(
                !diagnostic.contains(secret),
                "leaked {secret:?}: {diagnostic}"
            );
        }
        assert!(diagnostic.contains(&sanitized));
    }
}
