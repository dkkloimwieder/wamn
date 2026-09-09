//! Session verification binds signed claims to the host's configured issuer and scope.
//!
//! The host resolves tenant permissions from the verified roles, then checks
//! admission immediately before creating its caller. Key and token deadlines
//! govern new admission only, never work that the host already admitted.

use std::backtrace::Backtrace;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use wamn_platform_identity::IdentityError;
use wamn_platform_identity::session_token::{
    SessionClaims, SessionScope, session_key_id, validate_session_age, verify_session_token,
};

use crate::session_keys::{IssuerKeys, KeyEvidence, SessionKeyError};

/// Host-owned token verifier; clones share the configured issuer's cache and budget.
#[derive(Clone, Debug)]
pub struct SessionVerifier {
    keys: IssuerKeys,
    org: Arc<str>,
    audience: Arc<str>,
    clock: Clock,
}

impl SessionVerifier {
    /// Bind an existing issuer cache to a trusted organization and exact audience.
    ///
    /// The host supplies both scope values from its own loaded configuration.
    /// Neither value comes from a bearer token or request parameter.
    pub fn new(
        keys: IssuerKeys,
        org: &str,
        audience: &str,
    ) -> Result<Self, SessionVerificationError> {
        if org.trim().is_empty() || audience.trim().is_empty() {
            return Err(SessionVerificationError::new("host session scope is empty"));
        }
        Ok(Self {
            keys,
            org: org.into(),
            audience: audience.into(),
            clock: Clock::System,
        })
    }

    /// Authenticate the fixed JWT profile without any identity database read.
    ///
    /// The untrusted key ID selects only within the configured issuer's keys.
    /// A cold or expired key cache can perform its bounded HTTPS refresh.
    /// The returned roles still require the host's fresh tenant permission read.
    pub async fn verify(&self, token: &str) -> Result<VerifiedSession, SessionVerificationError> {
        let kid = session_key_id(token)?;
        let evidence = self.keys.key(&kid).await?;
        let claims = verify_session_token(
            token,
            evidence.public_key(),
            SessionScope {
                issuer: self.keys.issuer(),
                org: &self.org,
                audience: &self.audience,
            },
            self.clock.now()?,
        )?;
        let session = VerifiedSession {
            claims,
            evidence,
            clock: self.clock.clone(),
        };
        session.check_admission()?;
        Ok(session)
    }

    /// Bind a deterministic token clock for tests, without changing cache time.
    #[cfg(feature = "test-util")]
    pub fn with_test_clock(
        keys: IssuerKeys,
        org: &str,
        audience: &str,
        now: i64,
    ) -> Result<(Self, SessionTestClock), SessionVerificationError> {
        let mut verifier = Self::new(keys, org, audience)?;
        let clock = SessionTestClock {
            now: Arc::new(std::sync::Mutex::new(now)),
        };
        verifier.clock = Clock::Fixed(clock.clone());
        Ok((verifier, clock))
    }
}

/// Signed claims with key evidence, before the host's final request admission.
pub struct VerifiedSession {
    claims: SessionClaims,
    evidence: KeyEvidence,
    clock: Clock,
}

impl VerifiedSession {
    /// Borrow the authenticated principal and roles for fresh permission resolution.
    pub fn claims(&self) -> &SessionClaims {
        &self.claims
    }

    /// Refuse if the token or key evidence expired during permission resolution.
    ///
    /// Call immediately before admitting a new request, after its last await.
    /// Do not call for nested operations within an already-admitted request.
    pub fn check_admission(&self) -> Result<(), SessionVerificationError> {
        if !self.evidence.is_fresh() {
            return Err(SessionVerificationError::new("issuer key evidence expired"));
        }
        validate_session_age(self.claims.iat, self.claims.exp, self.clock.now()?)?;
        Ok(())
    }
}

impl fmt::Debug for VerifiedSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedSession")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
enum Clock {
    System,
    #[cfg(feature = "test-util")]
    Fixed(SessionTestClock),
}

impl Clock {
    fn now(&self) -> Result<i64, SessionVerificationError> {
        match self {
            Self::System => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|source| SessionVerificationError::caused("read token clock", source))?
                .as_secs()
                .try_into()
                .map_err(|source| SessionVerificationError::caused("token clock overflow", source)),
            #[cfg(feature = "test-util")]
            Self::Fixed(clock) => Ok(*clock.now.lock().expect("session test clock lock")),
        }
    }
}

/// Explicit token time for tests; the JWKS cache keeps its own evidence clock.
#[cfg(feature = "test-util")]
#[derive(Clone, Debug)]
pub struct SessionTestClock {
    now: Arc<std::sync::Mutex<i64>>,
}

#[cfg(feature = "test-util")]
impl SessionTestClock {
    /// Set the token-validation time in Unix seconds.
    pub fn set(&self, now: i64) {
        *self.now.lock().expect("session test clock lock") = now;
    }
}

/// Internal refusal context; the host maps every refusal to the same HTTP 401.
#[derive(Debug)]
pub struct SessionVerificationError {
    context: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    backtrace: Backtrace,
}

impl SessionVerificationError {
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

impl From<IdentityError> for SessionVerificationError {
    fn from(source: IdentityError) -> Self {
        Self::caused("session profile refused", source)
    }
}

impl From<SessionKeyError> for SessionVerificationError {
    fn from(source: SessionKeyError) -> Self {
        Self::caused("issuer key evidence refused", source)
    }
}

impl fmt::Display for SessionVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "session verification: {}", self.context)?;
        if self.backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            write!(formatter, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for SessionVerificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source.as_ref() as _)
    }
}
