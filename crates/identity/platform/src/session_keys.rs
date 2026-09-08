//! Issuer-scoped Ed25519 keys stay inside the system-database signing authority.
//!
//! Publication and activation are separate committed operations. Signers hold
//! the issuer row's shared lock until signing completes; lifecycle mutations
//! take its exclusive lock. A committed flip is therefore a signing-cutoff
//! barrier, not an estimate of the last signature. Retirement retains public
//! evidence for the approved 930 seconds without per-token writes.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair as _},
};
use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, GenericClient};

use crate::{IdentityError, IdentityErrorKind};

/// Public overlap after the signing cutoff: 900-second lifetime plus 30 seconds.
pub const KEY_RETENTION_SECONDS: i64 = 930;

/// The fixed-profile public JWK; private and unknown parameters are refused.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublicSessionKey {
    /// Immutable, issuer-scoped generation identifier.
    pub kid: String,
    /// Must be `OKP`.
    pub kty: String,
    /// Must be `Ed25519`.
    pub crv: String,
    /// Must be the fully specified `Ed25519`, not `EdDSA`.
    pub alg: String,
    /// Must be `sig`.
    pub r#use: String,
    /// Canonical unpadded base64url of the 32-byte public key.
    pub x: String,
}

/// A complete issuer key set; callers replace, rather than merge, cached sets.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionJwks {
    /// Published and not-yet-retired verification keys.
    pub keys: Vec<PublicSessionKey>,
}

/// Validate the exact JWK profile and decode its public verification bytes.
pub fn decode_public_key(key: &PublicSessionKey) -> Result<[u8; 32], IdentityError> {
    if key.kid.is_empty()
        || key.kty != "OKP"
        || key.crv != "Ed25519"
        || key.alg != "Ed25519"
        || key.r#use != "sig"
    {
        return Err(invalid(
            "session public key does not match the Ed25519 profile",
        ));
    }
    URL_SAFE_NO_PAD
        .decode(&key.x)
        .map_err(|_| invalid("session public key is not canonical base64url"))?
        .try_into()
        .map_err(|_| invalid("session public key must contain 32 bytes"))
}

/// Generate and commit a new public generation without activating it.
///
/// Private PKCS#8 bytes are generated internally and never returned.
pub async fn publish_session_key(
    client: &mut Client,
    issuer: &str,
) -> Result<PublicSessionKey, IdentityError> {
    validate_issuer(issuer)?;
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| {
        IdentityError::new(IdentityErrorKind::Entropy, "generate session signing key")
    })?;
    let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).map_err(|_| corrupt_key())?;
    let transaction = client.transaction().await.map_err(key_database_error)?;
    transaction.execute(
        "INSERT INTO identity.session_signing_state (issuer) VALUES ($1) ON CONFLICT DO NOTHING",
        &[&issuer],
    ).await.map_err(key_database_error)?;
    let row = transaction
        .query_one(
            "INSERT INTO identity.session_keys (issuer, public_key, private_pkcs8) \
         VALUES ($1, $2, $3) RETURNING kid::text",
            &[&issuer, &pair.public_key().as_ref(), &document.as_ref()],
        )
        .await
        .map_err(key_database_error)?;
    let key = public_key(row.get(0), pair.public_key().as_ref());
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
    let transaction = client.transaction().await.map_err(key_database_error)?;
    let active = lock_issuer(&transaction, issuer).await?;
    let row = transaction
        .query_opt(
            "SELECT signing_cutoff IS NULL AND private_pkcs8 IS NOT NULL \
         FROM identity.session_keys WHERE issuer = $1 AND kid::text = $2",
            &[&issuer, &kid],
        )
        .await
        .map_err(key_database_error)?;
    if !row.is_some_and(|row| row.get::<_, bool>(0)) {
        return Err(invalid(
            "session activation requires an unretired published key",
        ));
    }
    if active.as_deref() != Some(kid) {
        transaction.execute(
            "UPDATE identity.session_keys SET signing_cutoff = clock_timestamp(), private_pkcs8 = NULL \
             WHERE issuer = $1 AND kid::text = $2",
            &[&issuer, &active],
        ).await.map_err(key_database_error)?;
        transaction.execute(
            "UPDATE identity.session_signing_state SET active_kid = $2::text::uuid WHERE issuer = $1",
            &[&issuer, &kid],
        ).await.map_err(key_database_error)?;
    }
    transaction.commit().await.map_err(key_database_error)
}

/// Read only the public projection, excluding keys at their retirement deadline.
pub async fn session_jwks(
    client: &(impl GenericClient + Sync),
    issuer: &str,
) -> Result<SessionJwks, IdentityError> {
    validate_issuer(issuer)?;
    let rows = client
        .query(
            "SELECT kid::text, public_key FROM identity.session_keys \
         WHERE issuer = $1 AND (signing_cutoff IS NULL OR \
             clock_timestamp() < signing_cutoff + ($2::bigint * interval '1 second')) ORDER BY kid",
            &[&issuer, &KEY_RETENTION_SECONDS],
        )
        .await
        .map_err(key_database_error)?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows {
        let key = public_key(row.get(0), row.get::<_, &[u8]>(1));
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
    let transaction = client.transaction().await.map_err(key_database_error)?;
    lock_issuer(&transaction, issuer).await?;
    transaction
        .execute(
            "UPDATE identity.session_signing_state SET active_kid = NULL \
         WHERE issuer = $1 AND active_kid::text = $2",
            &[&issuer, &kid],
        )
        .await
        .map_err(key_database_error)?;
    let removed = transaction
        .execute(
            "DELETE FROM identity.session_keys WHERE issuer = $1 AND kid::text = $2",
            &[&issuer, &kid],
        )
        .await
        .map_err(key_database_error)?;
    transaction.commit().await.map_err(key_database_error)?;
    Ok(removed != 0)
}

/// Delete expired retiring generations; their public projection already excludes them.
pub async fn retire_session_keys(client: &mut Client, issuer: &str) -> Result<u64, IdentityError> {
    let transaction = client.transaction().await.map_err(key_database_error)?;
    lock_issuer(&transaction, issuer).await?;
    let removed = transaction
        .execute(
            "DELETE FROM identity.session_keys WHERE issuer = $1 AND \
         clock_timestamp() >= signing_cutoff + ($2::bigint * interval '1 second')",
            &[&issuer, &KEY_RETENTION_SECONDS],
        )
        .await
        .map_err(key_database_error)?;
    transaction.commit().await.map_err(key_database_error)?;
    Ok(removed)
}

// Only this module reads private material. The synchronous message builder runs
// after the shared lock is acquired, so iat is not sampled before a lock wait.
pub(crate) async fn sign_message(
    client: &mut Client,
    issuer: &str,
    message: impl FnOnce(&str) -> Result<String, IdentityError>,
) -> Result<String, IdentityError> {
    let transaction = client.transaction().await.map_err(key_database_error)?;
    let row = transaction.query_opt(
        "SELECT active_kid::text FROM identity.session_signing_state WHERE issuer = $1 FOR SHARE",
        &[&issuer],
    ).await.map_err(key_database_error)?;
    let kid = row
        .and_then(|row| row.get::<_, Option<String>>(0))
        .ok_or_else(|| invalid("issuer has no active session signing key"))?;
    let token = {
        let row = transaction
            .query_one(
                "SELECT private_pkcs8 FROM identity.session_keys \
             WHERE issuer = $1 AND kid::text = $2 AND signing_cutoff IS NULL",
                &[&issuer, &kid],
            )
            .await
            .map_err(key_database_error)?;
        let document: Option<&[u8]> = row.get(0);
        let pair = Ed25519KeyPair::from_pkcs8(document.ok_or_else(corrupt_key)?)
            .map_err(|_| corrupt_key())?;
        let message = message(&kid)?;
        let signature = pair.sign(message.as_bytes());
        format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
    };
    transaction.commit().await.map_err(key_database_error)?;
    Ok(token)
}

async fn lock_issuer(
    transaction: &tokio_postgres::Transaction<'_>,
    issuer: &str,
) -> Result<Option<String>, IdentityError> {
    validate_issuer(issuer)?;
    let row = transaction.query_opt(
        "SELECT active_kid::text FROM identity.session_signing_state WHERE issuer = $1 FOR UPDATE",
        &[&issuer],
    ).await.map_err(key_database_error)?;
    row.map(|row| row.get(0)).ok_or_else(|| {
        IdentityError::new(
            IdentityErrorKind::NotFound,
            "session issuer is not published",
        )
    })
}

fn public_key(kid: String, bytes: &[u8]) -> PublicSessionKey {
    PublicSessionKey {
        kid,
        kty: "OKP".into(),
        crv: "Ed25519".into(),
        alg: "Ed25519".into(),
        r#use: "sig".into(),
        x: URL_SAFE_NO_PAD.encode(bytes),
    }
}

fn validate_issuer(issuer: &str) -> Result<(), IdentityError> {
    if issuer.trim().is_empty() {
        return Err(invalid("session issuer must be nonempty"));
    }
    Ok(())
}

fn corrupt_key() -> IdentityError {
    IdentityError::new(
        IdentityErrorKind::CorruptData,
        "stored session signing key is invalid",
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err consumes the database error; only SQLSTATE leaves this boundary"
)]
fn key_database_error(error: tokio_postgres::Error) -> IdentityError {
    // PostgreSQL DETAIL can contain rejected row values, including private
    // PKCS#8 bytes. Retain only the safe SQLSTATE, never the database message.
    IdentityError::new(
        IdentityErrorKind::Database,
        format!(
            "session key database operation failed (SQLSTATE {})",
            error
                .code()
                .map_or("unavailable", tokio_postgres::error::SqlState::code),
        ),
    )
}

fn invalid(message: &'static str) -> IdentityError {
    IdentityError::new(IdentityErrorKind::InvalidInput, message)
}
