//! Select the operator credential and finish session login before terminal startup.

use std::sync::Arc;

use wamn_client::credentials::{CredentialError, SessionCredentials, SessionTarget};
use wamn_client::{CredentialProvider, StaticPat, Transport};

/// Parse the explicit issuer and audience pair without reading process environment.
///
/// # Errors
///
/// Refuses incomplete or invalid session configuration.
pub fn session_target(
    issuer: Option<&str>,
    audience: Option<&str>,
) -> Result<Option<SessionTarget>, CredentialError> {
    match (issuer, audience) {
        (None, None) => Ok(None),
        (Some(issuer), Some(audience)) => SessionTarget::new(issuer, audience).map(Some),
        _ => Err(CredentialError::new(
            "WAMN_SESSION_ISSUER and WAMN_SESSION_AUDIENCE must be configured together",
        )),
    }
}

/// Keep the PAT source and complete the optional session login before returning.
///
/// # Errors
///
/// Refuses a missing PAT or a failed session exchange without falling back to PAT mode.
pub async fn credentials(
    token: String,
    target: Option<SessionTarget>,
    transport: Arc<dyn Transport>,
) -> Result<Arc<dyn CredentialProvider>, CredentialError> {
    let pat: Arc<dyn CredentialProvider> = Arc::new(StaticPat::new(token)?);
    let Some(target) = target else {
        return Ok(pat);
    };
    let session = SessionCredentials::new(target, pat, transport);
    session.login().await?;
    Ok(Arc::new(session))
}
