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
const KEY_LIFETIME: Duration =
    Duration::from_secs(wamn_identity::session_keys::JWKS_CACHE_SECONDS.cast_unsigned());

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
                .time_to_live(KEY_LIFETIME)
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
        let remaining = KEY_LIFETIME
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

fn https_url(value: &str) -> Option<reqwest::Url> {
    reqwest::Url::parse(value).ok().filter(|url| {
        url.scheme() == "https"
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}
