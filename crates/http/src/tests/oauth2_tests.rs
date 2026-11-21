//! OAuth2 credentials exercise the host send path over a local listener.

use std::convert::Infallible;

use http_body_util::{BodyExt as _, Full};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;
use crate::test_util::{CLIENT_ID, CLIENT_SECRET, Observed};

#[test]
fn malformed_credentials_and_parameters_refuse() {
    let valid = serde_json::json!({"client_id":CLIENT_ID,"token_url":"https://example.com/token","scopes":[]});
    for material in [
        None,
        Some("not json"),
        Some("null"),
        Some("{}"),
        Some(r#"{"client_secret":""}"#),
        Some(r#"{"client_secret":1}"#),
        Some(r#"{"client_secret":"private","extra":true}"#),
    ] {
        assert!(oauth2_request("oauth2_client_credentials", &valid, material, None).is_err());
    }
    let secret = serde_json::json!({"client_secret":CLIENT_SECRET}).to_string();
    let mut invalid = vec![serde_json::json!(null)];
    for field in ["token_url", "client_id", "scopes"] {
        let mut parameters = valid.clone();
        parameters.as_object_mut().unwrap().remove(field);
        invalid.push(parameters);
        let mut parameters = valid.clone();
        parameters[field] = true.into();
        invalid.push(parameters);
    }
    invalid.extend([
        serde_json::json!({"client_id":"","token_url":"https://example.com/token","scopes":[]}),
        serde_json::json!({"client_id":CLIENT_ID,"token_url":"https://example.com/token","scopes":[1]}),
        serde_json::json!({"client_id":CLIENT_ID,"token_url":"https://example.com/token","scopes":[],"extra":true}),
    ]);
    for parameters in invalid {
        assert!(
            oauth2_request(
                "oauth2_client_credentials",
                &parameters,
                Some(&secret),
                None
            )
            .is_err()
        );
    }
}

struct TlsTokenEndpoint {
    url: String,
    transport: Arc<HttpTransport>,
    observed: Arc<Mutex<Vec<Observed>>>,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
    task: JoinHandle<()>,
}

impl TlsTokenEndpoint {
    async fn start() -> Self {
        let certificate = rcgen::generate_simple_self_signed(vec![
            super::interface_address().to_string(),
            "localhost".into(),
        ])
        .unwrap();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                certificate.signing_key.serialize_der(),
            )),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate.cert.der().clone()).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        // The listener has a private address, which only the local target reaches.
        let transport = Arc::new(HttpTransport::from_tls(client, AddressPolicy::Local));
        let listener = TcpListener::bind((super::interface_address(), 0))
            .await
            .unwrap();
        let url = format!("https://{}/token", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&observed);
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accept_count = Arc::clone(&accepted);
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        let task = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                accept_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let stream = acceptor.accept(tcp).await.unwrap();
                let observed = Arc::clone(&recorded);
                hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let observed = Arc::clone(&observed);
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes().to_vec();
                        observed.lock().unwrap().push(Observed { path: parts.uri.to_string(), method: parts.method.to_string(), headers: parts.headers, body });
                        Ok::<_,Infallible>(hyper::Response::builder().header("connection","close").body(Full::new(bytes::Bytes::from_static(br#"{"access_token":"fixture-token-1","refresh_token":"separate-refresh","token_type":"Bearer","expires_in":3600}"#))).unwrap())
                    }
                })).await.unwrap();
            }
        });
        Self {
            url,
            transport,
            observed,
            accepted,
            task,
        }
    }

    fn authority(&self) -> String {
        let url = url::Url::parse(&self.url).unwrap();
        format!(
            "{}:{}",
            url.host_str().unwrap(),
            url.port_or_known_default().unwrap()
        )
    }
}

impl Drop for TlsTokenEndpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn token_authority_health_allows_secret_and_jwt_and_refuses_other_origins() {
    let endpoint = TlsTokenEndpoint::start().await;
    let refused_endpoint = TlsTokenEndpoint::start().await;
    let mut primary = url::Url::parse(&endpoint.url).unwrap();
    primary.set_path("/");
    for auth_type in ["oauth2_client_credentials", "oauth2_client_credentials_jwt"] {
        let mut auth = serde_json::json!({"client_id":"client", "scopes":["a","b"], "token_url":endpoint.url, "token_authority":endpoint.authority()});
        let material = if auth_type == "oauth2_client_credentials_jwt" {
            auth["algorithm"] = "ES256".into();
            auth["key_id"] = "certificate-id".into();
            super::super::jwt::tests::material("ES256")
        } else {
            serde_json::json!({"client_secret":"secret"}).to_string()
        };
        let mut parameters = serde_json::json!({"primary-authority":primary.as_str(), "tls-verification":"verify-authority", "auth":auth});
        health(&endpoint.transport, &parameters, auth_type, Some(&material))
            .await
            .unwrap();
        let before = endpoint.observed.lock().unwrap().len();
        parameters["auth"]["token_url"] = refused_endpoint.url.clone().into();
        assert!(
            health(&endpoint.transport, &parameters, auth_type, Some(&material))
                .await
                .is_err()
        );
        assert_eq!(endpoint.observed.lock().unwrap().len(), before);
        assert!(refused_endpoint.observed.lock().unwrap().is_empty());
        assert_eq!(
            refused_endpoint
                .accepted
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}
