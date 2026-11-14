//! Assertion contracts use public keys independent of the signer.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;

pub(crate) const RSA: &str = include_str!("fixtures/rsa.pem");
pub(crate) const P256: &str = include_str!("fixtures/p256.pem");

pub(crate) fn parameters(algorithm: &str, url: &str) -> Value {
    json!({"client_id":"client", "token_url":url, "key_id":"certificate-id",
        "algorithm":algorithm, "scopes":["a","b"]})
}

pub(crate) fn material(algorithm: &str) -> String {
    json!({"private_key":if algorithm == "ES256" { P256 } else { RSA }}).to_string()
}

pub(crate) fn assert_assertion(
    body: &[u8],
    algorithm: &str,
    client: &str,
    url: &str,
    scope: Option<&str>,
    now: Option<i64>,
) -> String {
    let form: HashMap<_, _> = url::form_urlencoded::parse(body).into_owned().collect();
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("client_credentials")
    );
    assert_eq!(
        form.get("client_assertion_type").map(String::as_str),
        Some("urn:ietf:params:oauth:client-assertion-type:jwt-bearer")
    );
    assert_eq!(form.get("scope").map(String::as_str), scope);
    let algorithm: jsonwebtoken::Algorithm = algorithm.parse().unwrap();
    let key = match algorithm {
        jsonwebtoken::Algorithm::ES256 => {
            jsonwebtoken::DecodingKey::from_ec_der(include_bytes!("fixtures/p256-public.sec1"))
        }
        _ => jsonwebtoken::DecodingKey::from_rsa_der(include_bytes!("fixtures/rsa-public.der")),
    };
    let mut validation = jsonwebtoken::Validation::new(algorithm);
    validation.set_issuer(&[client]);
    validation.set_audience(&[url]);
    validation.validate_exp = false;
    let token =
        jsonwebtoken::decode::<Value>(&form["client_assertion"], &key, &validation).unwrap();
    assert_eq!(token.header.kid.as_deref(), Some("certificate-id"));
    assert_eq!(token.header.typ.as_deref(), Some("JWT"));
    let claims = token.claims;
    if let Some(scope) = scope {
        assert_eq!(claims["scope"].as_str(), Some(scope));
    } else {
        assert!(claims.get("scope").is_none());
    }
    let issued = claims["iat"].as_i64().unwrap();
    if let Some(now) = now {
        assert_eq!(issued, now);
    } else {
        assert!((chrono::Utc::now().timestamp() - issued).abs() < 10);
    }
    assert_eq!(claims["exp"].as_i64().unwrap() - issued, 300);
    claims["jti"].as_str().unwrap().into()
}

#[tokio::test]
async fn fixed_clock_headers_claims_and_scope_separators() {
    let url = "https://example.invalid/token";
    let now = 1_800_000_000;
    for (separator, scope) in [
        (None, Some("a b")),
        (Some(" "), Some("a b")),
        (Some(","), Some("a,b")),
        (None, None),
    ] {
        let mut parameters = parameters("RS256", url);
        if let Some(separator) = separator {
            parameters["scope_separator"] = separator.into();
        }
        if scope.is_none() {
            parameters["scopes"] = json!([]);
        }
        let request = request(&parameters, Some(&material("RS256")), now).unwrap();
        request
            .exchange(&|request: hyper::Request<Vec<u8>>| {
                assert_assertion(request.body(), "RS256", "client", url, scope, Some(now));
                Box::pin(std::future::ready(Ok::<_, std::convert::Infallible>(
                    hyper::Response::builder()
                        .header("content-type", "application/json")
                        .body(br#"{"access_token":"token","token_type":"Bearer"}"#.to_vec())
                        .unwrap(),
                )))
            })
            .await
            .unwrap();
    }
}

#[test]
fn malformed_pem_and_algorithm_mismatch_refuse_before_dispatch() {
    for (algorithm, pem) in [
        ("PS256", P256),
        ("RS256", P256),
        ("ES256", RSA),
        ("PS256", "not PEM"),
    ] {
        let parameters = parameters(algorithm, "https://example.invalid/token");
        let material = json!({"private_key":pem}).to_string();
        assert!(matches!(
            request(&parameters, Some(&material), 0),
            Err(ConnectionError::CredentialUnavailable)
        ));
    }
}

struct Endpoint {
    url: String,
    transport: super::super::HttpTransport,
    identifiers: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn endpoint(algorithm: &'static str, scope: &'static str) -> Endpoint {
    use http_body_util::{BodyExt as _, Full};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::sync::{Arc, Mutex};

    let address = super::super::tests::interface_address();
    let certificate = rcgen::generate_simple_self_signed(vec![address.to_string()]).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
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
    let transport = super::super::HttpTransport::with_tls_config(client);
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let listener = tokio::net::TcpListener::bind((address, 0)).await.unwrap();
    let base = format!("https://{}", listener.local_addr().unwrap());
    let url = format!("{base}/token");
    let audience = url.clone();
    let identifiers = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&identifiers);
    let task = tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let stream = acceptor.accept(tcp).await.unwrap();
            let identifiers = Arc::clone(&recorded);
            let audience = audience.clone();
            hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream),
                service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let identifiers = Arc::clone(&identifiers);
                    let audience = audience.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes();
                        let reply = if parts.uri.path() == "/token" {
                            assert_eq!(parts.method, "POST");
                            assert_eq!(parts.headers["content-type"], "application/x-www-form-urlencoded");
                            assert!(!parts.headers.contains_key("authorization"));
                            let jti = assert_assertion(&body, algorithm, "client", &audience, Some(scope), None);
                            let mut identifiers = identifiers.lock().unwrap();
                            assert!(!identifiers.contains(&jti));
                            identifiers.push(jti);
                            r#"{"access_token":"fixture-token","token_type":"Bearer","expires_in":3600}"#
                        } else {
                            assert_eq!(parts.method, "HEAD");
                            assert_eq!(parts.headers["authorization"], "Bearer fixture-token");
                            ""
                        };
                        Ok::<_, std::convert::Infallible>(hyper::Response::builder()
                            .header("connection", "close")
                            .body(Full::new(bytes::Bytes::from_static(reply.as_bytes()))).unwrap())
                    }
                })).await.unwrap();
        }
    });
    Endpoint {
        url,
        transport,
        identifiers,
        task,
    }
}

async fn health_round_trip(algorithm: &'static str, separator: Option<&str>) {
    let endpoint = endpoint(
        algorithm,
        if separator == Some(",") { "a,b" } else { "a b" },
    )
    .await;
    let mut auth = parameters(algorithm, &endpoint.url);
    if let Some(separator) = separator {
        auth["scope_separator"] = separator.into();
    }
    let parameters = json!({"primary-authority":endpoint.url.strip_suffix("/token").unwrap(),
        "tls-verification":"verify-authority", "auth":auth});
    for _ in 0..2 {
        super::super::health(
            &endpoint.transport,
            &parameters,
            "oauth2_client_credentials_jwt",
            Some(&material(algorithm)),
        )
        .await
        .unwrap();
    }
    assert!(!endpoint.identifiers.lock().unwrap().is_empty());
}

#[tokio::test]
async fn health_ps256_assertion_verified_by_endpoint() {
    health_round_trip("PS256", Some(",")).await;
}

#[tokio::test]
async fn health_es256_assertion_verified_by_endpoint() {
    health_round_trip("ES256", None).await;
}

#[tokio::test]
async fn health_rs256_assertion_verified_by_endpoint() {
    health_round_trip("RS256", Some(" ")).await;
}
