//! HTTPS adapter for control user management.

pub mod users;

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use url::Url;
use wamn_oci::package_artifact::http_client::{HttpClient, Redirects};

/// Each identity request stops after five seconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
fn https_client(
    cert_path: &Path,
    key_path: &Path,
    server_ca: Option<&Path>,
) -> anyhow::Result<HttpClient> {
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
    if let Some(path) = server_ca {
        let pem = std::fs::read(path)
            .map_err(|_| anyhow::anyhow!("identity server trust roots read failed"))?;
        builder = builder
            .roots(&pem)
            .map_err(|_| anyhow::anyhow!("identity server trust roots are invalid"))?;
    }
    builder
        .identity_pem(&identity_pem)
        .map_err(|_| anyhow::anyhow!("identity client certificate or private key is invalid"))?
        .build()
        .context("identity HTTPS client creation failed")
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::users::{UserClient, UserRefused, invitation_body};
    use std::io::Write as _;
    use std::path::PathBuf;
    use wamn_identity_protocol::PrincipalId;
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

    struct HttpsFixture {
        client: UserClient,
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
        let client = UserClient::new(
            &endpoint,
            &write_pem("operator.pem", &client.pem()),
            &write_pem("operator.key", &client_key.serialize_pem()),
            &write_pem("ca.pem", &ca.pem()),
        )
        .unwrap();
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
            client,
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
            let result = fixture.client.create("ann@example.test", "Ann").await;
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
