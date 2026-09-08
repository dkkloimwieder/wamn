//! Public session-key evidence from one explicitly trusted HTTPS issuer endpoint.
//!
//! Clone one cache per configured issuer. A key ID only selects within that
//! issuer's complete set; it never discovers an issuer or grants trust. This
//! module does not verify tokens. Callers must check the returned evidence's
//! freshness again at their final admission boundary.

use std::backtrace::Backtrace;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AGE};
use wamn_platform_identity::session_keys::{PublicSessionKey, SessionJwks, decode_public_key};

// Owner-approved policy: wamn-ctc8.24–28 and JWT proposal §3–4.
const MAX_AGE: Duration = Duration::from_secs(300);
const ATTEMPT_INTERVAL: Duration = Duration::from_secs(1);
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BODY_BYTES: usize = 65_536;

/// Trusted issuer identity, exact JWKS endpoint, and exclusive certificate roots.
#[derive(Clone, Debug)]
pub struct IssuerKeysConfig {
    issuer: String,
    endpoint: url::Url,
    roots: Vec<reqwest::Certificate>,
}

impl IssuerKeysConfig {
    /// Validate explicit HTTPS configuration and a nonempty PEM CA bundle.
    ///
    /// The issuer string is retained exactly for subsequent claim comparison.
    /// No discovery, redirect, ambient proxy, or additional CA roots are used.
    pub fn new(
        issuer: &str,
        endpoint: &str,
        trusted_ca_pem: &[u8],
    ) -> Result<Self, SessionKeyError> {
        let issuer_url = https_url(issuer)?;
        if issuer_url.query().is_some() {
            return Err(SessionKeyError::new("issuer must not contain a query"));
        }
        let endpoint = https_url(endpoint)?;
        let roots = reqwest::Certificate::from_pem_bundle(trusted_ca_pem)
            .map_err(|source| SessionKeyError::caused("invalid issuer CA bundle", source))?;
        if roots.is_empty() {
            return Err(SessionKeyError::new("issuer CA bundle is empty"));
        }
        Ok(Self {
            issuer: issuer.to_owned(),
            endpoint,
            roots,
        })
    }
}

fn https_url(value: &str) -> Result<url::Url, SessionKeyError> {
    let url = url::Url::parse(value)
        .map_err(|source| SessionKeyError::caused("invalid configured issuer URL", source))?;
    if value.trim() != value
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(SessionKeyError::new(
            "issuer URLs require HTTPS without credentials or fragments",
        ));
    }
    Ok(url)
}

/// Shared public-key cache for exactly one configured issuer.
#[derive(Clone, Debug)]
pub struct IssuerKeys {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    issuer: String,
    endpoint: url::Url,
    client: reqwest::Client,
    clock: Clock,
    state: Mutex<State>,
    refresh: tokio::sync::Mutex<()>,
}

#[derive(Debug, Default)]
struct State {
    set: Option<KeySet>,
    last_attempt: Option<Instant>,
}

#[derive(Debug)]
struct KeySet {
    keys: BTreeMap<String, PublicSessionKey>,
    deadline: Instant,
}

impl IssuerKeys {
    /// Construct one cache; clones share its set and refresh budget.
    pub fn new(config: IssuerKeysConfig) -> Result<Self, SessionKeyError> {
        Self::with_clock(config, Clock::System)
    }

    fn with_clock(config: IssuerKeysConfig, clock: Clock) -> Result<Self, SessionKeyError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .tls_backend_rustls()
            .tls_certs_only(config.roots)
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|source| SessionKeyError::caused("construct issuer HTTPS client", source))?;
        Ok(Self {
            inner: Arc::new(Inner {
                issuer: config.issuer,
                endpoint: config.endpoint,
                client,
                clock,
                state: Mutex::new(State::default()),
                refresh: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// The exact configured issuer, never a value supplied by a token.
    pub fn issuer(&self) -> &str {
        &self.inner.issuer
    }

    /// Obtain fresh public-key evidence, refreshing at most once for this call.
    ///
    /// Fresh known keys need no network request. Unknown or expired keys fail
    /// closed during another refresh or the one-second attempt-start interval.
    /// A cancelled fetch releases the single-flight slot but keeps its interval.
    pub async fn key(&self, kid: &str) -> Result<KeyEvidence, SessionKeyError> {
        if let Some(evidence) = self.cached(kid) {
            return Ok(evidence);
        }
        let _refresh = self
            .inner
            .refresh
            .try_lock()
            .map_err(|_| SessionKeyError::new("issuer refresh already in progress"))?;
        if let Some(evidence) = self.cached(kid) {
            return Ok(evidence);
        }
        // Stamp before dispatch, not at response completion. No unbounded
        // waiter queue or per-kid state can turn IDs into additional requests.
        let started = self.inner.clock.now();
        {
            let mut state = self.inner.state.lock().expect("session key state lock");
            if state
                .last_attempt
                .is_some_and(|last| started.saturating_duration_since(last) < ATTEMPT_INTERVAL)
            {
                return Err(SessionKeyError::new(
                    "issuer refresh attempt is rate limited",
                ));
            }
            state.last_attempt = Some(started);
        }
        let set = tokio::time::timeout(FETCH_TIMEOUT, self.fetch(started))
            .await
            .map_err(|source| {
                SessionKeyError::caused("issuer refresh exceeded five seconds", source)
            })??;
        {
            let mut state = self.inner.state.lock().expect("session key state lock");
            if self.inner.clock.now() >= set.deadline {
                return Err(SessionKeyError::new("issuer key response arrived expired"));
            }
            // Successful refresh replaces the whole set, including an empty
            // set. Previously cached keys absent from it cease to be selectable.
            state.set = Some(set);
        }
        self.cached(kid)
            .ok_or_else(|| SessionKeyError::new("key is absent from fresh issuer evidence"))
    }

    fn cached(&self, kid: &str) -> Option<KeyEvidence> {
        let state = self.inner.state.lock().expect("session key state lock");
        let set = state.set.as_ref()?;
        if self.inner.clock.now() >= set.deadline {
            return None;
        }
        Some(KeyEvidence {
            issuer: self.inner.issuer.clone(),
            key: set.keys.get(kid)?.clone(),
            deadline: set.deadline,
            clock: self.inner.clock.clone(),
        })
    }

    async fn fetch(&self, started: Instant) -> Result<KeySet, SessionKeyError> {
        let mut response = self
            .inner
            .client
            .get(self.inner.endpoint.clone())
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|source| SessionKeyError::caused("issuer HTTPS request failed", source))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(SessionKeyError::new("issuer JWKS response is not HTTP 200"));
        }
        let mut age_values = response.headers().get_all(AGE).iter();
        let age = match age_values.next() {
            None => 0,
            Some(value) => {
                let value = value
                    .to_str()
                    .map_err(|source| SessionKeyError::caused("invalid JWKS Age", source))?;
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(SessionKeyError::new("invalid JWKS Age"));
                }
                value
                    .parse::<u64>()
                    .map_err(|source| SessionKeyError::caused("invalid JWKS Age", source))?
            }
        };
        if age_values.next().is_some() {
            return Err(SessionKeyError::new("multiple JWKS Age values"));
        }
        let remaining = MAX_AGE
            .checked_sub(Duration::from_secs(age))
            .ok_or_else(|| SessionKeyError::new("issuer key response arrived expired"))?;
        let deadline = started + remaining;
        if self.inner.clock.now() >= deadline {
            return Err(SessionKeyError::new("issuer key response arrived expired"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| SessionKeyError::caused("read issuer JWKS response", source))?
        {
            if chunk.len() > MAX_BODY_BYTES - body.len() {
                return Err(SessionKeyError::new("issuer JWKS exceeds 65536 bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        let jwks: SessionJwks = serde_json::from_slice(&body)
            .map_err(|source| SessionKeyError::caused("invalid public JWKS document", source))?;
        let mut keys = BTreeMap::new();
        for key in jwks.keys {
            decode_public_key(&key)
                .map_err(|source| SessionKeyError::caused("invalid public Ed25519 key", source))?;
            if keys.insert(key.kid.clone(), key).is_some() {
                return Err(SessionKeyError::new("JWKS contains duplicate key IDs"));
            }
        }
        if self.inner.clock.now() >= deadline {
            return Err(SessionKeyError::new("issuer key response arrived expired"));
        }
        Ok(KeySet { keys, deadline })
    }

    /// Construct a real HTTPS cache whose evidence clock advances only explicitly.
    #[cfg(feature = "test-util")]
    pub fn with_test_clock(config: IssuerKeysConfig) -> Result<(Self, TestClock), SessionKeyError> {
        let clock = TestClock {
            now: Arc::new(Mutex::new(Instant::now())),
        };
        Ok((
            Self::with_clock(config, Clock::Fixed(clock.clone()))?,
            clock,
        ))
    }
}

/// Public key and its original evidence deadline, bound to the configured issuer.
#[derive(Clone, Debug)]
pub struct KeyEvidence {
    issuer: String,
    key: PublicSessionKey,
    deadline: Instant,
    clock: Clock,
}

impl KeyEvidence {
    /// Exact issuer under which this key was fetched.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// Public-only JWK. Possession does not by itself authorize token admission.
    pub fn public_key(&self) -> &PublicSessionKey {
        &self.key
    }

    /// Monotonic expiry anchored at request start, shortened by HTTP Age.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Recheck immediately at final admission; equality is expired.
    pub fn is_fresh(&self) -> bool {
        self.clock.now() < self.deadline
    }
}

#[derive(Clone, Debug)]
enum Clock {
    System,
    #[cfg(feature = "test-util")]
    Fixed(TestClock),
}

impl Clock {
    fn now(&self) -> Instant {
        match self {
            Self::System => Instant::now(),
            #[cfg(feature = "test-util")]
            Self::Fixed(clock) => clock.now(),
        }
    }
}

/// Per-cache deterministic evidence clock; transport deadlines remain real.
#[cfg(feature = "test-util")]
#[derive(Clone, Debug)]
pub struct TestClock {
    now: Arc<Mutex<Instant>>,
}

#[cfg(feature = "test-util")]
impl TestClock {
    /// Current monotonic evidence time.
    pub fn now(&self) -> Instant {
        *self.now.lock().expect("test clock lock")
    }

    /// Advance evidence time without sleeping or altering another cache's clock.
    pub fn advance(&self, duration: Duration) {
        let mut now = self.now.lock().expect("test clock lock");
        *now += duration;
    }
}

/// Contextual cache refusal; the later verifier owns its external error mapping.
#[derive(Debug)]
pub struct SessionKeyError {
    context: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    backtrace: Backtrace,
}

impl SessionKeyError {
    fn new(context: &'static str) -> Self {
        Self {
            context,
            source: None,
            backtrace: Backtrace::capture(),
        }
    }

    fn caused(
        context: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            context,
            source: Some(Box::new(source)),
            backtrace: Backtrace::capture(),
        }
    }
}

impl fmt::Display for SessionKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "session key cache: {}", self.context)?;
        if self.backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            write!(formatter, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for SessionKeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source.as_ref() as _)
    }
}
