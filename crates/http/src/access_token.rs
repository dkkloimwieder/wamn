//! HTTPS fetches for issuer signing keys.

use std::path::Path;
use std::time::{Duration, Instant};

use wamn_authentication::access_token::{
    AccessTokenError, AccessTokenErrorStage, AccessTokenVerifier, FetchedKeys, KeySource,
    MAX_KEY_LIFETIME,
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_JWKS_BYTES: usize = 65_536;

/// Bind access token verification to a configured HTTPS endpoint and CA bundle.
pub fn access_token_verifier(
    issuer: &str,
    jwks_url: &str,
    audience: &str,
    algorithms: &[String],
    ca_path: &Path,
) -> Result<AccessTokenVerifier, AccessTokenError> {
    AccessTokenVerifier::new(issuer, jwks_url, audience, algorithms, |endpoint| {
        HttpsKeySource::new(endpoint, ca_path)
    })
}

#[derive(Debug)]
struct HttpsKeySource {
    endpoint: url::Url,
    client: reqwest::Client,
}

impl HttpsKeySource {
    fn new(endpoint: url::Url, ca_path: &Path) -> Result<Self, AccessTokenError> {
        let pem = std::fs::read(ca_path).map_err(|source| {
            AccessTokenErrorStage::Configuration.caused_by("read JWKS CA bundle", source)
        })?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|source| {
            AccessTokenErrorStage::Configuration.caused_by("parse JWKS CA bundle", source)
        })?;
        if certs.is_empty() {
            return Err(AccessTokenErrorStage::Configuration.refusal("JWKS CA bundle is empty"));
        }
        let client = reqwest::Client::builder()
            .tls_certs_only(certs)
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|source| {
                AccessTokenErrorStage::Configuration.caused_by("configure JWKS client", source)
            })?;
        Ok(Self { endpoint, client })
    }
}

#[async_trait::async_trait]
impl KeySource for HttpsKeySource {
    async fn fetch(&self, started: Instant) -> Result<FetchedKeys, AccessTokenError> {
        let mut response = self
            .client
            .get(self.endpoint.clone())
            .send()
            .await
            .map_err(|source| {
                AccessTokenErrorStage::KeyFetch.caused_by("request JWKS", source.without_url())
            })?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(AccessTokenErrorStage::KeyFetch.refusal("JWKS response is not HTTP 200"));
        }
        let mut ages = response.headers().get_all(reqwest::header::AGE).iter();
        let age = match ages.next() {
            Some(value) => value
                .to_str()
                .ok()
                .and_then(|age| age.parse::<u64>().ok())
                .ok_or_else(|| AccessTokenErrorStage::KeyFetch.refusal("invalid JWKS Age"))?,
            None => 0,
        };
        if ages.next().is_some() {
            return Err(AccessTokenErrorStage::KeyFetch.refusal("multiple JWKS Age values"));
        }
        let remaining = jwks_lifetime(response.headers())
            .checked_sub(Duration::from_secs(age))
            .ok_or_else(|| AccessTokenErrorStage::KeyFetch.refusal("JWKS is stale"))?;
        let fresh_until = started + remaining;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|source| {
            AccessTokenErrorStage::KeyFetch.caused_by("read JWKS body", source.without_url())
        })? {
            if chunk.len() > MAX_JWKS_BYTES - body.len() {
                return Err(
                    AccessTokenErrorStage::KeyFetch.refusal("JWKS exceeds the response size limit")
                );
            }
            body.extend_from_slice(&chunk);
        }
        let document = serde_json::from_slice(&body)
            .map_err(|source| AccessTokenErrorStage::KeyFetch.caused_by("parse JWKS", source))?;
        if Instant::now() >= fresh_until {
            return Err(AccessTokenErrorStage::KeyFetch.refusal("JWKS is stale"));
        }
        Ok(FetchedKeys {
            document,
            fresh_until,
        })
    }
}

fn jwks_lifetime(headers: &reqwest::header::HeaderMap) -> Duration {
    headers
        .get_all(reqwest::header::CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|directive| directive.trim().split_once('='))
        .find(|(name, value)| {
            name.eq_ignore_ascii_case("max-age")
                && !value.is_empty()
                && value.as_bytes().iter().all(u8::is_ascii_digit)
        })
        .and_then(|(_, value)| value.parse().ok())
        .map_or(MAX_KEY_LIFETIME, Duration::from_secs)
        .min(MAX_KEY_LIFETIME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::fmt::writer::MakeWriterExt as _;
    use wamn_app::ingress::{Header as RequestHeader, RoutePolicy, authenticate_policy};

    use wamn_authentication::authorization::Authorization;
    use wamn_authentication::route_authentication::{
        ControlRouteAuthenticator, PlatformRouteAuthenticator,
    };
    use wamn_postgres::{StaticCredentialProvider, WamnPostgres};

    const ISSUER: &str = "https://identity.example";
    const AUDIENCE: &str = "urn:wamn:org:acme";

    /// One Ed25519 signing key and its public JWK.
    struct SigningKey {
        kid: &'static str,
        encoding: EncodingKey,
        jwk: serde_json::Value,
    }

    fn signing_key(kid: &'static str) -> SigningKey {
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        SigningKey {
            kid,
            encoding: EncodingKey::from_ed_pem(pair.serialize_pem().as_bytes()).unwrap(),
            jwk: serde_json::json!({
                "kty": "OKP", "crv": "Ed25519", "use": "sig", "kid": kid,
                "x": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pair.public_key_raw()),
            }),
        }
    }

    /// A JWKS endpoint over TLS that serves the current key set.
    struct Issuer {
        keys: Arc<std::sync::Mutex<Option<Vec<serde_json::Value>>>>,
        fetches: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
        _directory: tempfile::TempDir,
    }

    impl Drop for Issuer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn issuer(keys: Option<Vec<serde_json::Value>>) -> (Issuer, AccessTokenVerifier) {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyUsagePurpose};
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let ca_issuer = rcgen::Issuer::new(ca_params, ca_key);
        let server_key = rcgen::KeyPair::generate().unwrap();
        let server = CertificateParams::new(vec!["127.0.0.1".to_owned()])
            .unwrap()
            .signed_by(&server_key, &ca_issuer)
            .unwrap();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![server.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let keys = Arc::new(std::sync::Mutex::new(keys));
        let fetches = Arc::new(AtomicUsize::new(0));
        let (listed, counted) = (Arc::clone(&keys), Arc::clone(&fetches));
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    continue;
                };
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.ends_with(b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0);
                    request.extend_from_slice(&buffer[..read]);
                }
                counted.fetch_add(1, Ordering::SeqCst);
                let response = match listed.lock().unwrap().as_ref() {
                    Some(keys) => {
                        let body = serde_json::json!({"keys": keys}).to_string();
                        format!(
                            "HTTP/1.1 200 OK\r\nCache-Control: max-age=300\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    None => "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let ca_path = directory.path().join("ca.pem");
        std::fs::write(&ca_path, ca.pem()).unwrap();
        let verifier = access_token_verifier(
            ISSUER,
            &format!("https://{address}/jwks"),
            AUDIENCE,
            &["EdDSA".to_owned()],
            &ca_path,
        )
        .unwrap();
        let issuer = Issuer {
            keys,
            fetches,
            task,
            _directory: directory,
        };
        (issuer, verifier)
    }

    fn now() -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap()
    }

    fn claims() -> serde_json::Value {
        serde_json::json!({
            "iss": ISSUER, "sub": "subject", "aud": AUDIENCE, "exp": now() + 900,
            "iat": now() - 60, "jti": "token", "client_id": "web",
        })
    }

    fn header(kid: &str) -> Header {
        let mut header = Header::new(Algorithm::EdDSA);
        header.typ = Some("at+jwt".to_owned());
        header.kid = Some(kid.to_owned());
        header
    }

    fn token(key: &SigningKey, header: &Header, claims: &serde_json::Value) -> String {
        encode(header, claims, &key.encoding).unwrap()
    }

    #[tokio::test]
    async fn control_distinguishes_unavailable_keys_from_an_invalid_signature() {
        let key = signing_key("current");
        let mut claims = claims();
        claims["sub"] = serde_json::json!("11111111-1111-4111-8111-111111111111");
        let attachment = "control-http".parse().unwrap();
        let bearer = |jwt| {
            [RequestHeader {
                name: "authorization".to_owned(),
                value: format!("Bearer {jwt}"),
            }]
        };
        let valid = bearer(token(&key, &header(key.kid), &claims));
        let invalid = bearer(token(&signing_key("other"), &header(key.kid), &claims));

        let (_unavailable, verifier) = issuer(None).await;
        let rejected = ControlRouteAuthenticator::new(verifier)
            .authenticate_control(&attachment, &valid)
            .await
            .unwrap_err();
        assert_eq!(rejected.status, 503);
        assert_eq!(rejected.code, "authentication-unavailable");

        let (_available, verifier) = issuer(Some(vec![key.jwk])).await;
        let authenticator = ControlRouteAuthenticator::new(verifier);
        let rejected = authenticator
            .authenticate_control(&attachment, &invalid)
            .await
            .unwrap_err();
        assert_eq!(rejected.status, 401);
        assert_eq!(rejected.code, "unauthorized");
        authenticator
            .authenticate_control(&attachment, &valid)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn host_sessions_distinguish_unavailable_keys_from_invalid_tokens() {
        let key = signing_key("current");
        let other = signing_key("other");
        let claims = claims();
        let valid = token(&key, &header(key.kid), &claims);
        let invalid = token(&other, &header(key.kid), &claims);
        let unknown = token(&other, &header(other.kid), &claims);
        let target = "acme--dev--billing".parse().unwrap();
        let attachment = "session-http".parse().unwrap();
        let policy = serde_json::json!({"modes": ["session"]});
        let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
            StaticCredentialProvider::default(),
        )));
        let authorization = Arc::new(Authorization::new(postgres));
        let reject = async |authenticator: &PlatformRouteAuthenticator, jwt: &str| {
            let rejection = authenticate_policy(
                None,
                Some(authenticator),
                &target,
                &attachment,
                RoutePolicy { policy: &policy },
                &[RequestHeader {
                    name: "authorization".to_owned(),
                    value: format!("Bearer {jwt}"),
                }],
                &[],
            )
            .await
            .expect_err("the session must be refused");
            (rejection.status, rejection.code)
        };

        let (_unavailable, verifier) = issuer(None).await;
        let authenticator =
            PlatformRouteAuthenticator::new(Arc::clone(&authorization), Some(verifier));
        let log = tempfile::NamedTempFile::new().unwrap();
        let warnings = log.reopen().unwrap().with_filter(|metadata| {
            *metadata.level() == tracing::Level::WARN
                && metadata.target() == "wamn_authentication::route_authentication"
        });
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .with_writer(warnings)
            .finish();
        let (status, code) = reject(&authenticator, &valid)
            .with_subscriber(subscriber)
            .await;
        assert_eq!((status, code.as_str()), (503, "authentication-unavailable"));
        let warning = std::fs::read_to_string(log.path()).unwrap();
        assert!(!warning.is_empty(), "a failed key fetch must log a warning");
        assert!(
            warning.contains(target.as_str()),
            "the warning must identify the project deployment"
        );
        assert!(
            !warning.contains(&valid),
            "the warning must omit the bearer token"
        );

        let (available, verifier) = issuer(Some(vec![key.jwk])).await;
        let authenticator = PlatformRouteAuthenticator::new(authorization, Some(verifier));
        for (jwt, case) in [(&unknown, "missing key"), (&invalid, "invalid signature")] {
            let (status, code) = reject(&authenticator, jwt).await;
            assert_eq!((status, code.as_str()), (401, "unauthorized"), "{case}");
        }

        *available.keys.lock().unwrap() = None;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (status, code) = reject(&authenticator, &unknown).await;
        assert_eq!(
            (status, code.as_str()),
            (503, "authentication-unavailable"),
            "failed refresh"
        );
    }

    #[tokio::test]
    async fn only_tokens_of_the_access_token_profile_are_admitted() {
        let key = signing_key("current");
        let (_issuer, verifier) = issuer(Some(vec![key.jwk.clone()])).await;
        let admitted = verifier
            .verify(&token(&key, &header(key.kid), &claims()))
            .await
            .unwrap();
        assert_eq!((admitted.issuer(), admitted.subject()), (ISSUER, "subject"));
        // 30 seconds of clock tolerance for exp and iat.
        for (claim, offset) in [("exp", -20), ("iat", 20)] {
            let mut tolerated = claims();
            tolerated[claim] = (now() + offset).into();
            let token = token(&key, &header(key.kid), &tolerated);
            verifier.verify(&token).await.unwrap();
        }

        let mut refused = Vec::new();
        for (claim, value) in [
            ("iss", serde_json::json!("https://other.example")),
            ("aud", serde_json::json!("urn:wamn:org:globex")),
            ("exp", serde_json::json!(now() - 40)),
            ("iat", serde_json::json!(now() + 40)),
            ("sub", serde_json::json!("")),
            ("jti", serde_json::json!("")),
            ("client_id", serde_json::json!("")),
        ] {
            let mut changed = claims();
            changed[claim] = value;
            refused.push((claim, token(&key, &header(key.kid), &changed)));
        }
        for claim in ["iss", "sub", "aud", "exp", "iat", "jti", "client_id"] {
            let mut missing = claims();
            missing.as_object_mut().unwrap().remove(claim);
            refused.push((claim, token(&key, &header(key.kid), &missing)));
        }
        let mut untyped = header(key.kid);
        untyped.typ = Some("JWT".to_owned());
        refused.push(("typ", token(&key, &untyped, &claims())));
        let mut unnamed = header(key.kid);
        unnamed.kid = None;
        refused.push(("kid", token(&key, &unnamed, &claims())));
        let mut hmac = header(key.kid);
        hmac.alg = Algorithm::HS256;
        refused.push((
            "alg",
            encode(&hmac, &claims(), &EncodingKey::from_secret(b"secret")).unwrap(),
        ));
        let forged = SigningKey {
            kid: key.kid,
            ..signing_key("forged")
        };
        refused.push(("signature", token(&forged, &header(key.kid), &claims())));
        for (case, token) in refused {
            assert!(verifier.verify(&token).await.is_err(), "{case}");
        }
    }

    #[tokio::test]
    async fn an_unknown_key_id_fetches_the_key_set_again() {
        let (current, next) = (signing_key("current"), signing_key("next"));
        let (issuer, verifier) = issuer(Some(vec![current.jwk.clone()])).await;
        verifier
            .verify(&token(&current, &header(current.kid), &claims()))
            .await
            .unwrap();
        let next_token = token(&next, &header(next.kid), &claims());
        issuer.keys.lock().unwrap().as_mut().unwrap().push(next.jwk);
        // The verifier requires one second between key requests.
        tokio::time::sleep(Duration::from_secs(1)).await;
        verifier.verify(&next_token).await.unwrap();
        assert_eq!(issuer.fetches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_key_set_lifetime_follows_max_age_up_to_300_seconds() {
        let lifetime = |value: Option<&str>| {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(value) = value {
                headers.insert(reqwest::header::CACHE_CONTROL, value.parse().unwrap());
            }
            jwks_lifetime(&headers).as_secs()
        };
        assert_eq!(lifetime(Some("public, max-age=60")), 60);
        for value in [
            None,
            Some("max-age=600"),
            Some("max-age=-1"),
            Some("no-cache"),
        ] {
            assert_eq!(lifetime(value), 300, "{value:?}");
        }
    }
}
