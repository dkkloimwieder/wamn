//! Assertion contracts use public keys independent of the signer.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;

pub(in crate::plugins::connection_http) const RSA: &str = include_str!("fixtures/rsa.pem");
pub(in crate::plugins::connection_http) const P256: &str = include_str!("fixtures/p256.pem");

pub(in crate::plugins::connection_http) fn parameters(algorithm: &str, url: &str) -> Value {
    json!({"client_id":"client", "token_url":url, "key_id":"certificate-id",
        "algorithm":algorithm, "scopes":["a","b"]})
}

pub(in crate::plugins::connection_http) fn material(algorithm: &str) -> String {
    json!({"private_key":if algorithm == "ES256" { P256 } else { RSA }}).to_string()
}

pub(in crate::plugins::connection_http) fn assert_assertion(
    body: &[u8],
    algorithm: &str,
    client: &str,
    url: &str,
    scope: Option<&str>,
    now: Option<i64>,
) -> String {
    let form: HashMap<_, _> = url::form_urlencoded::parse(body).into_owned().collect();
    assert_eq!(form.len(), if scope.is_some() { 4 } else { 3 });
    assert_eq!(form["grant_type"], "client_credentials");
    assert_eq!(
        form["client_assertion_type"],
        "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
    );
    assert_eq!(form.get("scope").map(String::as_str), scope);
    let assertion = &form["client_assertion"];
    let parts: Vec<_> = assertion.split('.').collect();
    assert_eq!(parts.len(), 3);
    let decode = |part| URL_SAFE_NO_PAD.decode(part).unwrap();
    let header: Value = serde_json::from_slice(&decode(parts[0])).unwrap();
    assert_eq!(
        header,
        json!({"alg":algorithm,"kid":"certificate-id","typ":"JWT"})
    );
    let claims: Value = serde_json::from_slice(&decode(parts[1])).unwrap();
    let issued = claims["iat"].as_i64().unwrap();
    if let Some(now) = now {
        assert_eq!(issued, now);
    } else {
        assert!((chrono::Utc::now().timestamp() - issued).abs() < 10);
    }
    let jti = claims["jti"].as_str().unwrap();
    assert!(URL_SAFE_NO_PAD.decode(jti).unwrap().len() >= 16);
    let mut expected = json!({"iss":client,"aud":url,"iat":issued,"exp":issued+300,"jti":jti});
    if let Some(scope) = scope {
        expected["scope"] = scope.into();
    }
    assert_eq!(claims, expected);
    let (verification, public): (&dyn signature::VerificationAlgorithm, &[u8]) = match algorithm {
        "PS256" => (
            &signature::RSA_PSS_2048_8192_SHA256,
            include_bytes!("fixtures/rsa-public.der"),
        ),
        "RS256" => (
            &signature::RSA_PKCS1_2048_8192_SHA256,
            include_bytes!("fixtures/rsa-public.der"),
        ),
        "ES256" => {
            assert_eq!(decode(parts[2]).len(), 64, "RFC 7518 fixed R||S encoding");
            (
                &signature::ECDSA_P256_SHA256_FIXED,
                include_bytes!("fixtures/p256-public.sec1"),
            )
        }
        _ => panic!("unexpected test algorithm"),
    };
    signature::UnparsedPublicKey::new(verification, public)
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &decode(parts[2]),
        )
        .unwrap();
    jti.into()
}

#[test]
fn fixed_clock_headers_claims_and_scope_separators() {
    let now = 1_800_000_000;
    let url = "https://example.invalid/token";
    for algorithm in ["PS256", "ES256", "RS256"] {
        for separator in [None, Some(" "), Some(",")] {
            let mut parameters = parameters(algorithm, url);
            if let Some(separator) = separator {
                parameters["scope_separator"] = separator.into();
            }
            let material = material(algorithm);
            let first = request(&parameters, Some(&material), now).unwrap();
            let second = request(&parameters, Some(&material), now).unwrap();
            let scope = Some(if separator == Some(",") { "a,b" } else { "a b" });
            let first = assert_assertion(
                first.body.as_bytes(),
                algorithm,
                "client",
                url,
                scope,
                Some(now),
            );
            let second = assert_assertion(
                second.body.as_bytes(),
                algorithm,
                "client",
                url,
                scope,
                Some(now),
            );
            assert_ne!(first, second);
            for scopes in [Some(json!([])), None] {
                if let Some(scopes) = scopes {
                    parameters["scopes"] = scopes;
                } else {
                    parameters.as_object_mut().unwrap().remove("scopes");
                }
                let empty = request(&parameters, Some(&material), now).unwrap();
                assert_assertion(
                    empty.body.as_bytes(),
                    algorithm,
                    "client",
                    url,
                    None,
                    Some(now),
                );
            }
        }
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

    let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    assert_eq!(endpoint.identifiers.lock().unwrap().len(), 2);
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
