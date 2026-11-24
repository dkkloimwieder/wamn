//! Verify RFC 9068 access tokens against one configured issuer.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, JwkSet, KeyOperations, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use moka::sync::Cache;
use serde::Deserialize;
use tokio::sync::Mutex;

const CLOCK_TOLERANCE: u64 = 30;
const FETCH_INTERVAL: Duration = Duration::from_secs(1);
// The JWKS lifetime cap and the default when max-age is absent or invalid.
pub const MAX_KEY_LIFETIME: Duration = Duration::from_secs(300);

/// One verifier and its cache of the issuer's complete public key set.
#[derive(Debug)]
pub struct AccessTokenVerifier {
    source: Box<dyn KeySource>,
    validation: Validation,
    keys: Cache<(), Arc<FetchedKeys>>,
    last_fetch: Mutex<Option<Instant>>,
}

#[derive(Debug)]
pub struct FetchedKeys {
    pub document: JwkSet,
    pub fresh_until: Instant,
}

/// The issuer's signing keys and their freshness deadline.
#[async_trait::async_trait]
pub trait KeySource: std::fmt::Debug + Send + Sync {
    async fn fetch(&self, started: Instant) -> Result<FetchedKeys, AccessTokenError>;
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
    stage: AccessTokenErrorStage,
    context: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AccessTokenError {
    pub(crate) fn key_fetch_failed(&self) -> bool {
        matches!(self.stage, AccessTokenErrorStage::KeyFetch)
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
pub enum AccessTokenErrorStage {
    Configuration,
    KeyFetch,
    Token,
}

impl AccessTokenErrorStage {
    pub fn refusal(self, context: &'static str) -> AccessTokenError {
        AccessTokenError {
            stage: self,
            context,
            source: None,
        }
    }

    pub fn caused_by(
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
    /// Bind the verifier to one issuer and audience with the configured key source.
    ///
    /// # Errors
    /// Refuses incomplete, invalid, or unsupported verification settings.
    pub fn new<S: KeySource + 'static>(
        issuer: &str,
        jwks_url: &str,
        audience: &str,
        algorithms: &[String],
        key_source: impl FnOnce(url::Url) -> Result<S, AccessTokenError>,
    ) -> Result<Self, AccessTokenError> {
        https_url(issuer)
            .filter(|url| url.query().is_none())
            .ok_or_else(|| {
                AccessTokenErrorStage::Configuration
                    .refusal("issuer must be an HTTPS URL without a query")
            })?;
        let endpoint = https_url(jwks_url).ok_or_else(|| {
            AccessTokenErrorStage::Configuration.refusal("JWKS URL must use HTTPS")
        })?;
        if algorithms != ["EdDSA"] || audience.trim().is_empty() {
            return Err(AccessTokenErrorStage::Configuration
                .refusal("an audience and the EdDSA algorithm are required"));
        }
        let source = Box::new(key_source(endpoint)?);
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[issuer]);
        validation.set_audience(&[audience]);
        validation.set_required_spec_claims(&["iss", "sub", "aud", "exp"]);
        validation.leeway = CLOCK_TOLERANCE;
        validation.validate_nbf = true;
        Ok(Self {
            source,
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
        let header = decode_header(token).map_err(|source| {
            AccessTokenErrorStage::Token.caused_by("decode access token header", source)
        })?;
        if header.typ.as_deref() != Some("at+jwt")
            || !self.validation.algorithms.contains(&header.alg)
            || header.crit.is_some()
        {
            return Err(AccessTokenErrorStage::Token.refusal("unsupported access token header"));
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.is_empty())
            .ok_or_else(|| AccessTokenErrorStage::Token.refusal("access token has no key ID"))?;
        let keys = self.keys_for(kid).await?;
        let mut matching = keys
            .document
            .keys
            .iter()
            .filter(|key| key.common.key_id.as_deref() == Some(kid));
        let key = matching
            .next()
            .ok_or_else(|| AccessTokenErrorStage::Token.refusal("unknown signing key"))?;
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
            return Err(AccessTokenErrorStage::Token.refusal("unsuitable signing key"));
        }
        let decoding_key = DecodingKey::from_jwk(key).map_err(|source| {
            AccessTokenErrorStage::Token.caused_by("decode signing key", source)
        })?;
        let claims = decode::<Claims>(token, &decoding_key, &self.validation)
            .map_err(|source| {
                AccessTokenErrorStage::Token.caused_by("verify access token", source)
            })?
            .claims;
        if claims.sub.is_empty()
            || claims.jti.is_empty()
            || claims.client_id.is_empty()
            || claims.exp <= claims.iat
        {
            return Err(AccessTokenErrorStage::Token.refusal("invalid access token claims"));
        }
        let token = AccessToken {
            issuer: claims.iss,
            subject: claims.sub,
            expires_at: claims.exp,
            issued_at: claims.iat,
            keys_fresh_until: keys.fresh_until,
        };
        if !token.is_current() {
            return Err(AccessTokenErrorStage::Token
                .refusal("access token is outside its admission window"));
        }
        Ok(token)
    }

    fn cached_key(&self, kid: &str) -> Option<Arc<FetchedKeys>> {
        self.keys
            .get(&())
            .filter(|keys| Instant::now() < keys.fresh_until && keys.document.find(kid).is_some())
    }

    async fn keys_for(&self, kid: &str) -> Result<Arc<FetchedKeys>, AccessTokenError> {
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
            return Err(AccessTokenErrorStage::KeyFetch.refusal("JWKS refresh is rate limited"));
        }
        *last_fetch = Some(started);
        let keys = Arc::new(self.source.fetch(started).await?);
        self.keys.insert((), Arc::clone(&keys));
        Ok(keys)
    }
}

fn https_url(value: &str) -> Option<url::Url> {
    url::Url::parse(value).ok().filter(|url| {
        url.scheme() == "https"
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}
