//! In-memory issuer keys for tests of session admission.

use crate::{
    keys::PublicSessionKey,
    token::SessionClaims,
    verifier::{KeyEvidence, KeySource, SessionVerificationError, SessionVerifier},
};
use async_trait::async_trait;

#[derive(Clone, Copy, Debug)]
pub struct Scope<'a> {
    pub issuer: &'a str,
    pub org: &'a str,
    pub audience: &'a str,
}

#[derive(Debug)]
struct Keys<'a> {
    issuer: &'a str,
    key: &'a PublicSessionKey,
}
#[derive(Debug)]
struct Evidence<'a>(&'a PublicSessionKey);
impl KeyEvidence for Evidence<'_> {
    fn public_key(&self) -> &PublicSessionKey {
        self.0
    }
    fn is_fresh(&self) -> bool {
        true
    }
}
#[async_trait]
impl<'a> KeySource for Keys<'a> {
    type Evidence = Evidence<'a>;
    type Error = std::io::Error;
    fn issuer(&self) -> &str {
        self.issuer
    }
    async fn key(&self, kid: &str) -> Result<Self::Evidence, Self::Error> {
        if kid != self.key.kid {
            return Err(std::io::Error::other("unknown session key"));
        }
        Ok(Evidence(self.key))
    }
}

/// Verify through the public verifier with a fixed token clock.
pub async fn verify(
    token: &str,
    key: &PublicSessionKey,
    scope: Scope<'_>,
    now: i64,
) -> Result<SessionClaims, SessionVerificationError> {
    let (verifier, _) = SessionVerifier::with_test_clock(
        Keys {
            issuer: scope.issuer,
            key,
        },
        scope.org,
        scope.audience,
        now,
    )?;
    Ok(verifier.verify(token).await?.claims)
}
