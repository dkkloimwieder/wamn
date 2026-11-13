//! Authorization code exchange and encrypted, single-use PKCE state.

use std::{fmt, sync::Arc, time::Duration};
use wamn_kubernetes::Store as _;

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio_postgres::{Client, Transaction};
use url::Url;
use zeroize::Zeroizing;

use crate::oauth_encryption::{Encrypted, Envelope};

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
    transport: Arc<crate::plugins::connection_http::transport::HttpTransport>,
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
            transport: Arc::new(crate::plugins::connection_http::transport::HttpTransport::new()?),
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
    pub fn with_transport(
        mut self,
        transport: Arc<crate::plugins::connection_http::transport::HttpTransport>,
    ) -> Self {
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
        tx: &Transaction<'_>,
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
        let auth: Parameters = serde_json::from_value(context["parameters"]["auth"].clone())
            .map_err(|error| failed("invalid authorization parameters", error))?;
        let state = random()?;
        // Thirty-two random bytes encode to 43 unreserved characters (RFC 7636).
        let verifier = Zeroizing::new(random()?);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = provider_url(&auth.authorize_url)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", &auth.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("scope", &auth.scopes.join(" ")),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        let encrypted = self
            .envelope
            .encrypt(verifier.as_bytes())
            .await
            .map_err(|error| failed("authorization state encryption failed", error))?;
        tx.execute("INSERT INTO identity.oauth_authorization(state,connection_id,generation,code_verifier,wrapped_key,nonce,key_id,created_by,expires_at) \
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+interval '10 minutes')",
            &[&state,&connection,&generation,&encrypted.ciphertext,&encrypted.wrapped_key,&encrypted.nonce,&encrypted.key_id,&caller])
            .await.map_err(|error| failed("authorization state write failed", error))?;
        Ok(url.into())
    }

    /// Roll back every refusal, including expired and stale-generation states.
    pub async fn callback(
        &self,
        database: &mut Client,
        org: &str,
        query: &str,
    ) -> Result<(), Error> {
        let tx = database
            .transaction()
            .await
            .map_err(|error| failed("callback transaction failed", error))?;
        match self.exchange(&tx, org, query).await {
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

    async fn exchange(&self, tx: &Transaction<'_>, org: &str, query: &str) -> Result<(), Error> {
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
        let row = tx.query_opt("SELECT connection_id,generation,code_verifier,wrapped_key,nonce,key_id,expires_at>clock_timestamp() AS unexpired \
            FROM identity.oauth_authorization WHERE state=$1 FOR UPDATE", &[&state]).await
            .map_err(|error| failed("authorization state read failed", error))?.ok_or_else(|| refused("unknown or replayed state"))?;
        if !row.get::<_, bool>("unexpired") {
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
        tx.execute(
            "DELETE FROM identity.oauth_authorization WHERE state=$1",
            &[&state],
        )
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
        let auth: Parameters = serde_json::from_value(context["parameters"]["auth"].clone())
            .map_err(|error| failed("invalid authorization parameters", error))?;
        let material = self
            .credential(&context)
            .await
            .map_err(|error| failed("client credential unavailable", error))?;
        let secret: Credential = serde_json::from_str(&material)
            .map_err(|error| failed("invalid client credential", error))?;
        let form = Zeroizing::new(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs([
                    ("grant_type", "authorization_code"),
                    ("code", code.as_str()),
                    ("redirect_uri", &self.redirect_uri),
                    ("client_id", &auth.client_id),
                    ("client_secret", &secret.client_secret),
                    ("code_verifier", verifier),
                ])
                .finish(),
        );
        let parameters = &context["parameters"];
        let tls = match parameters["tls-verification"].as_str() {
            Some("disabled") => crate::connection_authority::TlsPolicy::Disabled,
            Some("verify-authority") => crate::connection_authority::TlsPolicy::VerifyAuthority,
            _ => return Err(refused("invalid connection TLS policy")),
        };
        let authority = crate::connection_authority::parse_http_connection_authority(
            parameters["primary-authority"]
                .as_str()
                .ok_or_else(|| refused("missing connection authority"))?,
            tls,
            None,
        )
        .map_err(|error| failed("invalid connection authority", error))?;
        let decision = crate::plugins::connection_http::token_endpoint(
            &authority,
            auth.token_authority.as_deref(),
            &auth.token_url,
            None,
        )
        .await
        .map_err(|error| failed("token endpoint authority refused", error))?;
        let request = hyper::Request::builder()
            .method("POST")
            .uri(decision.logical_url.as_ref())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form.as_bytes())
            .map_err(|error| failed("invalid token request", error))?;
        let response = self
            .transport
            .send_once(&decision, request)
            .await
            .map_err(|error| failed("provider token endpoint unavailable", error))?;
        let status = response.status();
        let bytes = Zeroizing::new(response.into_body().to_vec());
        if let Ok(error) = serde_json::from_slice::<ProviderError>(&bytes) {
            return Err(refused(format!(
                "provider token exchange failed: {}",
                error.error
            )));
        }
        if !status.is_success() {
            return Err(refused(format!(
                "provider token exchange failed (HTTP {})",
                status.as_u16()
            )));
        }
        let token: Response = serde_json::from_slice(&bytes)
            .map_err(|error| failed("invalid provider token response", error))?;
        if !token.token_type.eq_ignore_ascii_case("bearer") || token.access_token.is_empty() {
            return Err(refused("invalid provider bearer token"));
        }
        let received = tokio::time::Instant::now();
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&crate::oauth_token::Token {
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
        crate::oauth_token::write_row(
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
        #[cfg(any(test, feature = "test-util"))]
        if let Some(store) = &self.store {
            return Ok(Zeroizing::new(String::from_utf8(
                store.get(text("ref")?, text("version")?).await?,
            )?));
        }
        let store = wamn_kubernetes::kubernetes::KubernetesStore::in_cluster()?;
        let bytes = store.get(text("ref")?, text("version")?).await?;
        Ok(Zeroizing::new(String::from_utf8(bytes)?))
    }
}

#[derive(Deserialize)]
struct Parameters {
    client_id: String,
    authorize_url: String,
    token_url: String,
    token_authority: Option<String>,
    scopes: Vec<String>,
}
#[derive(Deserialize)]
struct Credential {
    client_secret: String,
}
impl Drop for Credential {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.client_secret.zeroize();
    }
}
#[derive(Deserialize)]
struct ProviderError {
    error: String,
}
#[derive(Deserialize)]
struct Response {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    token_type: String,
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
fn random() -> Result<String, Error> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| refused("authorization entropy unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn provider_url(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|error| failed("invalid provider URL", error))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(refused("invalid provider URL"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entropy_and_callback_parameters() {
        let a = random().unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), 32);
        assert_eq!(a.len(), 43);
        assert!(
            a.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
        assert_ne!(a, random().unwrap());
        assert!(callback_parameters("state=a&state=b").is_err());
        assert_eq!(
            callback_parameters("state=a%2Bb&code=c%26d")
                .unwrap()
                .state
                .as_deref(),
            Some("a+b")
        );
    }
}
