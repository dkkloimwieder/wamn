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

/// One deployment-owned application address and its exact session audience.
#[derive(Debug, Clone)]
pub struct EnvironmentTarget {
    pub audience: String,
    pub binding: wamn_client_tui::submission::SessionBinding,
}

/// Read the public deployment targets used after authenticated discovery.
///
/// # Errors
/// Refuses empty lists, duplicate audiences and malformed application addresses.
pub fn environment_targets(bytes: &[u8]) -> Result<Vec<EnvironmentTarget>, CredentialError> {
    let refused = || CredentialError::new("Receiving environment configuration refused");
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| refused())?;
    let rows = value
        .as_array()
        .filter(|rows| !rows.is_empty())
        .ok_or_else(refused)?;
    let mut audiences = std::collections::BTreeSet::new();
    let mut targets = Vec::new();
    for row in rows {
        let row = row.as_object().ok_or_else(refused)?;
        if row
            .keys()
            .any(|key| !["audience", "base_url", "host", "target_instance"].contains(&key.as_str()))
        {
            return Err(refused());
        }
        let field = |key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.trim().is_empty() && !text.chars().any(char::is_control))
                .ok_or_else(refused)
        };
        let audience = field("audience")?.to_owned();
        if !audiences.insert(audience.clone()) {
            return Err(refused());
        }
        let url = reqwest::Url::parse(field("base_url")?).map_err(|_| refused())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(refused());
        }
        let host = match row.get("host") {
            None | Some(serde_json::Value::Null) => None,
            Some(_) => {
                let host = field("host")?;
                reqwest::header::HeaderValue::from_str(host).map_err(|_| refused())?;
                Some(host.to_owned())
            }
        };
        targets.push(EnvironmentTarget {
            audience,
            binding: wamn_client_tui::submission::SessionBinding {
                url: url.to_string(),
                host,
                target_instance: field("target_instance")?.to_owned(),
            },
        });
    }
    Ok(targets)
}
