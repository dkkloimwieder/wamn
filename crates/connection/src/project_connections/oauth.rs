//! Shared OAuth tokens, with provider I/O between short SQL transactions.

use crate::oauth_token::{Available, Error, Fetch, Refresh, Token};
use anyhow::Context as _;
use sqlx::{Connection as _, PgConnection, Postgres, Row as _, Transaction};
use tokio::time::Instant;
use uuid::Uuid;
use wamn_envelope::encryption::{Encrypted, Envelope};

use super::{ConnectionEvent, EventOutcome, EventType};

/// Serialize callbacks and refreshes for this connection on a pinned session.
pub async fn lock_oauth_session(database: &mut PgConnection, connection: Uuid) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('wamn-oauth/' || $1::uuid::text, 0))")
        .bind(connection)
        .execute(database)
        .await?;
    Ok(())
}

/// Release the session before the database connection is returned to a pool.
pub async fn unlock_oauth_session(
    database: &mut PgConnection,
    connection: Uuid,
) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended('wamn-oauth/' || $1::uuid::text, 0))")
        .bind(connection)
        .execute(database)
        .await?;
    Ok(())
}

pub(super) async fn get_token(
    database: &super::ProjectDatabase<'_>,
    request: &Refresh<'_>,
    envelope: &Envelope,
    fetch: Fetch<'_>,
) -> anyhow::Result<Result<Available, Error>> {
    let mut session = database.oauth_connection().await?;
    let database = session.connection();
    lock_oauth_session(database, request.connection).await?;
    let result = refresh_if_due(database, request, envelope, fetch).await;
    unlock_oauth_session(database, request.connection).await?;
    session.repool();
    result
}

async fn refresh_if_due(
    database: &mut PgConnection,
    request: &Refresh<'_>,
    envelope: &Envelope,
    fetch: Fetch<'_>,
) -> anyhow::Result<Result<Available, Error>> {
    let mut tx = database.begin().await?;
    anyhow::ensure!(
        is_current(&mut tx, request).await?,
        "OAuth connection generation changed"
    );
    let stored = read_token(&mut tx, request).await?;
    tx.commit().await?;

    if stored.encrypted.is_none() && request.authorization_code {
        return Ok(Err(Error::authorization_required(request)));
    }
    anyhow::ensure!(!stored.cooling, "OAuth refresh retry window");

    let event = ConnectionEvent {
        connection: request.connection,
        generation: request.generation,
        event_type: EventType::TokenRefresh,
        principal: Some(request.principal),
    };
    let old = match stored.encrypted {
        Some(encrypted) => match decrypt(envelope, &encrypted).await {
            Ok(mut token) => {
                if stored.healthy
                    && stored.seconds_left > 60.0
                    && let Some(access) = token
                        .access_token
                        .take_if(|access| request.rejected != Some(access.as_str()))
                {
                    let mut tx = database.begin().await?;
                    anyhow::ensure!(
                        is_current(&mut tx, request).await?,
                        "OAuth connection generation changed"
                    );
                    tx.commit().await?;
                    return Ok(Ok(Available { token: access }));
                }
                Some(token)
            }
            Err(error) => {
                event.record_refusal(database, EventOutcome::Failed).await;
                return Err(error);
            }
        },
        None => None,
    };

    let result = fetch_and_encrypt(envelope, old, fetch).await;
    let (encrypted, access, seconds, fetched_at) = match result {
        Ok(result) => result,
        Err(error) => {
            event.record_refusal(database, EventOutcome::Failed).await;
            return Err(error);
        }
    };
    let mut tx = database.begin().await?;
    let remaining = (seconds - fetched_at.elapsed().as_secs_f64()).max(0.0);
    let publish = async {
        wamn_postgres::bind_transaction_identity(&mut tx, &request.principal, "", "", None, None)
            .await?;
        let current = is_current(&mut tx, request).await?;
        let outcome = if current {
            write_token(&mut tx, request.connection, &encrypted, remaining).await?;
            EventOutcome::Succeeded
        } else {
            EventOutcome::Superseded
        };
        event.append(&mut tx, outcome).await?;
        Ok::<_, sqlx::Error>(current)
    }
    .await;
    let current = match publish {
        Ok(current) => current,
        Err(error) => {
            tx.rollback().await?;
            event.record_refusal(database, EventOutcome::Failed).await;
            return Err(error.into());
        }
    };
    tx.commit().await?;
    anyhow::ensure!(current, "OAuth connection generation changed");
    Ok(Ok(Available { token: access }))
}

async fn is_current(
    tx: &mut Transaction<'_, Postgres>,
    request: &Refresh<'_>,
) -> sqlx::Result<bool> {
    Ok(lock_active_generation(tx, request.connection).await? == Some(request.generation))
}

/// Read the active generation and keep it fixed until the SQL transaction ends.
pub async fn lock_active_generation(
    tx: &mut Transaction<'_, Postgres>,
    connection: Uuid,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT active_generation FROM platform.connection
         WHERE id=$1 AND active FOR NO KEY UPDATE",
    )
    .bind(connection)
    .fetch_optional(&mut **tx)
    .await
}

struct Stored {
    encrypted: Option<Encrypted>,
    seconds_left: f64,
    healthy: bool,
    cooling: bool,
}

async fn read_token(
    tx: &mut Transaction<'_, Postgres>,
    request: &Refresh<'_>,
) -> sqlx::Result<Stored> {
    sqlx::query(
        "SELECT t.ciphertext,t.nonce,t.wrapped_key,t.key_id,
                coalesce(extract(epoch FROM t.expires_at)::double precision
                         - extract(epoch FROM clock_timestamp())::double precision, 0) AS seconds_left,
                NOT coalesce(r.failed, false) AS healthy,
                coalesce(r.failed AND r.occurred_at > clock_timestamp() - interval '30 seconds', false) AS cooling
         FROM platform.connection c
         LEFT JOIN platform.oauth_token t ON t.connection_id=c.id
         LEFT JOIN LATERAL (
             SELECT NOT o.success AS failed, e.occurred_at
             FROM platform.connection_event e
             JOIN platform.connection_event_outcome o ON o.code=e.outcome
             WHERE e.connection_id=c.id AND e.generation=c.active_generation
               AND e.event_type='token_refresh'
             ORDER BY e.occurred_at DESC, e.id DESC LIMIT 1
         ) r ON t.updated_at IS NULL OR r.occurred_at >= t.updated_at
         WHERE c.id=$1",
    )
    .bind(request.connection)
    .try_map(|row: sqlx::postgres::PgRow| {
        let encrypted = match row.try_get::<Option<Vec<u8>>, _>("ciphertext")? {
            Some(ciphertext) => Some(Encrypted {
                ciphertext,
                nonce: row.try_get("nonce")?,
                wrapped_key: row.try_get("wrapped_key")?,
                key_id: row.try_get("key_id")?,
            }),
            None => None,
        };
        Ok(Stored {
            encrypted,
            seconds_left: row.try_get("seconds_left")?,
            healthy: row.try_get("healthy")?,
            cooling: row.try_get("cooling")?,
        })
    })
    .fetch_one(&mut **tx)
    .await
}

async fn decrypt(envelope: &Envelope, encrypted: &Encrypted) -> anyhow::Result<Token> {
    let plaintext = envelope.decrypt(encrypted).await?;
    serde_json::from_slice(plaintext.expose()).context("decode OAuth token")
}

async fn fetch_and_encrypt(
    envelope: &Envelope,
    old: Option<Token>,
    fetch: Fetch<'_>,
) -> anyhow::Result<(Encrypted, String, f64, Instant)> {
    let previous_refresh = old.as_ref().and_then(|token| token.refresh_token.clone());
    let fetched_at = Instant::now();
    let mut fetched = fetch(old)
        .await
        .map_err(|_| anyhow::anyhow!("OAuth provider unavailable"))?;
    let seconds = std::time::Duration::from_secs(fetched.expires_in.unwrap_or(0)).as_secs_f64();
    if fetched.token.refresh_token.is_none() {
        fetched.token.refresh_token = previous_refresh;
    }
    let encrypted = envelope
        .encrypt(&serde_json::to_vec(&fetched.token)?)
        .await
        .context("OAuth encryption unavailable")?;
    let access = fetched
        .token
        .access_token
        .context("OAuth access token missing")?;
    Ok((encrypted, access, seconds, fetched_at))
}

pub async fn write_token(
    tx: &mut Transaction<'_, Postgres>,
    connection: Uuid,
    encrypted: &Encrypted,
    remaining: f64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO platform.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,updated_at)
         VALUES($1,$2,$3,$4,$5,clock_timestamp()+$6::double precision*interval '1 second',clock_timestamp())
         ON CONFLICT(connection_id) DO UPDATE SET ciphertext=EXCLUDED.ciphertext,nonce=EXCLUDED.nonce,
           wrapped_key=EXCLUDED.wrapped_key,key_id=EXCLUDED.key_id,expires_at=EXCLUDED.expires_at,
           updated_at=EXCLUDED.updated_at",
    )
    .bind(connection)
    .bind(&encrypted.ciphertext)
    .bind(&encrypted.nonce)
    .bind(&encrypted.wrapped_key)
    .bind(&encrypted.key_id)
    .bind(remaining)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
