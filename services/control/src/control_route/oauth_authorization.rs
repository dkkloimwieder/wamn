//! Authorization code exchange and encrypted, single-use PKCE state.

use std::{fmt, sync::Arc, time::Duration};

use anyhow::Context as _;
use serde_json::Value;
use sqlx::Connection as _;
use url::Url;
use zeroize::Zeroizing;

use wamn_connection::connection_credentials::KubernetesStore;
use wamn_envelope::encryption::Envelope;
use wamn_http::{oauth, transport::HttpTransport};
use wamn_postgres::oauth_authorization as store;

/// A refusal with a safe public reason. Sources never enter a callback page.
pub struct Error {
    pub reason: String,
    source: Option<anyhow::Error>,
}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthAuthorizationError")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(AsRef::as_ref)
    }
}
fn refused(reason: impl Into<String>) -> Error {
    Error {
        reason: reason.into(),
        source: None,
    }
}
fn failed(reason: &'static str, source: impl Into<anyhow::Error>) -> Error {
    Error {
        reason: reason.into(),
        source: Some(source.into()),
    }
}

/// The control host's public callback address and shared envelope backend.
#[derive(Debug)]
pub struct Authorization {
    redirect_uri: String,
    envelope: Arc<Envelope>,
    transport: HttpTransport,
}

impl Authorization {
    /// Configure one public HTTP origin, without accepting request headers as authority.
    pub fn new(origin: &str, envelope: Arc<Envelope>) -> anyhow::Result<Self> {
        let url = Url::parse(origin)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid control public origin"
        );
        Ok(Self {
            redirect_uri: format!("{}/oauth/callback", url.origin().ascii_serialization()),
            envelope,
            transport: HttpTransport::new()?,
        })
    }

    /// The address that the administrator registers with the provider.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The caller holds the active connection row and its ProjectAdmin transaction.
    pub async fn authorize(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        context: &Value,
        caller: wamn_identity_protocol::PrincipalId,
    ) -> Result<String, Error> {
        let connection = context["id"]
            .as_str()
            .and_then(|id| id.parse::<uuid::Uuid>().ok())
            .ok_or_else(|| refused("connection not found"))?;
        let generation = context["active_generation"]
            .as_i64()
            .ok_or_else(|| refused("connection generation missing"))?;
        if context["auth_type"] != "oauth2_authorization_code" {
            return Err(refused("unsupported authentication type"));
        }
        let (url, state, verifier) = oauth::authorize(
            &context["parameters"]["auth"],
            &self.redirect_uri,
            &format!(
                "{}/{}",
                context["project"]
                    .as_str()
                    .ok_or_else(|| refused("missing project"))?,
                context["env"]
                    .as_str()
                    .ok_or_else(|| refused("missing environment"))?
            ),
        )
        .map_err(|error| failed("invalid authorization parameters", error))?;
        let verifier = Zeroizing::new(verifier);
        let encrypted = self
            .envelope
            .encrypt(verifier.as_bytes())
            .await
            .map_err(|error| failed("authorization state encryption failed", error))?;
        store::insert(tx, &state, connection, generation, &encrypted, caller)
            .await
            .map_err(|error| failed("authorization state write failed", error))?;
        Ok(url)
    }

    /// Roll back every refusal, including expired and stale-generation states.
    pub async fn callback(
        &self,
        database: &mut sqlx::PgConnection,
        query: &str,
    ) -> Result<(), Error> {
        let mut tx = database
            .begin()
            .await
            .map_err(|error| failed("callback transaction failed", error))?;
        match self.exchange(&mut tx, query).await {
            Ok(()) => tx
                .commit()
                .await
                .map_err(|error| failed("callback commit failed", error)),
            Err(error) => {
                tx.rollback()
                    .await
                    .map_err(|source| failed("callback rollback failed", source))?;
                Err(error)
            }
        }
    }

    async fn exchange(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        query: &str,
    ) -> Result<(), Error> {
        let query: Callback = callback_parameters(query)?;
        if let Some(error) = query.error {
            return Err(refused(format!("provider refused authorization: {error}")));
        }
        let state = query
            .state
            .filter(|state| !state.is_empty())
            .ok_or_else(|| refused("missing state"))?;
        let code = Zeroizing::new(
            query
                .code
                .filter(|code| !code.is_empty())
                .ok_or_else(|| refused("missing authorization code"))?,
        );
        let authorization = store::lock(tx, &state)
            .await
            .map_err(|error| failed("authorization state read failed", error))?
            .ok_or_else(|| refused("unknown or replayed state"))?;
        if !authorization.unexpired {
            return Err(refused("expired state"));
        }
        wamn_postgres::bind_transaction_identity(
            tx,
            &authorization.principal,
            "",
            "",
            Some("connection.oauth_callback"),
            None,
        )
        .await
        .map_err(|error| failed("callback identity failed", error))?;
        let connection = authorization.connection;
        let context = store::lock_connection(tx, connection)
            .await
            .map_err(|error| failed("connection unavailable", error))?
            .ok_or_else(|| refused("connection unavailable"))?;
        if context["active_generation"].as_i64() != Some(authorization.generation) {
            return Err(refused("connection generation changed"));
        }
        if context["auth_type"] != "oauth2_authorization_code" {
            return Err(refused("connection authentication type changed"));
        }
        store::delete(tx, &state)
            .await
            .map_err(|error| failed("authorization state delete failed", error))?;
        let verifier = self
            .envelope
            .decrypt(&authorization.verifier)
            .await
            .map_err(|error| failed("authorization state decryption failed", error))?;
        let verifier = std::str::from_utf8(verifier.expose())
            .map_err(|error| failed("invalid PKCE verifier", error))?;
        let material = credential(&context)
            .await
            .map_err(|error| failed("client credential unavailable", error))?;
        let token = oauth::exchange_code(
            &self.transport,
            &context["parameters"],
            &material,
            &self.redirect_uri,
            code.to_string(),
            verifier.to_owned(),
        )
        .await
        .map_err(|error| failed(error.reason, error))?;
        let received = tokio::time::Instant::now();
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&token.token)
                .map_err(|error| failed("token encoding failed", error))?,
        );
        let encrypted = self
            .envelope
            .encrypt(&plaintext)
            .await
            .map_err(|error| failed("token encryption failed", error))?;
        wamn_postgres::project_connections::write_token(
            tx,
            connection,
            &encrypted,
            (Duration::from_secs(token.expires_in.unwrap_or(3600)).as_secs_f64()
                - received.elapsed().as_secs_f64())
            .max(0.0),
        )
        .await
        .map_err(|error| failed("token write failed", error))?;
        Ok(())
    }
}

async fn credential(context: &Value) -> anyhow::Result<Zeroizing<String>> {
    let reference = &context["credential"];
    let text = |field: &str| {
        reference[field]
            .as_str()
            .context("credential reference missing")
    };
    let reference = text("ref")?;
    let version = text("version")?;
    let bytes = KubernetesStore.read(reference, version).await?;
    String::from_utf8(bytes)
        .context("credential is not UTF-8")
        .map(Zeroizing::new)
}

/// Select only the project and environment named by an opaque authorization state.
/// The matching state row in that database remains the authority for the exchange.
///
/// # Errors
/// Returns a refusal for missing, duplicate or malformed state coordinates.
pub fn callback_target(
    query: &str,
) -> Result<(wamn_registry::ProjectId, wamn_registry::EnvName), Error> {
    let state = callback_parameters(query)?
        .state
        .ok_or_else(|| refused("missing state"))?;
    let mut parts = state.split('/');
    let project = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| refused("invalid state project"))?;
    let environment = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| refused("invalid state environment"))?;
    if parts.next().is_none_or(str::is_empty) || parts.next().is_some() {
        return Err(refused("invalid state"));
    }
    Ok((project, environment))
}

struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
fn callback_parameters(query: &str) -> Result<Callback, Error> {
    let mut callback = Callback {
        state: None,
        code: None,
        error: None,
    };
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let field = match key.as_ref() {
            "state" => &mut callback.state,
            "code" => &mut callback.code,
            "error" => &mut callback.error,
            _ => continue,
        };
        if field.replace(value.into_owned()).is_some() {
            return Err(refused("duplicate callback parameter"));
        }
    }
    Ok(callback)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_parameters_refuse_duplicates() {
        assert!(callback_parameters("state=a&state=b").is_err());
        assert!(callback_parameters("state=a%2Bb&code=c%26d").is_ok());
    }
}
