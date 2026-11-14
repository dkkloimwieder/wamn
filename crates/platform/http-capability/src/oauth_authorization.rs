//! Authorization code exchange and encrypted, single-use PKCE state.

use std::{fmt, sync::Arc, time::Duration};

use anyhow::Context as _;
use serde_json::{Value, json};
use sqlx::{Connection as _, Row as _};
use url::Url;
use zeroize::Zeroizing;

use wamn_connections::oauth_encryption::{Encrypted, Envelope};

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
    transport: Arc<crate::transport::HttpTransport>,
    #[cfg(any(test, feature = "test-util"))]
    store: Option<Arc<dyn wamn_kubernetes::Store>>,
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
            #[cfg(any(test, feature = "test-util"))]
            store: None,
            transport: Arc::new(crate::transport::HttpTransport::new()?),
        })
    }

    /// Inject a private fixture store without changing process environment.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_store(mut self, store: Arc<dyn wamn_kubernetes::Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Use a fixture's private TLS roots for pinned token exchanges.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_transport(mut self, transport: Arc<crate::transport::HttpTransport>) -> Self {
        self.transport = transport;
        self
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
        caller: uuid::Uuid,
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
        let (url, state, verifier) =
            crate::oauth::authorize(&context["parameters"]["auth"], &self.redirect_uri)
                .map_err(|error| failed("invalid authorization parameters", error))?;
        let verifier = Zeroizing::new(verifier);
        let encrypted = self
            .envelope
            .encrypt(verifier.as_bytes())
            .await
            .map_err(|error| failed("authorization state encryption failed", error))?;
        sqlx::query("INSERT INTO identity.oauth_authorization(state,connection_id,generation,code_verifier,wrapped_key,nonce,key_id,created_by,expires_at) \
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+interval '10 minutes')").bind(&state).bind(connection).bind(generation).bind(&encrypted.ciphertext).bind(&encrypted.wrapped_key).bind(&encrypted.nonce).bind(&encrypted.key_id).bind(caller).execute(&mut **tx)
            .await.map_err(|error| failed("authorization state write failed", error))?;
        Ok(url)
    }

    /// Roll back every refusal, including expired and stale-generation states.
    pub async fn callback(
        &self,
        database: &mut sqlx::PgConnection,
        org: &str,
        query: &str,
    ) -> Result<(), Error> {
        let mut tx = database
            .begin()
            .await
            .map_err(|error| failed("callback transaction failed", error))?;
        match self.exchange(&mut tx, org, query).await {
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
        org: &str,
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
        let row = sqlx::query("SELECT connection_id,generation,code_verifier,wrapped_key,nonce,key_id,expires_at>clock_timestamp() AS unexpired \
            FROM identity.oauth_authorization WHERE state=$1 FOR UPDATE").bind(&state).fetch_optional(&mut **tx).await
            .map_err(|error| failed("authorization state read failed", error))?.ok_or_else(|| refused("unknown or replayed state"))?;
        if !row.get::<bool, _>("unexpired") {
            return Err(refused("expired state"));
        }
        let connection: uuid::Uuid = row.get("connection_id");
        let context = wamn_platform_identity::connections::administration::configure(
            tx,
            org,
            "connection.oauth_callback",
            &json!({"id":connection}),
        )
        .await
        .map_err(|error| failed("connection unavailable", error))?;
        if context["active_generation"].as_i64() != Some(row.get("generation")) {
            return Err(refused("connection generation changed"));
        }
        if context["auth_type"] != "oauth2_authorization_code" {
            return Err(refused("connection authentication type changed"));
        }
        sqlx::query("DELETE FROM identity.oauth_authorization WHERE state=$1")
            .bind(&state)
            .execute(&mut **tx)
            .await
            .map_err(|error| failed("authorization state delete failed", error))?;
        let verifier = self
            .envelope
            .decrypt(&Encrypted {
                ciphertext: row.get("code_verifier"),
                wrapped_key: row.get("wrapped_key"),
                nonce: row.get("nonce"),
                key_id: row.get("key_id"),
            })
            .await
            .map_err(|error| failed("authorization state decryption failed", error))?;
        let verifier = std::str::from_utf8(verifier.expose())
            .map_err(|error| failed("invalid PKCE verifier", error))?;
        let material = self
            .credential(&context)
            .await
            .map_err(|error| failed("client credential unavailable", error))?;
        let request = crate::oauth::Request::code(
            &context["parameters"]["auth"],
            &material,
            &self.redirect_uri,
            code.to_string(),
            verifier.to_owned(),
        )
        .map_err(|error| failed("invalid authorization parameters", error))?;
        let parameters = &context["parameters"];
        let authority =
            wamn_capability_authority::connection_authority::parse_http_definition(parameters)
                .map_err(|error| failed("invalid connection authority", error))?;
        let decision = crate::token_endpoint(
            &authority,
            request.token_authority.as_deref(),
            &request.token_url,
            None,
        )
        .await
        .map_err(|error| failed("token endpoint authority refused", error))?;
        let token = request
            .send(&crate::oauth::Transport {
                http: (*self.transport).clone(),
                decision,
                identity: None,
            })
            .await
            .map_err(|error| failed("invalid provider token response", error))?;
        let received = tokio::time::Instant::now();
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&wamn_connections::oauth_token::Token {
                access_token: Some(token.access_token),
                refresh_token: token.refresh_token,
            })
            .map_err(|error| failed("token encoding failed", error))?,
        );
        let encrypted = self
            .envelope
            .encrypt(&plaintext)
            .await
            .map_err(|error| failed("token encryption failed", error))?;
        wamn_connections::oauth_token::write_row(
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

    async fn credential(&self, context: &Value) -> anyhow::Result<Zeroizing<String>> {
        let reference = &context["credential"];
        let text = |field: &str| {
            reference[field]
                .as_str()
                .context("credential reference missing")
        };
        let store = None;
        #[cfg(any(test, feature = "test-util"))]
        let store = self.store.as_deref().or(store);
        wamn_connections::connection_credentials::read_secret(text("ref")?, text("version")?, store)
            .await
            .map(Zeroizing::new)
    }
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
