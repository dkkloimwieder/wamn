//! Issuer-targeted Ed25519 keys stay inside the system-database signing authority.
//!
//! Publication and activation are separate committed operations. Signers hold
//! the issuer row's shared lock until signing completes; lifecycle mutations
//! take its exclusive lock. A committed flip is therefore a signing-cutoff
//! barrier, not an estimate of the last signature. Retirement retains public
//! evidence for the approved 930 seconds without per-token writes.

use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair as _},
};
use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};

use wamn_identity_protocol::keys::{PublicSessionKey, SessionJwks, decode_public_key};

use crate::{IdentityError, IdentityErrorType};

/// Public overlap after the signing cutoff: 900-second lifetime plus 30 seconds.
pub const KEY_RETENTION_SECONDS: i64 = 930;

/// Generate and commit a new public generation without activating it.
///
/// Private PKCS#8 bytes are generated internally and never returned.
pub async fn publish_session_key(
    client: &mut Client,
    issuer: &str,
) -> Result<PublicSessionKey, IdentityError> {
    validate_issuer(issuer)?;
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| {
        IdentityError::new(IdentityErrorType::Entropy, "generate session signing key")
    })?;
    let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).map_err(|_| corrupt_key())?;
    let mut transaction = client.begin().await.map_err(key_database_error)?;
    bind_provisioning(&mut transaction).await?;
    sqlx::query(
        "INSERT INTO identity.session_signing_state (issuer) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(issuer)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .map_err(key_database_error)?;
    let row = sqlx::query(
        "INSERT INTO identity.session_keys (issuer, public_key, private_pkcs8) \
         VALUES ($1, $2, $3) RETURNING kid::text",
    )
    .bind(issuer)
    .bind(pair.public_key().as_ref())
    .bind(document.as_ref())
    .fetch_one(&mut *transaction)
    .await
    .map_err(key_database_error)?;
    let key = PublicSessionKey::new(row.get(0), pair.public_key().as_ref());
    transaction.commit().await.map_err(key_database_error)?;
    Ok(key)
}

/// Activate an already committed publication and retire the prior signing key.
///
/// Retired generations cannot be reactivated. Repeating the current activation
/// is harmless and does not restart any retirement deadline.
pub async fn activate_session_key(
    client: &mut Client,
    issuer: &str,
    kid: &str,
) -> Result<(), IdentityError> {
    activate_session_key_if_current(client, issuer, kid, None).await
}

async fn activate_session_key_if_current(
    client: &mut Client,
    issuer: &str,
    kid: &str,
    expected: Option<&str>,
) -> Result<(), IdentityError> {
    let mut transaction = client.begin().await.map_err(key_database_error)?;
    bind_provisioning(&mut transaction).await?;
    let active = lock_issuer(&mut transaction, issuer).await?;
    // Scheduled publication must not undo a removal or an explicit activation.
    if expected.is_some_and(|expected| active.as_deref() != Some(expected)) {
        transaction.commit().await.map_err(key_database_error)?;
        return Ok(());
    }
    let row = sqlx::query(
        "SELECT signing_cutoff IS NULL AND private_pkcs8 IS NOT NULL \
         FROM identity.session_keys WHERE issuer = $1 AND kid::text = $2",
    )
    .bind(issuer)
    .bind(kid)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(key_database_error)?;
    if !row.is_some_and(|row| row.get::<bool, _>(0)) {
        return Err(invalid(
            "session activation requires an unretired published key",
        ));
    }
    if active.as_deref() != Some(kid) {
        sqlx::query("UPDATE identity.session_keys SET signing_cutoff = clock_timestamp(), private_pkcs8 = NULL \
             WHERE issuer = $1 AND kid::text = $2").bind(issuer).bind(&active).execute(&mut *transaction).await.map(|result| result.rows_affected()).map_err(key_database_error)?;
        sqlx::query("UPDATE identity.session_signing_state SET active_kid = $2::text::uuid WHERE issuer = $1").bind(issuer).bind(kid).execute(&mut *transaction).await.map(|result| result.rows_affected()).map_err(key_database_error)?;
    }
    transaction.commit().await.map_err(key_database_error)
}

/// Rotate a due issuer with the same publication, activation and retirement commands.
/// A session lock serializes schedulers in different identity replicas.
/// An issuer without an active key still requires explicit activation.
pub async fn rotate_session_keys(
    client: &mut Client,
    issuer: &str,
    interval: std::time::Duration,
) -> Result<(), IdentityError> {
    validate_issuer(issuer)?;
    let seconds =
        i64::try_from(interval.as_secs()).map_err(|_| invalid("invalid signing key interval"))?;
    if seconds == 0 {
        return Err(invalid("signing key interval must be positive"));
    }
    let locked: bool = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT pg_try_advisory_lock(hashtextextended($1, 0))").to_string(),
    ))
    .bind(issuer)
    .fetch_one(&mut *client)
    .await
    .map_err(key_database_error)?
    .get(0);
    if !locked {
        return Ok(());
    }
    let result: Result<(), IdentityError> = async {
        let due = sqlx::query(sqlx::AssertSqlSafe(("SELECT s.active_kid::text FROM identity.session_signing_state s \
             JOIN identity.session_keys k ON k.issuer=s.issuer AND k.kid=s.active_kid \
             WHERE s.issuer=$1 AND k.published_at + ($2::bigint * interval '1 second') <= clock_timestamp()").to_string())).bind(issuer).bind(seconds).fetch_optional(&mut *client).await.map_err(key_database_error)?;
        if let Some(due) = due {
            let expected: String = due.get(0);
            let pending = sqlx::query(sqlx::AssertSqlSafe(("SELECT kid::text FROM identity.session_keys WHERE issuer=$1 AND signing_cutoff IS NULL \
                 AND kid IS DISTINCT FROM (SELECT active_kid FROM identity.session_signing_state WHERE issuer=$1) \
                 ORDER BY published_at DESC LIMIT 1").to_string())).bind(issuer).fetch_optional(&mut *client).await.map_err(key_database_error)?;
            let kid = if let Some(row) = pending { row.get(0) } else { publish_session_key(client, issuer).await?.kid };
            activate_session_key_if_current(client, issuer, &kid, Some(&expected)).await?;
        }
        retire_session_keys(client, issuer).await?;
        Ok(())
    }.await;
    let unlock = sqlx::query(sqlx::AssertSqlSafe(
        ("SELECT pg_advisory_unlock(hashtextextended($1, 0))").to_string(),
    ))
    .bind(issuer)
    .fetch_one(&mut *client)
    .await
    .map_err(key_database_error);
    result?;
    unlock?;
    Ok(())
}

/// Read only the public projection, excluding keys at their retirement deadline.
pub async fn session_jwks(
    client: &mut sqlx::PgConnection,
    issuer: &str,
) -> Result<SessionJwks, IdentityError> {
    validate_issuer(issuer)?;
    let rows = sqlx::query(
        "SELECT kid::text, public_key FROM identity.session_keys \
         WHERE issuer = $1 AND (signing_cutoff IS NULL OR \
             clock_timestamp() < signing_cutoff + ($2::bigint * interval '1 second')) ORDER BY kid",
    )
    .bind(issuer)
    .bind(KEY_RETENTION_SECONDS)
    .fetch_all(&mut *client)
    .await
    .map_err(key_database_error)?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows {
        let key = PublicSessionKey::new(row.get(0), row.get::<&[u8], _>(1));
        decode_public_key(&key).map_err(|_| corrupt_key())?;
        keys.push(key);
    }
    Ok(SessionJwks { keys })
}

/// Remove compromised public and private material without activating another key.
///
/// A concurrent signer finishes before this operation commits. An active-key
/// removal leaves the issuer unable to sign until an explicit activation.
pub async fn remove_compromised_session_key(
    client: &mut Client,
    issuer: &str,
    kid: &str,
) -> Result<bool, IdentityError> {
    let mut transaction = client.begin().await.map_err(key_database_error)?;
    bind_provisioning(&mut transaction).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(issuer)
        .execute(&mut *transaction)
        .await
        .map_err(key_database_error)?;
    lock_issuer(&mut transaction, issuer).await?;
    sqlx::query(
        "UPDATE identity.session_signing_state SET active_kid = NULL \
         WHERE issuer = $1 AND active_kid::text = $2",
    )
    .bind(issuer)
    .bind(kid)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .map_err(key_database_error)?;
    let removed =
        sqlx::query("DELETE FROM identity.session_keys WHERE issuer = $1 AND kid::text = $2")
            .bind(issuer)
            .bind(kid)
            .execute(&mut *transaction)
            .await
            .map(|result| result.rows_affected())
            .map_err(key_database_error)?;
    transaction.commit().await.map_err(key_database_error)?;
    Ok(removed != 0)
}

/// Delete expired retiring generations; their public projection already excludes them.
pub async fn retire_session_keys(client: &mut Client, issuer: &str) -> Result<u64, IdentityError> {
    let mut transaction = client.begin().await.map_err(key_database_error)?;
    bind_provisioning(&mut transaction).await?;
    lock_issuer(&mut transaction, issuer).await?;
    let removed = sqlx::query(
        "DELETE FROM identity.session_keys WHERE issuer = $1 AND \
         clock_timestamp() >= signing_cutoff + ($2::bigint * interval '1 second')",
    )
    .bind(issuer)
    .bind(KEY_RETENTION_SECONDS)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .map_err(key_database_error)?;
    transaction.commit().await.map_err(key_database_error)?;
    Ok(removed)
}

// Only this module reads private material. The synchronous message builder runs
// after the shared lock is acquired, so iat is not sampled before a lock wait.
pub(crate) async fn sign_message(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    issuer: &str,
    message: impl FnOnce(&str, &jsonwebtoken::EncodingKey) -> Result<String, IdentityError>,
) -> Result<String, IdentityError> {
    let row = sqlx::query(
        "SELECT active_kid::text FROM identity.session_signing_state WHERE issuer = $1 FOR SHARE",
    )
    .bind(issuer)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(key_database_error)?;
    let kid = row
        .and_then(|row| row.get::<Option<String>, _>(0))
        .ok_or_else(|| invalid("issuer has no active session signing key"))?;
    let token = {
        let row = sqlx::query(
            "SELECT private_pkcs8 FROM identity.session_keys \
             WHERE issuer = $1 AND kid::text = $2 AND signing_cutoff IS NULL",
        )
        .bind(issuer)
        .bind(&kid)
        .fetch_one(&mut **transaction)
        .await
        .map_err(key_database_error)?;
        let document: Option<&[u8]> = row.get(0);
        let key = jsonwebtoken::EncodingKey::from_ed_der(document.ok_or_else(corrupt_key)?);
        message(&kid, &key)?
    };
    Ok(token)
}

/// Bind `wamn:provisioning` as the stamp actor of `transaction`.
async fn bind_provisioning(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), IdentityError> {
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(
            wamn_identity_protocol::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .execute(&mut **transaction)
        .await
        .map(|result| result.rows_affected())
        .map_err(key_database_error)?;
    Ok(())
}

async fn lock_issuer(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    issuer: &str,
) -> Result<Option<String>, IdentityError> {
    validate_issuer(issuer)?;
    let row = sqlx::query(
        "SELECT active_kid::text FROM identity.session_signing_state WHERE issuer = $1 FOR UPDATE",
    )
    .bind(issuer)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(key_database_error)?;
    row.map(|row| row.get(0)).ok_or_else(|| {
        IdentityError::new(
            IdentityErrorType::NotFound,
            "session issuer is not published",
        )
    })
}

fn validate_issuer(issuer: &str) -> Result<(), IdentityError> {
    if issuer.trim().is_empty() {
        return Err(invalid("session issuer must be nonempty"));
    }
    Ok(())
}

pub(crate) fn corrupt_key() -> IdentityError {
    IdentityError::new(
        IdentityErrorType::CorruptData,
        "stored session signing key is invalid",
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err consumes the database error; only SQLSTATE leaves this boundary"
)]
pub(crate) fn key_database_error(error: sqlx::Error) -> IdentityError {
    // PostgreSQL DETAIL can contain rejected row values, including private
    // PKCS#8 bytes. Retain only the safe SQLSTATE, never the database message.
    IdentityError::new(
        IdentityErrorType::Database,
        format!(
            "session key database operation failed (SQLSTATE {})",
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref()
                .unwrap_or("unavailable"),
        ),
    )
}

fn invalid(message: &'static str) -> IdentityError {
    IdentityError::new(IdentityErrorType::InvalidInput, message)
}
