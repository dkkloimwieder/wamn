//! Verify RFC 9068 access tokens against one configured issuer.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, JwkSet, KeyOperations, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use moka::sync::Cache;
use serde::Deserialize;
use tokio::sync::Mutex;

const CLOCK_TOLERANCE: u64 = 30;
const FETCH_INTERVAL: Duration = Duration::from_secs(1);
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_JWKS_BYTES: usize = 65_536;
// The JWKS lifetime cap and the default when max-age is absent or invalid.
const MAX_KEY_LIFETIME: Duration = Duration::from_secs(300);

/// One verifier and its cache of the issuer's complete public key set.
#[derive(Debug)]
pub struct AccessTokenVerifier {
    endpoint: reqwest::Url,
    client: reqwest::Client,
    validation: Validation,
    keys: Cache<(), Arc<CachedKeys>>,
    last_fetch: Mutex<Option<Instant>>,
}

#[derive(Debug)]
struct CachedKeys {
    document: JwkSet,
    fresh_until: Instant,
}

/// The identity and admission deadline extracted from a verified token.
#[derive(Debug)]
pub struct AccessToken {
    issuer: String,
    subject: String,
    expires_at: u64,
    issued_at: u64,
    keys_fresh_until: Instant,
}

impl AccessToken {
    /// The exact configured issuer matched by this token.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The issuer's opaque, stable subject.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Confirm that the token and key set still allow admission after a database read.
    pub fn is_current(&self) -> bool {
        Instant::now() < self.keys_fresh_until
            && SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .is_ok_and(|now| {
                    now.as_secs() < self.expires_at.saturating_add(CLOCK_TOLERANCE)
                        && self.issued_at <= now.as_secs().saturating_add(CLOCK_TOLERANCE)
                })
    }
}

#[derive(Debug, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    #[serde(rename = "aud")]
    _aud: String,
    exp: u64,
    iat: u64,
    jti: String,
    client_id: String,
}

/// Internal verification failure. HTTP callers return an opaque authentication refusal.
#[derive(Debug)]
pub struct AccessTokenError {
    stage: Stage,
    context: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AccessTokenError {
    pub(crate) fn key_fetch_failed(&self) -> bool {
        matches!(self.stage, Stage::KeyFetch)
    }
}

impl std::fmt::Display for AccessTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.stage, self.context)
    }
}

impl std::error::Error for AccessTokenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

#[derive(Debug, Clone, Copy)]
enum Stage {
    Configuration,
    KeyFetch,
    Token,
}

impl Stage {
    fn refusal(self, context: &'static str) -> AccessTokenError {
        AccessTokenError {
            stage: self,
            context,
            source: None,
        }
    }

    fn caused_by(
        self,
        context: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> AccessTokenError {
        AccessTokenError {
            stage: self,
            context,
            source: Some(Box::new(source)),
        }
    }
}

impl AccessTokenVerifier {
    /// Open the configured CA and bind the verifier to one issuer and audience.
    ///
    /// # Errors
    /// Refuses incomplete, invalid, or unsupported verification settings.
    pub fn new(
        issuer: &str,
        jwks_url: &str,
        audience: &str,
        algorithms: &[String],
        ca_path: &Path,
    ) -> Result<Self, AccessTokenError> {
        https_url(issuer)
            .filter(|url| url.query().is_none())
            .ok_or_else(|| {
                Stage::Configuration.refusal("issuer must be an HTTPS URL without a query")
            })?;
        let endpoint = https_url(jwks_url)
            .ok_or_else(|| Stage::Configuration.refusal("JWKS URL must use HTTPS"))?;
        if algorithms != ["EdDSA"] || audience.trim().is_empty() {
            return Err(
                Stage::Configuration.refusal("an audience and the EdDSA algorithm are required")
            );
        }
        let pem = std::fs::read(ca_path)
            .map_err(|source| Stage::Configuration.caused_by("read JWKS CA bundle", source))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|source| Stage::Configuration.caused_by("parse JWKS CA bundle", source))?;
        if certs.is_empty() {
            return Err(Stage::Configuration.refusal("JWKS CA bundle is empty"));
        }
        let client = reqwest::Client::builder()
            .tls_certs_only(certs)
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|source| Stage::Configuration.caused_by("configure JWKS client", source))?;
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[issuer]);
        validation.set_audience(&[audience]);
        validation.set_required_spec_claims(&["iss", "sub", "aud", "exp"]);
        validation.leeway = CLOCK_TOLERANCE;
        validation.validate_nbf = true;
        Ok(Self {
            endpoint,
            client,
            validation,
            keys: Cache::builder()
                .max_capacity(1)
                .time_to_live(MAX_KEY_LIFETIME)
                .build(),
            last_fetch: Mutex::new(None),
        })
    }

    /// Authenticate one bearer token. Its subject is usable only until admission expires.
    ///
    /// # Errors
    /// Refuses unsupported profiles, unavailable keys, invalid signatures, and invalid claims.
    pub async fn verify(&self, token: &str) -> Result<AccessToken, AccessTokenError> {
        let header = decode_header(token)
            .map_err(|source| Stage::Token.caused_by("decode access token header", source))?;
        if header.typ.as_deref() != Some("at+jwt")
            || !self.validation.algorithms.contains(&header.alg)
            || header.crit.is_some()
        {
            return Err(Stage::Token.refusal("unsupported access token header"));
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.is_empty())
            .ok_or_else(|| Stage::Token.refusal("access token has no key ID"))?;
        let keys = self.keys_for(kid).await?;
        let mut matching = keys
            .document
            .keys
            .iter()
            .filter(|key| key.common.key_id.as_deref() == Some(kid));
        let key = matching
            .next()
            .ok_or_else(|| Stage::Token.refusal("unknown signing key"))?;
        if matching.next().is_some()
            || !matches!(&key.algorithm, AlgorithmParameters::OctetKeyPair(params) if params.curve == EllipticCurve::Ed25519)
            || key
                .common
                .key_algorithm
                .is_some_and(|algorithm| algorithm != header.alg.into())
            || key
                .common
                .public_key_use
                .as_ref()
                .is_some_and(|purpose| purpose != &PublicKeyUse::Signature)
            || key
                .common
                .key_operations
                .as_ref()
                .is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
        {
            return Err(Stage::Token.refusal("unsuitable signing key"));
        }
        let decoding_key = DecodingKey::from_jwk(key)
            .map_err(|source| Stage::Token.caused_by("decode signing key", source))?;
        let claims = decode::<Claims>(token, &decoding_key, &self.validation)
            .map_err(|source| Stage::Token.caused_by("verify access token", source))?
            .claims;
        if claims.sub.is_empty()
            || claims.jti.is_empty()
            || claims.client_id.is_empty()
            || claims.exp <= claims.iat
        {
            return Err(Stage::Token.refusal("invalid access token claims"));
        }
        let token = AccessToken {
            issuer: claims.iss,
            subject: claims.sub,
            expires_at: claims.exp,
            issued_at: claims.iat,
            keys_fresh_until: keys.fresh_until,
        };
        if !token.is_current() {
            return Err(Stage::Token.refusal("access token is outside its admission window"));
        }
        Ok(token)
    }

    fn cached_key(&self, kid: &str) -> Option<Arc<CachedKeys>> {
        self.keys
            .get(&())
            .filter(|keys| Instant::now() < keys.fresh_until && keys.document.find(kid).is_some())
    }

    async fn keys_for(&self, kid: &str) -> Result<Arc<CachedKeys>, AccessTokenError> {
        if let Some(keys) = self.cached_key(kid) {
            return Ok(keys);
        }
        // Only one refresh runs at a time. Keep the attempt deadline even on failure.
        let mut last_fetch = self.last_fetch.lock().await;
        if let Some(keys) = self.cached_key(kid) {
            return Ok(keys);
        }
        let started = Instant::now();
        if last_fetch.is_some_and(|last| started.duration_since(last) < FETCH_INTERVAL) {
            return Err(Stage::KeyFetch.refusal("JWKS refresh is rate limited"));
        }
        *last_fetch = Some(started);
        let keys = Arc::new(self.fetch_keys(started).await?);
        self.keys.insert((), Arc::clone(&keys));
        Ok(keys)
    }

    async fn fetch_keys(&self, started: Instant) -> Result<CachedKeys, AccessTokenError> {
        let mut response = self
            .client
            .get(self.endpoint.clone())
            .send()
            .await
            .map_err(|source| Stage::KeyFetch.caused_by("request JWKS", source.without_url()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(Stage::KeyFetch.refusal("JWKS response is not HTTP 200"));
        }
        let mut ages = response.headers().get_all(reqwest::header::AGE).iter();
        let age = match ages.next() {
            Some(value) => value
                .to_str()
                .ok()
                .and_then(|age| age.parse::<u64>().ok())
                .ok_or_else(|| Stage::KeyFetch.refusal("invalid JWKS Age"))?,
            None => 0,
        };
        if ages.next().is_some() {
            return Err(Stage::KeyFetch.refusal("multiple JWKS Age values"));
        }
        let remaining = jwks_lifetime(response.headers())
            .checked_sub(Duration::from_secs(age))
            .ok_or_else(|| Stage::KeyFetch.refusal("JWKS is stale"))?;
        let fresh_until = started + remaining;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| Stage::KeyFetch.caused_by("read JWKS body", source.without_url()))?
        {
            if chunk.len() > MAX_JWKS_BYTES - body.len() {
                return Err(Stage::KeyFetch.refusal("JWKS exceeds the response size limit"));
            }
            body.extend_from_slice(&chunk);
        }
        let document = serde_json::from_slice(&body)
            .map_err(|source| Stage::KeyFetch.caused_by("parse JWKS", source))?;
        if Instant::now() >= fresh_until {
            return Err(Stage::KeyFetch.refusal("JWKS is stale"));
        }
        Ok(CachedKeys {
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

fn https_url(value: &str) -> Option<reqwest::Url> {
    reqwest::Url::parse(value).ok().filter(|url| {
        url.scheme() == "https"
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use wamn_application::ingress::Header as RequestHeader;

    use crate::route_authentication::ControlRouteAuthenticator;

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
        let verifier = AccessTokenVerifier::new(
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
        // The product spaces fetches by FETCH_INTERVAL on the system clock.
        tokio::time::sleep(FETCH_INTERVAL).await;
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
