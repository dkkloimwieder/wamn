//! Key-source contracts and temporary refusal of legacy session admission.

use crate::keys::PublicSessionKey;
use async_trait::async_trait;
use std::fmt;

/// Where a verifier gets the issuer's public keys.
///
/// The cloud source fetches them over HTTPS (`IssuerKeys` in `wamn-authentication`).
/// The edge source reads them from a file ([`FileKeys`](crate::file_keys::FileKeys)).
#[async_trait]
pub trait KeySource: Send + Sync {
    /// The key and its freshness.
    type Evidence: KeyEvidence;
    /// A refused key lookup.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The exact configured issuer. A key ID selects only within its keys.
    fn issuer(&self) -> &str;

    /// The key with this untrusted key ID, from the issuer's complete key set.
    async fn key(&self, kid: &str) -> Result<Self::Evidence, Self::Error>;
}

/// A public key and whether it may still admit a new request.
pub trait KeyEvidence: Send + Sync {
    /// Public-only JWK. Possession does not by itself authorize admission.
    fn public_key(&self) -> &PublicSessionKey;

    /// Test again immediately at final admission.
    fn is_fresh(&self) -> bool;
}

/// Legacy verifier placeholder, which refuses sessions until replacement.
#[derive(Clone, Debug)]
pub struct SessionVerifier<K> {
    _keys: K,
}

impl<K: KeySource> SessionVerifier<K> {
    /// Retain configured key ownership while legacy session admission is closed.
    pub fn new(keys: K, org: &str, audience: &str) -> Result<Self, SessionVerificationError> {
        if org.trim().is_empty() || audience.trim().is_empty() {
            return Err(SessionVerificationError("host session audience is empty"));
        }
        Ok(Self { _keys: keys })
    }

    /// Refuse every session until RFC 9068 verification is installed.
    pub async fn verify(&self, _token: &str) -> Result<(), SessionVerificationError> {
        Err(SessionVerificationError(
            "session verification is unavailable",
        ))
    }
}

/// Legacy session admission is unavailable.
#[derive(Debug)]
pub struct SessionVerificationError(&'static str);

impl fmt::Display for SessionVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for SessionVerificationError {}
