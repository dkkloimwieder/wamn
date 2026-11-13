//! Shared OAuth token refresh, serialized with connection generation writes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::connection_credentials::ConnectionCredentials;
use crate::oauth_encryption::{Encrypted, Envelope};

pub(crate) type CacheKey = (uuid::Uuid, i64);

#[derive(Default)]
pub(crate) struct Cache {
    pub entries: Mutex<HashMap<CacheKey, (String, Instant)>>,
}

/// JSON encrypted in oauth_token.ciphertext, also used by authorization callbacks.
/// A null access token records a failed first fetch without inventing a token.
#[derive(Default, Serialize, Deserialize)]
pub struct Token {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

pub(crate) struct Fetched {
    pub token: Token,
    pub expires_in: Option<u64>,
}

pub(crate) struct Available {
    pub token: String,
    pub retain_until: Option<Instant>,
}

/// A shared refresh failure. Debug output omits provider and token material.
pub(crate) struct Error {
    pub unauthorized: bool,
    connection: uuid::Uuid,
    generation: i64,
    source: anyhow::Error,
}
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokenError")
            .field("connection", &self.connection)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "OAuth token unavailable for connection {} generation {}",
            self.connection, self.generation
        )
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}
pub(crate) fn is_oauth(auth_type: &str) -> bool {
    matches!(
        auth_type,
        "oauth2_client_credentials" | "oauth2_client_credentials_jwt" | "oauth2_authorization_code"
    )
}

/// The access token stops being reusable at the sixty-second refresh margin.
fn retain_until(started: Instant, seconds_left: f64) -> Option<Instant> {
    Duration::try_from_secs_f64(seconds_left - 60.0)
        .ok()
        .and_then(|duration| started.checked_add(duration))
        .filter(|expires| *expires > Instant::now())
}

pub(crate) struct Request<'a> {
    pub reader: &'a ConnectionCredentials,
    pub project: &'a str,
    pub environment: &'a str,
    pub connection: uuid::Uuid,
    pub generation: i64,
    pub authorization_code: bool,
    /// A 401 refreshes only the access token that the provider refused.
    pub rejected: Option<&'a str>,
}

/// Hold the connection row through the provider call and the encrypted write.
pub(crate) async fn get<F, Fut>(
    request: Request<'_>,
    envelope: &Envelope,
    fetch: F,
) -> Result<Available, Error>
where
    F: FnOnce(Option<Token>) -> Fut,
    Fut: Future<Output = anyhow::Result<Fetched>>,
{
    get_in_transaction(&request, envelope, fetch)
        .await
        .map_err(|source| Error {
            unauthorized: false,
            connection: request.connection,
            generation: request.generation,
            source,
        })?
}

async fn get_in_transaction<F, Fut>(
    request: &Request<'_>,
    envelope: &Envelope,
    fetch: F,
) -> anyhow::Result<Result<Available, Error>>
where
    F: FnOnce(Option<Token>) -> Fut,
    Fut: Future<Output = anyhow::Result<Fetched>>,
{
    let mut database = request.reader.oauth_database().await?;
    let tx = database.transaction().await?;
    lock(&tx, request).await?;
    let started = Instant::now();
    let stored = read_row(&tx, request.connection, envelope).await?;
    if let Some(stored) = &stored {
        if let Some(access) = &stored.token.access_token
            && stored.healthy
            && stored.seconds_left > 60.0
            && request.rejected != Some(access.as_str())
        {
            let available = Available {
                token: access.clone(),
                retain_until: retain_until(started, stored.seconds_left),
            };
            tx.commit().await?;
            return Ok(Ok(available));
        }
    } else if request.authorization_code {
        return Ok(Err(Error {
            unauthorized: true,
            connection: request.connection,
            generation: request.generation,
            source: anyhow::anyhow!("OAuth authorization required"),
        }));
    }
    let old = stored.map(|stored| stored.token);
    let available = fetch_and_write(tx, request.connection, envelope, old, fetch).await?;
    Ok(Ok(available))
}

async fn lock(tx: &tokio_postgres::Transaction<'_>, request: &Request<'_>) -> anyhow::Result<()> {
    let generation: Option<i64> = tx
        .query_one(
            wamn_registry::sql::PLATFORM_RUNTIME_SRC_OAUTH_TOKEN_LOCK,
            &[
                &request.reader.org()?,
                &request.project,
                &request.environment,
                &request.connection,
            ],
        )
        .await?
        .get(0);
    anyhow::ensure!(
        generation == Some(request.generation),
        "OAuth connection generation changed"
    );
    Ok(())
}

struct Stored {
    token: Token,
    seconds_left: f64,
    healthy: bool,
}

async fn read_row(
    tx: &tokio_postgres::Transaction<'_>,
    connection: uuid::Uuid,
    envelope: &Envelope,
) -> anyhow::Result<Option<Stored>> {
    let Some(row) = tx
        .query_opt(
            "SELECT ciphertext,nonce,wrapped_key,key_id,\
             extract(epoch FROM expires_at)::double precision-extract(epoch FROM clock_timestamp())::double precision AS seconds_left,\
             refresh_status='ok' AS healthy,\
             refresh_status='failing' AND updated_at > clock_timestamp()-interval '30 seconds' AS cooling \
             FROM identity.oauth_token WHERE connection_id=$1",
            &[&connection],
        )
        .await?
    else {
        return Ok(None);
    };
    anyhow::ensure!(!row.get::<_, bool>("cooling"), "OAuth refresh retry window");
    let plaintext = envelope
        .decrypt(&Encrypted {
            ciphertext: row.get("ciphertext"),
            nonce: row.get("nonce"),
            wrapped_key: row.get("wrapped_key"),
            key_id: row.get("key_id"),
        })
        .await?;
    let token = serde_json::from_slice(plaintext.expose()).context("decode OAuth token")?;
    Ok(Some(Stored {
        token,
        seconds_left: row.get("seconds_left"),
        healthy: row.get("healthy"),
    }))
}

async fn fetch_and_write<F, Fut>(
    tx: deadpool_postgres::Transaction<'_>,
    connection: uuid::Uuid,
    envelope: &Envelope,
    old: Option<Token>,
    fetch: F,
) -> anyhow::Result<Available>
where
    F: FnOnce(Option<Token>) -> Fut,
    Fut: Future<Output = anyhow::Result<Fetched>>,
{
    let previous_refresh = old.as_ref().and_then(|token| token.refresh_token.clone());
    // Prepare the first-failure marker before any provider call needs it.
    let marker = if old.is_none() {
        Some(
            envelope
                .encrypt(&serde_json::to_vec(&Token::default())?)
                .await?,
        )
    } else {
        None
    };
    let fetched_at = Instant::now();
    let Ok(mut fetched) = fetch(old).await else {
        record_failure(&tx, connection, marker.as_ref()).await?;
        tx.commit().await?;
        anyhow::bail!("OAuth provider unavailable");
    };
    if fetched.token.refresh_token.is_none() {
        fetched.token.refresh_token = previous_refresh;
    }
    let seconds = Duration::from_secs(fetched.expires_in.unwrap_or(0)).as_secs_f64();
    let token = fetched.token;
    let Ok(encrypted) = envelope.encrypt(&serde_json::to_vec(&token)?).await else {
        record_failure(&tx, connection, marker.as_ref()).await?;
        tx.commit().await?;
        anyhow::bail!("OAuth encryption unavailable");
    };
    // PostgreSQL owns expiry and retry times. A missing expiry is always due.
    let remaining = (seconds - fetched_at.elapsed().as_secs_f64()).max(0.0);
    write_row(&tx, connection, &encrypted, remaining).await?;
    tx.commit().await?;
    Ok(Available {
        token: token.access_token.context("OAuth access token missing")?,
        retain_until: retain_until(fetched_at, seconds),
    })
}

pub(crate) async fn write_row(
    tx: &tokio_postgres::Transaction<'_>,
    connection: uuid::Uuid,
    encrypted: &Encrypted,
    remaining: f64,
) -> anyhow::Result<()> {
    tx.execute(
        "INSERT INTO identity.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,refresh_status,updated_at) \
         VALUES($1,$2,$3,$4,$5,clock_timestamp()+$6::double precision*interval '1 second','ok',clock_timestamp()) \
         ON CONFLICT(connection_id) DO UPDATE SET ciphertext=EXCLUDED.ciphertext,nonce=EXCLUDED.nonce,\
         wrapped_key=EXCLUDED.wrapped_key,key_id=EXCLUDED.key_id,expires_at=EXCLUDED.expires_at,\
         refresh_status=EXCLUDED.refresh_status,updated_at=EXCLUDED.updated_at",
        &[
            &connection,
            &encrypted.ciphertext,
            &encrypted.nonce,
            &encrypted.wrapped_key,
            &encrypted.key_id,
            &remaining,
        ],
    )
    .await?;
    Ok(())
}

async fn record_failure(
    tx: &tokio_postgres::Transaction<'_>,
    connection: uuid::Uuid,
    marker: Option<&Encrypted>,
) -> anyhow::Result<()> {
    if let Some(marker) = marker {
        tx.execute(
            "INSERT INTO identity.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,refresh_status,updated_at) \
             VALUES($1,$2,$3,$4,$5,'-infinity','failing',clock_timestamp())",
            &[
                &connection,
                &marker.ciphertext,
                &marker.nonce,
                &marker.wrapped_key,
                &marker.key_id,
            ],
        )
        .await?;
    } else {
        tx.execute(
            "UPDATE identity.oauth_token SET refresh_status='failing',updated_at=clock_timestamp() WHERE connection_id=$1",
            &[&connection],
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_and_memory_window_use_sixty_seconds() {
        let now = Instant::now();
        for seconds in [f64::NEG_INFINITY, 0.0, 59.0, 60.0] {
            assert!(retain_until(now, seconds).is_none());
        }
        assert_eq!(
            retain_until(now, 3600.0),
            Some(now + Duration::from_mins(59))
        );
        assert!(retain_until(now - Duration::from_mins(59), 3600.0).is_none());
    }
}
