//! Operator-authenticated HTTPS transport for PAT issuance.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use chrono::DateTime;
use clap::Args;
use serde::Deserialize;
use url::Url;
use wamn_platform_identity::{PAT_TOKEN_PREFIX, PrincipalId};

/// The provisioning request stops after five seconds and never retries issuance.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// A PAT response contains five small fields. Larger bodies are refused.
const MAX_RESPONSE_BYTES: usize = 4096;
/// A lost response does not mean that the service refused to create the PAT.
const UNCERTAIN_ISSUANCE: &str =
    "A PAT can exist without a returned token. The request was not retried.";

/// TLS credentials and the identity service endpoint for operator PAT issuance.
#[derive(Clone, Default, Args)]
pub struct PatIssuerArgs {
    /// HTTPS base URL of the identity service. Its path is preserved.
    #[arg(long = "pat-issuer", env = "WAMN_PAT_ISSUER", hide_env_values = true)]
    pub endpoint: Option<String>,

    /// PEM certificate chain for the provisioning operator.
    #[arg(
        long = "pat-client-cert",
        env = "WAMN_PAT_CLIENT_CERT",
        hide_env_values = true
    )]
    pub client_cert: Option<PathBuf>,

    /// PEM private key for the provisioning operator.
    #[arg(
        long = "pat-client-key",
        env = "WAMN_PAT_CLIENT_KEY",
        hide_env_values = true
    )]
    pub client_key: Option<PathBuf>,

    /// PEM roots that replace the default trust roots for this identity service.
    #[arg(
        long = "pat-server-ca",
        env = "WAMN_PAT_SERVER_CA",
        hide_env_values = true
    )]
    pub server_ca: Option<PathBuf>,
}

impl fmt::Debug for PatIssuerArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PatIssuerArgs")
            .field("endpoint", &self.endpoint.as_ref().map(|_| "<redacted>"))
            .field(
                "client_cert",
                &self.client_cert.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "client_key",
                &self.client_key.as_ref().map(|_| "<redacted>"),
            )
            .field("server_ca", &self.server_ca.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

pub(crate) struct PatClient {
    http: reqwest::Client,
    endpoint: Url,
}

impl fmt::Debug for PatClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("PatClient").finish_non_exhaustive()
    }
}

impl PatClient {
    pub(crate) fn new(args: &PatIssuerArgs) -> anyhow::Result<Self> {
        let endpoint = pat_endpoint(
            args.endpoint
                .as_deref()
                .context("PAT issuance requires --pat-issuer")?,
        )?;
        let cert_path = args
            .client_cert
            .as_deref()
            .context("PAT issuance requires --pat-client-cert")?;
        let key_path = args
            .client_key
            .as_deref()
            .context("PAT issuance requires --pat-client-key")?;

        // Upstream errors can contain paths, URLs, or certificate material.
        // This credential boundary exposes only fixed diagnostic text.
        let mut identity_pem = std::fs::read(cert_path)
            .map_err(|_| anyhow::anyhow!("PAT client certificate read failed"))?;
        identity_pem.push(b'\n');
        identity_pem.extend(
            std::fs::read(key_path)
                .map_err(|_| anyhow::anyhow!("PAT client private key read failed"))?,
        );
        let identity = reqwest::Identity::from_pem(&identity_pem)
            .map_err(|_| anyhow::anyhow!("PAT client certificate or private key is invalid"))?;
        let mut builder = reqwest::Client::builder()
            .tls_backend_rustls()
            .identity(identity)
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(REQUEST_TIMEOUT);
        if let Some(path) = args.server_ca.as_deref() {
            let pem = std::fs::read(path)
                .map_err(|_| anyhow::anyhow!("PAT server trust roots read failed"))?;
            let roots = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|_| anyhow::anyhow!("PAT server trust roots are invalid"))?;
            anyhow::ensure!(!roots.is_empty(), "PAT server trust roots are empty");
            builder = builder.tls_certs_only(roots);
        }
        let http = builder
            .build()
            .map_err(|_| anyhow::anyhow!("PAT HTTPS client creation failed"))?;
        Ok(Self { http, endpoint })
    }

    pub(crate) async fn issue(
        &self,
        principal_id: &PrincipalId,
        label: &str,
        lifetime: Duration,
    ) -> anyhow::Result<PatResponse> {
        let body = serde_json::to_vec(&serde_json::json!({
            "principal_id": principal_id.as_str(),
            "label": label,
            "lifetime_seconds": lifetime.as_secs(),
        }))
        .map_err(|_| anyhow::anyhow!("PAT request encoding failed"))?;
        let mut response = self
            .http
            .post(self.endpoint.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("PAT issuance transport failed. {UNCERTAIN_ISSUANCE}"))?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::CREATED,
            "PAT issuer returned an unsuccessful response. {UNCERTAIN_ISSUANCE}"
        );
        anyhow::ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MAX_RESPONSE_BYTES as u64),
            "PAT issuer response exceeds the size limit. {UNCERTAIN_ISSUANCE}"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("PAT issuer response read failed. {UNCERTAIN_ISSUANCE}"))?
        {
            anyhow::ensure!(
                chunk.len() <= MAX_RESPONSE_BYTES - body.len(),
                "PAT issuer response exceeds the size limit. {UNCERTAIN_ISSUANCE}"
            );
            body.extend_from_slice(&chunk);
        }
        decode_response(&body, principal_id, lifetime).context(UNCERTAIN_ISSUANCE)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatResponse {
    pub(crate) token: String,
    pub(crate) token_prefix: String,
    principal_id: String,
    created_at: String,
    pub(crate) expires_at: String,
}

impl fmt::Debug for PatResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PatResponse")
            .finish_non_exhaustive()
    }
}

fn pat_endpoint(value: &str) -> anyhow::Result<Url> {
    let mut endpoint = Url::parse(value).map_err(|_| {
        anyhow::anyhow!("PAT issuer must be an HTTPS URL without credentials, query, or fragment")
    })?;
    anyhow::ensure!(
        endpoint.scheme() == "https"
            && endpoint.host_str().is_some()
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "PAT issuer must be an HTTPS URL without credentials, query, or fragment"
    );
    let path = format!("{}/pats", endpoint.path().trim_end_matches('/'));
    endpoint.set_path(&path);
    Ok(endpoint)
}

fn decode_response(
    body: &[u8],
    principal_id: &PrincipalId,
    lifetime: Duration,
) -> anyhow::Result<PatResponse> {
    anyhow::ensure!(
        body.len() <= MAX_RESPONSE_BYTES,
        "PAT issuer response exceeds the size limit"
    );
    let response: PatResponse = serde_json::from_slice(body)
        .map_err(|_| anyhow::anyhow!("PAT issuer response is invalid"))?;
    let created_at = DateTime::parse_from_rfc3339(&response.created_at)
        .map_err(|_| anyhow::anyhow!("PAT issuer response metadata is invalid"))?;
    let expires_at = DateTime::parse_from_rfc3339(&response.expires_at)
        .map_err(|_| anyhow::anyhow!("PAT issuer response metadata is invalid"))?;
    let token_parts = response
        .token
        .strip_prefix(PAT_TOKEN_PREFIX)
        .and_then(|token| token.split_once('_'));
    // Identity tokens carry a 16-hex lookup prefix and a 64-hex secret.
    // The caller also authenticates the full token against the system database.
    let token_matches = token_parts.is_some_and(|(prefix, secret)| {
        prefix == response.token_prefix
            && prefix.len() == 16
            && secret.len() == 64
            && prefix
                .bytes()
                .chain(secret.bytes())
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    });
    anyhow::ensure!(
        response.principal_id == principal_id.as_str()
            && token_matches
            && expires_at.signed_duration_since(created_at).to_std().ok() == Some(lifetime),
        "PAT issuer response metadata is invalid"
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio_rustls::{TlsAcceptor, rustls};

    const PRINCIPAL: &str = "6d3f2d1c-0000-4000-8000-00000000abcd";
    const PREFIX: &str = "0123456789abcdef";
    const LIFETIME: Duration = Duration::from_secs(2_592_000);

    fn response() -> serde_json::Value {
        serde_json::json!({
            "token": format!("wamn_pat_{PREFIX}_{}", "a".repeat(64)),
            "token_prefix": PREFIX,
            "principal_id": PRINCIPAL,
            "created_at": "2026-09-10T12:34:56.123456Z",
            "expires_at": "2026-10-10T12:34:56.123456Z",
        })
    }

    #[test]
    fn issuer_path_is_preserved() {
        for (base, expected) in [
            ("https://identity.example", "https://identity.example/pats"),
            (
                "https://identity.example/authority",
                "https://identity.example/authority/pats",
            ),
            (
                "https://identity.example/authority/",
                "https://identity.example/authority/pats",
            ),
        ] {
            assert_eq!(pat_endpoint(base).unwrap().as_str(), expected);
        }
    }

    #[test]
    fn unsafe_endpoints_refuse_without_exposing_input() {
        for input in [
            "http://identity.example/private-marker",
            "https://private-marker@identity.example",
            "https://user:private-marker@identity.example",
            "https://identity.example?private-marker",
            "https://identity.example#private-marker",
            "private-marker",
        ] {
            let error = pat_endpoint(input).unwrap_err();
            assert!(!format!("{error:#} {error:?}").contains("private-marker"));
        }
    }

    #[test]
    fn issuer_credentials_are_required_and_debug_is_redacted() {
        assert!(PatClient::new(&PatIssuerArgs::default()).is_err());
        let mut args = PatIssuerArgs {
            endpoint: Some("https://identity.example/private-marker".to_owned()),
            ..Default::default()
        };
        assert!(PatClient::new(&args).is_err());
        args.client_cert = Some(PathBuf::from("/private-marker/operator.pem"));
        assert!(PatClient::new(&args).is_err());
        args.client_key = Some(PathBuf::from("/private-marker/operator.key"));
        args.server_ca = Some(PathBuf::from("/private-marker/roots.pem"));
        let error = PatClient::new(&args).unwrap_err();
        assert!(!format!("{args:?} {error:#} {error:?}").contains("private-marker"));
    }

    #[test]
    fn exact_response_accepts_and_redacts_the_token() {
        let input = response();
        let result = decode_response(
            &serde_json::to_vec(&input).unwrap(),
            &PRINCIPAL.parse().unwrap(),
            LIFETIME,
        )
        .unwrap();
        assert_eq!(result.token, input["token"].as_str().unwrap());
        assert_eq!(result.token_prefix, PREFIX);
        assert!(!format!("{result:?}").contains(result.token.as_str()));
    }

    #[test]
    fn malformed_or_mismatched_response_refuses_without_secret_details() {
        for (field, value) in [
            ("principal_id", "6d3f2d1c-0000-4000-8000-00000000abce"),
            ("token_prefix", "fedcba9876543210"),
            ("token", "private-marker"),
            ("created_at", "private-marker"),
            ("expires_at", "2026-10-10T12:34:57.123456Z"),
            ("unknown", "private-marker"),
        ] {
            let mut input = response();
            input[field] = value.into();
            let error = decode_response(
                &serde_json::to_vec(&input).unwrap(),
                &PRINCIPAL.parse().unwrap(),
                LIFETIME,
            )
            .unwrap_err();
            assert!(!format!("{error:#} {error:?}").contains("private-marker"));
        }
        let duplicate = format!(
            "{{\"token\":\"private-marker\",{}",
            &response().to_string()[1..]
        );
        assert!(
            decode_response(duplicate.as_bytes(), &PRINCIPAL.parse().unwrap(), LIFETIME).is_err()
        );
        assert!(
            decode_response(
                &vec![b'a'; MAX_RESPONSE_BYTES + 1],
                &PRINCIPAL.parse().unwrap(),
                LIFETIME
            )
            .is_err()
        );
    }

    struct HttpsFixture {
        args: PatIssuerArgs,
        requests: Arc<AtomicUsize>,
        received: tokio::sync::mpsc::UnboundedReceiver<String>,
        task: tokio::task::JoinHandle<()>,
        directory: PathBuf,
    }

    impl Drop for HttpsFixture {
        fn drop(&mut self) {
            self.task.abort();
            std::fs::remove_dir_all(&self.directory).expect("remove isolated TLS fixture");
        }
    }

    async fn https_fixture(
        server_name: &str,
        response: impl FnOnce(&str) -> String,
    ) -> HttpsFixture {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "wamn-pat-client-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .expect("isolated TLS fixture");

        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_key = KeyPair::generate().unwrap();
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let server_key = KeyPair::generate().unwrap();
        let server = CertificateParams::new(vec![server_name.to_owned()])
            .unwrap()
            .signed_by(&server_key, &issuer)
            .unwrap();
        let client_key = KeyPair::generate().unwrap();
        let client = CertificateParams::new(vec!["operator.example".to_owned()])
            .unwrap()
            .signed_by(&client_key, &issuer)
            .unwrap();
        let write_pem = |name: &str, pem: &str| {
            let path = directory.join(name);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .expect("fixture credential file");
            file.write_all(pem.as_bytes())
                .expect("fixture credential contents");
            path
        };
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::clone(&provider),
        )
        .build()
        .unwrap();
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![server.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
            )
            .unwrap();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("https://{}/identity", listener.local_addr().unwrap());
        let args = PatIssuerArgs {
            endpoint: Some(endpoint.clone()),
            client_cert: Some(write_pem("operator.pem", &client.pem())),
            client_key: Some(write_pem("operator.key", &client_key.serialize_pem())),
            server_ca: Some(write_pem("ca.pem", &ca.pem())),
        };
        let response = response(&endpoint);
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let (sender, received) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    continue;
                };
                assert!(stream.get_ref().1.peer_certificates().is_some());
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                let headers_end = loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0 && request.len() + read <= 4096);
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(offset) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                    {
                        break offset + 4;
                    }
                };
                let headers = std::str::from_utf8(&request[..headers_end]).unwrap();
                let body_len = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while request.len() < headers_end + body_len {
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0 && request.len() + read <= 4096);
                    request.extend_from_slice(&buffer[..read]);
                }
                request_count.fetch_add(1, Ordering::SeqCst);
                sender.send(String::from_utf8(request).unwrap()).unwrap();
                if !response.is_empty() {
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                }
            }
        });
        HttpsFixture {
            args,
            requests,
            received,
            task,
            directory,
        }
    }

    #[tokio::test]
    async fn https_issuance_uses_operator_certificate_and_exact_request() {
        let mut fixture = https_fixture("127.0.0.1", |_| {
            let body = response().to_string();
            format!("HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        }).await;
        let client = PatClient::new(&fixture.args).unwrap();
        let result = client
            .issue(&PRINCIPAL.parse().unwrap(), "route-caller", LIFETIME)
            .await
            .unwrap();
        assert_eq!(result.token_prefix, PREFIX);
        let request = fixture.received.recv().await.unwrap();
        assert!(request.starts_with("POST /identity/pats HTTP/1.1\r\n"));
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap(),
            serde_json::json!({
                "principal_id": PRINCIPAL, "label": "route-caller", "lifetime_seconds": 2_592_000,
            })
        );
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        assert!(!format!("{client:?}").contains("/identity"));
    }

    #[tokio::test]
    async fn https_issuance_refuses_wrong_server_certificate() {
        let fixture = https_fixture("wrong.example", |_| String::new()).await;
        let error = PatClient::new(&fixture.args)
            .unwrap()
            .issue(&PRINCIPAL.parse().unwrap(), "route-caller", LIFETIME)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("transport failed"));
        assert!(error.to_string().contains(UNCERTAIN_ISSUANCE));
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn https_issuance_does_not_redirect_retry_or_accept_large_bodies() {
        for response_kind in [
            "redirect",
            "unavailable",
            "disconnect",
            "truncated",
            "length",
            "chunked",
        ] {
            let fixture = https_fixture("127.0.0.1", |endpoint| match response_kind {
                "redirect" => format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {endpoint}/private-marker\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                "unavailable" => "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                "disconnect" => String::new(),
                "truncated" => "HTTP/1.1 201 Created\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"token\":\"private-marker".to_owned(),
                "length" => "HTTP/1.1 201 Created\r\nContent-Length: 4097\r\nConnection: close\r\n\r\n".to_owned(),
                "chunked" => format!("HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1000\r\n{}\r\n1\r\na\r\n0\r\n\r\n", "a".repeat(4096)),
                _ => unreachable!(),
            }).await;
            let error = PatClient::new(&fixture.args)
                .unwrap()
                .issue(&PRINCIPAL.parse().unwrap(), "route-caller", LIFETIME)
                .await
                .unwrap_err();
            assert_eq!(
                fixture.requests.load(Ordering::SeqCst),
                1,
                "{response_kind}"
            );
            assert!(!format!("{error:#} {error:?}").contains("private-marker"));
            assert!(
                error.to_string().contains(UNCERTAIN_ISSUANCE),
                "{response_kind}"
            );
            if response_kind == "truncated" {
                assert_eq!(
                    error.to_string(),
                    "PAT issuer response read failed. A PAT can exist without a returned token. The request was not retried."
                );
            }
            if response_kind == "disconnect" {
                assert_eq!(
                    error.to_string(),
                    "PAT issuance transport failed. A PAT can exist without a returned token. The request was not retried."
                );
            }
            if matches!(response_kind, "length" | "chunked") {
                assert!(error.to_string().contains("size limit"));
            }
        }
    }
}
