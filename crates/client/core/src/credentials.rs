//! Where a caller's credential comes from.
//!
//! A PROVIDER, not a token. The v1 implementation holds a static PAT, but the
//! interface exists so a refreshing source can replace it without touching a
//! call site — and so nothing above this layer stores a credential itself.

/// Why a credential could not be supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialError {
    detail: String,
}

impl CredentialError {
    /// A refusal carrying its reason.
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl core::fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "credential unavailable: {}", self.detail)
    }
}

impl std::error::Error for CredentialError {}

/// Supplies the bearer credential for one request.
#[async_trait::async_trait]
pub trait CredentialProvider: Send + Sync + core::fmt::Debug {
    /// The bearer token to present. Called per request, so a refreshing
    /// implementation can rotate without the caller knowing.
    ///
    /// # Errors
    ///
    /// [`CredentialError`] when no credential can be supplied.
    async fn bearer(&self) -> Result<String, CredentialError>;
}

/// The v1 implementation: one static personal access token.
pub struct StaticPat {
    token: String,
}

impl StaticPat {
    /// Hold one token.
    ///
    /// # Errors
    ///
    /// [`CredentialError`] when the token is empty — an empty bearer would be
    /// sent as a well-formed header and refused as an authentication failure,
    /// which reads as a server problem rather than a missing configuration.
    pub fn new(token: impl Into<String>) -> Result<Self, CredentialError> {
        let token = token.into();
        if token.is_empty() {
            return Err(CredentialError::new("the token is empty"));
        }
        Ok(Self { token })
    }
}

impl core::fmt::Debug for StaticPat {
    /// The token is never rendered. A client is the layer most likely to end
    /// up in a log line.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("StaticPat").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl CredentialProvider for StaticPat {
    async fn bearer(&self) -> Result<String, CredentialError> {
        Ok(self.token.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_static_pat_supplies_its_token() {
        let provider = StaticPat::new("pat-abc").expect("a non-empty token is accepted");
        assert_eq!(provider.bearer().await.expect("bearer"), "pat-abc");
    }

    #[test]
    fn an_empty_token_is_refused_at_construction() {
        assert!(StaticPat::new("").is_err());
    }

    /// The credential must not reach a log through `Debug`.
    #[test]
    fn debug_never_renders_the_token() {
        let provider = StaticPat::new("pat-secret-value").expect("token");
        let rendered = format!("{provider:?}");
        assert!(!rendered.contains("pat-secret-value"), "{rendered}");
    }
}
