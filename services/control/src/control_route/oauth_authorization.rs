//! Authorization code exchange and encrypted, single-use PKCE state.

use std::{fmt, time::Duration};

use anyhow::Context as _;
use serde_json::Value;
use sqlx::{Connection as _, PgConnection, postgres::PgConnectOptions};
use tokio::time::{Instant, sleep, timeout, timeout_at};
use url::Url;
use wamn_app::MAX_HOST_CALL_DURATION;
use zeroize::Zeroizing;

use super::oauth_authorization_store as store;
use wamn_connection::connection_credentials::KubernetesStore;
use wamn_connection::project_connections::{
    ConnectionEvent, ConnectionRow, EventOutcome, EventType, OAUTH_LEASE_POLL_INTERVAL, OAuthLease,
    lock_active_generation,
};
use wamn_envelope::encryption::{Encrypted, Envelope};
use wamn_http::{oauth, transport::HttpTransport};
use wamn_registry::{EnvName, ProjectId};

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

pub(super) struct PreparedAuthorization {
    url: String,
    state: String,
    connection: uuid::Uuid,
    generation: i64,
    encrypted: Encrypted,
}

impl PreparedAuthorization {
    /// Store the state only if the connection still has the authorized generation.
    pub(super) async fn store(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        caller: wamn_identity_protocol::PrincipalId,
    ) -> Result<String, Error> {
        let current = lock_active_generation(tx, self.connection)
            .await
            .map_err(|error| failed("authorization connection check failed", error))?;
        if current != Some(self.generation) {
            return Err(refused("connection changed during authorization"));
        }
        store::insert(
            tx,
            &self.state,
            self.connection,
            self.generation,
            &self.encrypted,
            caller,
        )
        .await
        .map_err(|error| failed("authorization state write failed", error))?;
        ConnectionEvent {
            connection: self.connection,
            generation: self.generation,
            event_type: EventType::OAuthAuthorize,
            principal: Some(caller),
        }
        .append(tx, EventOutcome::Succeeded)
        .await
        .map_err(|error| failed("authorization event write failed", error))?;
        Ok(self.url)
    }
}

struct CallbackToken {
    encrypted: Encrypted,
    lifetime: Duration,
    received: Instant,
}

/// The control host's public callback address and shared envelope backend.
#[derive(Debug)]
pub struct Authorization {
    redirect_uri: String,
    envelope: Option<Envelope>,
    transport: HttpTransport,
}

impl Authorization {
    /// Configure one public HTTP origin, without accepting request headers as authority.
    pub fn new(origin: &str, envelope: Option<Envelope>) -> anyhow::Result<Self> {
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

    pub(super) fn shared_envelope(&self) -> Option<&Envelope> {
        self.envelope.as_ref()
    }

    /// Encrypt the verifier after the read transaction ends.
    pub(super) async fn prepare_authorization(
        &self,
        envelope: &Envelope,
        context: &ConnectionRow,
        project: &ProjectId,
        env: &EnvName,
    ) -> Result<PreparedAuthorization, Error> {
        let (url, state, verifier) = oauth::authorize(
            &context.parameters["auth"],
            &self.redirect_uri,
            &format!("{project}/{env}"),
        )
        .map_err(|error| failed("invalid authorization parameters", error))?;
        let verifier = Zeroizing::new(verifier);
        let encrypted = envelope
            .encrypt(verifier.as_bytes())
            .await
            .map_err(|error| failed("authorization state encryption failed", error))?;
        Ok(PreparedAuthorization {
            url,
            state,
            connection: context.id,
            generation: context.active_generation,
            encrypted,
        })
    }

    /// Exchange the code outside SQL using the connection's shared OAuth lease.
    pub async fn callback(
        &self,
        login: &PgConnectOptions,
        mut database: PgConnection,
        envelope: &Envelope,
        namespace: &str,
        query: &str,
    ) -> Result<(), Error> {
        let query = callback_parameters(query)?;
        let state = query
            .state
            .as_deref()
            .filter(|state| !state.is_empty())
            .ok_or_else(|| refused("missing state"))?;
        let code = match query.error {
            Some(error) => Err(refused(format!("provider refused authorization: {error}"))),
            None => query
                .code
                .filter(|code| !code.is_empty())
                .map(Zeroizing::new)
                .ok_or_else(|| refused("missing authorization code")),
        };
        let pending = store::find(&mut database, state)
            .await
            .map_err(|error| failed("authorization state read failed", error))?;
        let Some((connection, generation, principal)) = pending else {
            code?;
            return Err(refused("connection or authorization state unavailable"));
        };
        let event = ConnectionEvent {
            connection,
            generation,
            event_type: EventType::OAuthCallback,
            principal: Some(principal.into()),
        };
        let code = match code {
            Ok(code) => code,
            Err(error) => {
                event
                    .record_refusal(&mut database, EventOutcome::Refused)
                    .await;
                return Err(error);
            }
        };
        let prepared = wait_for_callback_lease(login, database, state).await;
        let (context, authorization, lease) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                record_callback_failure(login, &event, None, &error).await;
                return Err(error);
            }
        };
        let result = async {
            let token = timeout_at(
                lease.work_deadline(),
                self.exchange(envelope, namespace, &code, &context, &authorization),
            )
            .await
            .map_err(|error| failed("authorization exchange timed out", error))??;
            publish_callback(login, state, &authorization, &event, &lease, &token).await
        }
        .await;
        if let Err(error) = &result {
            record_callback_failure(login, &event, Some(&lease), error).await;
        }
        result
    }

    async fn exchange(
        &self,
        envelope: &Envelope,
        namespace: &str,
        code: &str,
        context: &Value,
        authorization: &store::LockedAuthorization,
    ) -> Result<CallbackToken, Error> {
        let verifier = envelope
            .decrypt(&authorization.verifier)
            .await
            .map_err(|error| failed("authorization state decryption failed", error))?;
        let verifier = std::str::from_utf8(verifier.expose())
            .map_err(|error| failed("invalid PKCE verifier", error))?;
        let material = credential(namespace, context)
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
        let received = Instant::now();
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&token.token)
                .map_err(|error| failed("token encoding failed", error))?,
        );
        let encrypted = envelope
            .encrypt(&plaintext)
            .await
            .map_err(|error| failed("token encryption failed", error))?;

        Ok(CallbackToken {
            encrypted,
            lifetime: Duration::from_secs(token.expires_in.unwrap_or(3600)),
            received,
        })
    }
}

async fn callback_connection(login: &PgConnectOptions) -> Result<PgConnection, Error> {
    PgConnection::connect_with(login)
        .await
        .map_err(|error| failed("callback database unavailable", error))
}

async fn wait_for_callback_lease(
    login: &PgConnectOptions,
    mut database: PgConnection,
    state: &str,
) -> Result<(Value, store::LockedAuthorization, OAuthLease), Error> {
    timeout(MAX_HOST_CALL_DURATION, async {
        loop {
            let snapshot = claim_callback(&mut database, state).await?;
            drop(database);
            if let Some(snapshot) = snapshot {
                return Ok(snapshot);
            }
            sleep(OAUTH_LEASE_POLL_INTERVAL).await;
            database = callback_connection(login).await?;
        }
    })
    .await
    .map_err(|error| failed("authorization lease wait timed out", error))?
}

async fn claim_callback(
    database: &mut PgConnection,
    state: &str,
) -> Result<Option<(Value, store::LockedAuthorization, OAuthLease)>, Error> {
    let mut tx = database
        .begin()
        .await
        .map_err(|error| failed("callback transaction failed", error))?;
    // A generation write locks the connection before deleting pending states.
    let context = store::lock_connection(&mut tx, state)
        .await
        .map_err(|error| failed("connection unavailable", error))?
        .ok_or_else(|| refused("connection or authorization state unavailable"))?;
    let authorization = store::lock(&mut tx, state)
        .await
        .map_err(|error| failed("authorization state read failed", error))?
        .ok_or_else(|| refused("unknown or replayed state"))?;
    if !authorization.unexpired {
        return Err(refused("expired state"));
    }
    if context["active_generation"].as_i64() != Some(authorization.generation) {
        return Err(refused("connection generation changed"));
    }
    if context["auth_type"] != "oauth2_authorization_code" {
        return Err(refused("connection authentication type changed"));
    }
    let lease = OAuthLease::claim(&mut tx, authorization.connection)
        .await
        .map_err(|error| failed("authorization lease claim failed", error))?;
    tx.commit()
        .await
        .map_err(|error| failed("callback snapshot failed", error))?;
    Ok(lease.map(|lease| (context, authorization, lease)))
}

async fn publish_callback(
    login: &PgConnectOptions,
    state: &str,
    authorization: &store::LockedAuthorization,
    event: &ConnectionEvent,
    lease: &OAuthLease,
    token: &CallbackToken,
) -> Result<(), Error> {
    let mut database = callback_connection(login).await?;
    let mut tx = database
        .begin()
        .await
        .map_err(|error| failed("callback transaction failed", error))?;
    wamn_postgres::bind_transaction_identity(
        &mut tx,
        &authorization.principal,
        "",
        "",
        Some("connection.oauth_callback"),
        None,
    )
    .await
    .map_err(|error| failed("callback identity failed", error))?;
    let current = lock_active_generation(&mut tx, authorization.connection)
        .await
        .map_err(|error| failed("connection unavailable", error))?;
    if current != Some(authorization.generation) {
        return Err(refused("connection generation changed"));
    }
    let remaining =
        (token.lifetime.as_secs_f64() - token.received.elapsed().as_secs_f64()).max(0.0);
    if !lease
        .write_token(&mut tx, &token.encrypted, remaining)
        .await
        .map_err(|error| failed("token write failed", error))?
    {
        return Err(refused("authorization lease expired"));
    }
    if !store::delete(&mut tx, state)
        .await
        .map_err(|error| failed("authorization state delete failed", error))?
    {
        return Err(refused("unknown or replayed state"));
    }
    event
        .append(&mut tx, EventOutcome::Succeeded)
        .await
        .map_err(|error| failed("callback event write failed", error))?;
    tx.commit()
        .await
        .map_err(|error| failed("callback commit failed", error))
}

async fn record_callback_failure(
    login: &PgConnectOptions,
    event: &ConnectionEvent,
    lease: Option<&OAuthLease>,
    error: &Error,
) {
    let outcome = if error.source.is_some() {
        EventOutcome::Failed
    } else {
        EventOutcome::Refused
    };
    let record = async {
        let mut database = callback_connection(login).await?;
        if let Some(lease) = lease {
            lease
                .record_failure(&mut database, event, outcome)
                .await
                .map_err(|error| failed("callback lease release failed", error))?;
        } else {
            event.record_refusal(&mut database, outcome).await;
        }
        Ok::<_, Error>(())
    };
    if !timeout(Duration::from_secs(1), record)
        .await
        .is_ok_and(|result| result.is_ok())
    {
        tracing::warn!("OAuth callback failure could not be recorded");
    }
}

async fn credential(namespace: &str, context: &Value) -> anyhow::Result<Zeroizing<String>> {
    let reference = &context["credential"];
    let text = |field: &str| {
        reference[field]
            .as_str()
            .context("credential reference missing")
    };
    let reference = text("ref")?;
    let version = text("version")?;
    let bytes = KubernetesStore::in_namespace(namespace)
        .read(reference, version)
        .await?;
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
    let env = parts
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| refused("invalid state environment"))?;
    if parts.next().is_none_or(str::is_empty) || parts.next().is_some() {
        return Err(refused("invalid state"));
    }
    Ok((project, env))
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
