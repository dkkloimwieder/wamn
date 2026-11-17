//! The operator client of the identity service: operator-authenticated
//! HTTPS for user creation and invitations from control.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use url::Url;
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::wire::{
    InvitationRequest, UserRequest, UserResponse as UserBody,
};
use wamn_oci::package_artifact::http_client::{HttpClient, Redirects};

/// Each identity request stops after five seconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Identity responses contain small user and invitation results.
const MAX_RESPONSE_BYTES: usize = 4096;
/// TLS credentials and the identity service endpoint for operator user management.
#[derive(Clone, Default)]
pub struct IdentityClientConfig {
    /// HTTPS base URL of the identity service. Its path is preserved.
    pub endpoint: Option<String>,

    /// PEM certificate chain for the provisioning operator.
    pub client_cert: Option<PathBuf>,

    /// PEM private key for the provisioning operator.
    pub client_key: Option<PathBuf>,

    /// PEM roots that replace the default trust roots for this identity service.
    pub server_ca: Option<PathBuf>,
}

impl fmt::Debug for IdentityClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityClientConfig")
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

/// The operator HTTPS client of one identity route.
struct IdentityClient {
    http: HttpClient,
    endpoint: Url,
}

impl fmt::Debug for IdentityClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("IdentityClient").finish_non_exhaustive()
    }
}

impl IdentityClient {
    /// The operator client for one route of the identity service.
    fn for_route(args: &IdentityClientConfig, route: &str) -> anyhow::Result<Self> {
        let endpoint = service_endpoint(
            args.endpoint
                .as_deref()
                .context("identity client endpoint is missing")?,
            route,
        )?;
        let cert_path = args
            .client_cert
            .as_deref()
            .context("identity client certificate is missing")?;
        let key_path = args
            .client_key
            .as_deref()
            .context("identity client private key is missing")?;

        // Credential errors can contain paths or certificate material.
        // Keep their diagnostics fixed. The transport never records request URLs.
        let mut identity_pem = std::fs::read(cert_path)
            .map_err(|_| anyhow::anyhow!("identity client certificate read failed"))?;
        identity_pem.push(b'\n');
        identity_pem.extend(
            std::fs::read(key_path)
                .map_err(|_| anyhow::anyhow!("identity client private key read failed"))?,
        );
        let mut builder = HttpClient::builder()
            .https_only(true)
            .redirects(Redirects::None)
            .timeout(REQUEST_TIMEOUT);
        if let Some(path) = args.server_ca.as_deref() {
            let pem = std::fs::read(path)
                .map_err(|_| anyhow::anyhow!("identity server trust roots read failed"))?;
            builder = builder
                .roots(&pem)
                .map_err(|_| anyhow::anyhow!("identity server trust roots are invalid"))?;
        }
        let http = builder
            .identity_pem(&identity_pem)
            .map_err(|_| anyhow::anyhow!("identity client certificate or private key is invalid"))?
            .build()
            .context("identity HTTPS client creation failed")?;
        Ok(Self { http, endpoint })
    }

}

fn service_endpoint(value: &str, route: &str) -> anyhow::Result<Url> {
    let mut endpoint = Url::parse(value).map_err(|_| {
        anyhow::anyhow!("identity endpoint must be an HTTPS URL without credentials, query, or fragment")
    })?;
    anyhow::ensure!(
        endpoint.scheme() == "https"
            && endpoint.host_str().is_some()
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "identity endpoint must be an HTTPS URL without credentials, query, or fragment"
    );
    let path = format!("{}/{route}", endpoint.path().trim_end_matches('/'));
    endpoint.set_path(&path);
    Ok(endpoint)
}

/// Identity's reply to one operator invitation. Identity mails the
/// invitation secret, so the reply carries no token.
#[derive(Debug)]
pub struct InvitationReply {
    /// The HTTP status: 201 when identity accepted the mail for delivery.
    pub status: u16,
    /// The reply body, for example `{"status":"accepted_for_delivery"}`.
    pub body: String,
}

/// The invitation request body for one user principal.
fn invitation_body(principal_id: &PrincipalId) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(&InvitationRequest {
        principal_id: *principal_id,
    })
    .map_err(|_| anyhow::anyhow!("invitation request encoding failed"))
}

/// Ask identity to mail an invitation to one user principal, as the operator
/// of `config`. The request is not retried.
pub async fn send_invitation(
    config: &IdentityClientConfig,
    principal_id: &PrincipalId,
) -> anyhow::Result<InvitationReply> {
    let client = IdentityClient::for_route(config, "invitations")?;
    let response = client
        .http
        .post(client.endpoint.clone())
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(invitation_body(principal_id)?)
        .send()
        .await
        .context("invitation transport failed; the request was not retried")?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .context("invitation reply read failed")?;
    anyhow::ensure!(
        bytes.len() <= MAX_RESPONSE_BYTES,
        "invitation reply exceeds the size limit"
    );
    Ok(InvitationReply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// The user identity created or reused for one email.
#[derive(Debug)]
pub struct UserReply {
    /// The user principal.
    pub principal_id: PrincipalId,
    /// Whether the user has a password credential.
    pub enrolled: bool,
}

#[derive(Deserialize)]
struct RefusalBody {
    error: String,
}

/// Identity refused the user, with identity's reason: for example an email
/// that the identity rules refuse, or a disabled user.
#[derive(Debug)]
pub struct UserRefused(pub String);

impl fmt::Display for UserRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "identity refused the user: {}", self.0)
    }
}

impl std::error::Error for UserRefused {}

/// The request body of one user.
fn user_body(email: &str, display_name: &str) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(&UserRequest {
        email,
        display_name,
    })
    .map_err(|_| anyhow::anyhow!("user request encoding failed"))
}

/// Ask identity to create the user of `email`, or to reuse the user that has
/// it, as the operator of `config`. A refusal is a [`UserRefused`] with
/// identity's reason. The request is not retried, and a repeated request
/// reuses the user.
pub async fn create_user(
    config: &IdentityClientConfig,
    email: &str,
    display_name: &str,
) -> anyhow::Result<UserReply> {
    let client = IdentityClient::for_route(config, "users")?;
    let response = client
        .http
        .post(client.endpoint.clone())
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(user_body(email, display_name)?)
        .send()
        .await
        .context("user request transport failed; the request was not retried")?;
    let status = response.status();
    let bytes = response.bytes().await.context("user reply read failed")?;
    anyhow::ensure!(
        bytes.len() <= MAX_RESPONSE_BYTES,
        "user reply exceeds the size limit"
    );
    if matches!(
        status,
        hyper::StatusCode::BAD_REQUEST | hyper::StatusCode::CONFLICT
    ) && let Ok(body) = serde_json::from_slice::<RefusalBody>(&bytes)
    {
        return Err(UserRefused(body.error).into());
    }
    anyhow::ensure!(
        status == hyper::StatusCode::OK,
        "identity did not create the user: {status}"
    );
    let body: UserBody =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("user reply is invalid"))?;
    Ok(UserReply {
        principal_id: body.principal_id,
        enrolled: body.enrolled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    const PRINCIPAL: &str = "6d3f2d1c-0000-4000-8000-00000000abcd";
    #[test]
    fn issuer_path_is_preserved() {
        for (base, expected) in [
            ("https://identity.example", "https://identity.example/users"),
            (
                "https://identity.example/authority",
                "https://identity.example/authority/users",
            ),
            (
                "https://identity.example/authority/",
                "https://identity.example/authority/users",
            ),
        ] {
            assert_eq!(service_endpoint(base, "users").unwrap().as_str(), expected);
        }
    }

    #[test]
    fn an_invitation_posts_the_principal_to_the_invitations_route() {
        let principal: PrincipalId = PRINCIPAL.parse().unwrap();
        assert_eq!(
            service_endpoint("https://identity.example/authority/", "invitations")
                .unwrap()
                .as_str(),
            "https://identity.example/authority/invitations"
        );
        {
            let observed = &(serde_json::from_slice::<serde_json::Value>(
                &invitation_body(&principal).unwrap(),
            )
            .unwrap());
            assert_eq!(observed["principal_id"], serde_json::json!(PRINCIPAL));
        };
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
            let error = service_endpoint(input, "users").unwrap_err();
            assert!(!format!("{error:#} {error:?}").contains("private-marker"));
        }
    }

    #[test]
    fn issuer_credentials_are_required_and_debug_is_redacted() {
        assert!(IdentityClient::for_route(&IdentityClientConfig::default(), "users").is_err());
        let mut args = IdentityClientConfig {
            endpoint: Some("https://identity.example/private-marker".to_owned()),
            ..Default::default()
        };
        assert!(IdentityClient::for_route(&args, "users").is_err());
        args.client_cert = Some(PathBuf::from("/private-marker/operator.pem"));
        assert!(IdentityClient::for_route(&args, "users").is_err());
        args.client_key = Some(PathBuf::from("/private-marker/operator.key"));
        args.server_ca = Some(PathBuf::from("/private-marker/roots.pem"));
        let error = IdentityClient::for_route(&args, "users").unwrap_err();
        assert!(!format!("{args:?} {error:#} {error:?}").contains("private-marker"));
    }

    struct HttpsFixture {
        args: IdentityClientConfig,
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
            "wamn-identity-client-{}-{}",
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
        let address = listener.local_addr().unwrap();
        let endpoint = format!("https://{address}/identity");
        let args = IdentityClientConfig {
            endpoint: Some(endpoint.clone()),
            client_cert: Some(write_pem("operator.pem", &client.pem())),
            client_key: Some(write_pem("operator.key", &client_key.serialize_pem())),
            server_ca: Some(write_pem("ca.pem", &ca.pem())),
        };
        let response = response(&endpoint);
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
                sender.send(String::from_utf8(request).unwrap()).unwrap();
                if !response.is_empty() {
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                }
            }
        });
        HttpsFixture {
            args,
            received,
            task,
            directory,
        }
    }

    #[tokio::test]
    async fn a_user_request_posts_the_email_and_names_a_refusal() {
        for (status, body, refused) in [
            (
                "200 OK",
                format!(r#"{{"principal_id":"{PRINCIPAL}","enrolled":true}}"#),
                false,
            ),
            (
                "409 Conflict",
                format!(r#"{{"error":"principal {PRINCIPAL} is disabled"}}"#),
                true,
            ),
        ] {
            let mut fixture = https_fixture("127.0.0.1", |_| {
                format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
            })
            .await;
            let result = create_user(&fixture.args, "ann@example.test", "Ann").await;
            let request = fixture.received.recv().await.unwrap();
            assert!(request.starts_with("POST /identity/users HTTP/1.1\r\n"));
            let (_, sent) = request.split_once("\r\n\r\n").unwrap();
            {
                let observed = &(serde_json::from_str::<serde_json::Value>(sent).unwrap());
                assert_eq!(observed["email"], serde_json::json!("ann@example.test"));
                assert_eq!(observed["display_name"], serde_json::json!("Ann"));
            };
            if refused {
                assert!(result.unwrap_err().downcast_ref::<UserRefused>().is_some());
            } else {
                let reply = result.unwrap();
                assert_eq!(reply.principal_id, PRINCIPAL.parse().unwrap());
                assert!(reply.enrolled);
            }
        }
    }
}
