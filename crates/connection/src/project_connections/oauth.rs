//! OAuth token persistence under the active generation lock.

use std::time::Duration;

use crate::oauth_token::{Available, Error, Fetch, Refresh, Token};
use anyhow::Context as _;
use sqlx::{Connection as _, Row as _};
use tokio::time::Instant;
use wamn_envelope::encryption::{Encrypted, Envelope};

pub(super) async fn get_token(
    database: &super::ProjectDatabase<'_>,
    request: &Refresh<'_>,
    envelope: &Envelope,
    fetch: Fetch<'_>,
) -> anyhow::Result<Result<Available, Error>> {
    let mut database = database.oauth_connection().await?;
    let mut tx = database.begin().await?;
    wamn_postgres::bind_transaction_identity(&mut tx, &request.principal, "", "", None, None)
        .await?;
    lock(&mut tx, request).await?;
    let stored = read_row(&mut tx, request.connection, envelope).await?;
    if let Some(stored) = &stored {
        if let Some(access) = &stored.token.access_token
            && stored.healthy
            && stored.seconds_left > 60.0
            && request.rejected != Some(access.as_str())
        {
            let available = Available {
                token: access.clone(),
            };
            tx.commit().await?;
            return Ok(Ok(available));
        }
    } else if request.authorization_code {
        return Ok(Err(Error::authorization_required(request)));
    }
    let old = stored.map(|stored| stored.token);
    let available = fetch_and_write(tx, request.connection, envelope, old, fetch).await?;
    Ok(Ok(available))
}

async fn lock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &Refresh<'_>,
) -> anyhow::Result<()> {
    let generation: Option<i64> = sqlx::query_scalar("SELECT active_generation FROM platform.connection WHERE id=$1 AND active FOR NO KEY UPDATE")
        .bind(request.connection).fetch_optional(&mut **tx).await?;
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
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    connection: uuid::Uuid,
    envelope: &Envelope,
) -> anyhow::Result<Option<Stored>> {
    let Some(row) = sqlx::query(
            "SELECT ciphertext,nonce,wrapped_key,key_id,\
             extract(epoch FROM expires_at)::double precision-extract(epoch FROM clock_timestamp())::double precision AS seconds_left,\
             refresh_status='ok' AS healthy,\
             refresh_status='failing' AND updated_at > clock_timestamp()-interval '30 seconds' AS cooling \
             FROM platform.oauth_token WHERE connection_id=$1",
        ).bind(connection).fetch_optional(&mut **tx)
        .await?
    else {
        return Ok(None);
    };
    anyhow::ensure!(!row.get::<bool, _>("cooling"), "OAuth refresh retry window");
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

async fn fetch_and_write(
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    connection: uuid::Uuid,
    envelope: &Envelope,
    old: Option<Token>,
    fetch: Fetch<'_>,
) -> anyhow::Result<Available> {
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
        record_failure(&mut tx, connection, marker.as_ref()).await?;
        tx.commit().await?;
        anyhow::bail!("OAuth provider unavailable");
    };
    if fetched.token.refresh_token.is_none() {
        fetched.token.refresh_token = previous_refresh;
    }
    let seconds = Duration::from_secs(fetched.expires_in.unwrap_or(0)).as_secs_f64();
    let token = fetched.token;
    let Ok(encrypted) = envelope.encrypt(&serde_json::to_vec(&token)?).await else {
        record_failure(&mut tx, connection, marker.as_ref()).await?;
        tx.commit().await?;
        anyhow::bail!("OAuth encryption unavailable");
    };
    // PostgreSQL owns expiry and retry times. A missing expiry is always due.
    let remaining = (seconds - fetched_at.elapsed().as_secs_f64()).max(0.0);
    write_token(&mut tx, connection, &encrypted, remaining).await?;
    tx.commit().await?;
    Ok(Available {
        token: token.access_token.context("OAuth access token missing")?,
    })
}

pub async fn write_token(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    connection: uuid::Uuid,
    encrypted: &Encrypted,
    remaining: f64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO platform.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,refresh_status,updated_at) \
         VALUES($1,$2,$3,$4,$5,clock_timestamp()+$6::double precision*interval '1 second','ok',clock_timestamp()) \
         ON CONFLICT(connection_id) DO UPDATE SET ciphertext=EXCLUDED.ciphertext,nonce=EXCLUDED.nonce,\
         wrapped_key=EXCLUDED.wrapped_key,key_id=EXCLUDED.key_id,expires_at=EXCLUDED.expires_at,\
         refresh_status=EXCLUDED.refresh_status,updated_at=EXCLUDED.updated_at").bind(connection).bind(&encrypted.ciphertext).bind(&encrypted.nonce)
        .bind(&encrypted.wrapped_key).bind(&encrypted.key_id).bind(remaining)
        .execute(&mut **tx).await?;
    Ok(())
}

async fn record_failure(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    connection: uuid::Uuid,
    marker: Option<&Encrypted>,
) -> anyhow::Result<()> {
    if let Some(marker) = marker {
        sqlx::query(
            "INSERT INTO platform.oauth_token(connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,refresh_status,updated_at) \
             VALUES($1,$2,$3,$4,$5,'-infinity','failing',clock_timestamp())").bind(connection).bind(&marker.ciphertext).bind(&marker.nonce)
            .bind(&marker.wrapped_key).bind(&marker.key_id).execute(&mut **tx).await?;
    } else {
        sqlx::query(
            "UPDATE platform.oauth_token SET refresh_status='failing',updated_at=clock_timestamp() WHERE connection_id=$1",
        ).bind(connection).fetch_optional(&mut **tx)
        .await?;
    }
    Ok(())
}
