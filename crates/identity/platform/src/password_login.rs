//! Revocable password logins and single-use renewal credentials.
//!
//! Callers bind their actor in the transaction and check current environment
//! authority before issuance or rotation. Lock the principal before verifying
//! a password, then create the login in that same transaction. Commit a refused
//! rotation: consumed-credential replay revokes the family as part of refusal.
//! A successful rotation and access-token signing share one transaction.

use std::fmt;

use ring::rand::{SecureRandom as _, SystemRandom};
use sha2::{Digest as _, Sha256};
use tokio_postgres::{Row, Transaction};
use zeroize::Zeroizing;

use crate::{IdentityError, IdentityErrorKind, PrincipalId};

const PREFIX: &str = "wamn_renew_";
/// Maximum login duration from password authentication, in seconds.
pub const ABSOLUTE_LIFETIME: i64 = 8 * 60 * 60;
/// Maximum time between successful renewals, in seconds.
pub const INACTIVITY_LIFETIME: i64 = 30 * 60;

/// A stored login's immutable identity and absolute deadline.
#[derive(Debug, Clone)]
pub struct Login {
    pub id: String,
    pub principal: PrincipalId,
    pub authenticated_at: i64,
    pub expires_at: i64,
}

/// One replacement credential, returned only after its transaction commits.
pub struct Renewal {
    pub login: Login,
    secret: Zeroizing<String>,
}
impl Renewal {
    /// The opaque credential for private delivery to the authenticated caller.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}
impl fmt::Debug for Renewal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Renewal")
            .field("login", &self.login)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

fn database(error: &tokio_postgres::Error) -> IdentityError {
    IdentityError::new(
        IdentityErrorKind::Database,
        format!(
            "password login storage failed ({})",
            error.code().map_or("connection", |c| c.code())
        ),
    )
}
fn decode(row: &Row) -> Result<Login, IdentityError> {
    Ok(Login {
        id: row.get(0),
        principal: row.get::<_, String>(1).parse()?,
        authenticated_at: row.get(2),
        expires_at: row.get(3),
    })
}
fn hash(secret: &str) -> Option<Vec<u8>> {
    let value = secret.strip_prefix(PREFIX)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    Some(Sha256::digest(secret.as_bytes()).to_vec())
}
async fn credential(tx: &Transaction<'_>, login: Login) -> Result<Renewal, IdentityError> {
    let mut bytes = Zeroizing::new([0; 32]);
    SystemRandom::new().fill(bytes.as_mut()).map_err(|_| {
        IdentityError::new(
            IdentityErrorKind::Entropy,
            "renewal credential generation failed",
        )
    })?;
    let secret = Zeroizing::new(format!("{PREFIX}{}", hex::encode(*bytes)));
    let digest = Sha256::digest(secret.as_bytes()).to_vec();
    tx.execute(
        "INSERT INTO identity.renewal_credentials (token_hash,login_id) VALUES ($1,$2::text::uuid)",
        &[&digest, &login.id],
    )
    .await
    .map_err(|error| database(&error))?;
    Ok(Renewal { login, secret })
}

/// Lock an active human through password verification and login creation.
///
/// Reset, revocation and renewal take this same lock before writing credentials.
/// A false result still locks an existing disabled principal until transaction end.
pub async fn lock_principal(
    tx: &Transaction<'_>,
    principal: &PrincipalId,
) -> Result<bool, IdentityError> {
    Ok(tx
        .query_one(
            "SELECT identity.lock_password_principal($1::text::uuid)",
            &[&principal.as_str()],
        )
        .await
        .map_err(|error| database(&error))?
        .get(0))
}

/// Create one login after password and environment authorization succeed.
///
/// The caller must lock the principal before password verification, not just
/// here, so reset cannot commit between verification and creation.
pub async fn create_login(
    tx: &Transaction<'_>,
    principal: &PrincipalId,
    issuer: &str,
    audience: &str,
) -> Result<Option<Renewal>, IdentityError> {
    if !lock_principal(tx, principal).await? {
        return Ok(None);
    }
    let row = tx.query_one(
        "WITH instant AS (SELECT date_trunc('second',clock_timestamp()) AS at) \
         INSERT INTO identity.password_logins (principal_id,issuer,audience,authenticated_at,expires_at,renewal_expires_at) \
         SELECT $1::text::uuid,$2,$3,at,at+($4::bigint*interval '1 second'),at+($5::bigint*interval '1 second') FROM instant \
         RETURNING id::text,principal_id::text,extract(epoch FROM authenticated_at)::bigint,extract(epoch FROM expires_at)::bigint",
        &[&principal.as_str(),&issuer,&audience,&ABSOLUTE_LIFETIME,&INACTIVITY_LIFETIME],
    ).await.map_err(|error| database(&error))?;
    Ok(Some(credential(tx, decode(&row)?).await?))
}

// Lookup before locking reveals only the principal to lock. Every credential
// fact is re-read in a separate statement after the principal lock is held.
async fn lookup(
    tx: &Transaction<'_>,
    issuer: &str,
    audience: &str,
    digest: &[u8],
) -> Result<Option<(Login, bool, bool)>, IdentityError> {
    let Some(row) = tx.query_opt(
        "SELECT l.principal_id::text FROM identity.renewal_credentials c JOIN identity.password_logins l ON l.id=c.login_id \
         WHERE c.token_hash=$1 AND l.issuer=$2 AND l.audience=$3", &[&digest,&issuer,&audience],
    ).await.map_err(|error| database(&error))? else { return Ok(None) };
    let principal: PrincipalId = row.get::<_, String>(0).parse()?;
    if !lock_principal(tx, &principal).await? {
        return Ok(None);
    }
    let row = tx.query_opt(
        "SELECT l.id::text,l.principal_id::text,extract(epoch FROM l.authenticated_at)::bigint,extract(epoch FROM l.expires_at)::bigint, \
         c.consumed_at IS NOT NULL, l.revoked_at IS NULL AND l.expires_at>clock_timestamp() AND l.renewal_expires_at>clock_timestamp() \
         FROM identity.renewal_credentials c JOIN identity.password_logins l ON l.id=c.login_id \
         WHERE c.token_hash=$1 AND l.issuer=$2 AND l.audience=$3 FOR UPDATE OF l", &[&digest,&issuer,&audience],
    ).await.map_err(|error| database(&error))?;
    row.map(|row| Ok((decode(&row)?, row.get(4), row.get(5))))
        .transpose()
}

/// Consume a credential once and create its replacement in the same transaction.
///
/// `None` refuses authentication. Commit that outcome to retain replay revocation.
/// On success, authorize current environment access and sign before committing.
pub async fn rotate_login(
    tx: &Transaction<'_>,
    issuer: &str,
    audience: &str,
    secret: &str,
) -> Result<Option<Renewal>, IdentityError> {
    let Some(digest) = hash(secret) else {
        return Ok(None);
    };
    let Some((login, consumed, usable)) = lookup(tx, issuer, audience, &digest).await? else {
        return Ok(None);
    };
    if consumed {
        revoke_family(tx, &login.id).await?;
        return Ok(None);
    }
    if !usable {
        return Ok(None);
    }
    tx.execute(
        "UPDATE identity.renewal_credentials SET consumed_at=clock_timestamp() WHERE token_hash=$1",
        &[&digest],
    )
    .await
    .map_err(|error| database(&error))?;
    tx.execute("UPDATE identity.password_logins SET renewal_expires_at=LEAST(expires_at,clock_timestamp()+($2::bigint*interval '1 second')) WHERE id=$1::text::uuid", &[&login.id,&INACTIVITY_LIFETIME]).await.map_err(|error| database(&error))?;
    Ok(Some(credential(tx, login).await?))
}
async fn revoke_family(tx: &Transaction<'_>, login: &str) -> Result<(), IdentityError> {
    tx.execute("UPDATE identity.password_logins SET revoked_at=COALESCE(revoked_at,clock_timestamp()) WHERE id=$1::text::uuid", &[&login]).await.map_err(|error| database(&error))?;
    Ok(())
}

/// Revoke the credential's family, including an already consumed credential.
pub async fn revoke_login(
    tx: &Transaction<'_>,
    issuer: &str,
    audience: &str,
    secret: &str,
) -> Result<(), IdentityError> {
    if let Some(digest) = hash(secret)
        && let Some((login, _, _)) = lookup(tx, issuer, audience, &digest).await?
    {
        revoke_family(tx, &login.id).await?;
    }
    Ok(())
}

/// Revoke every family for a principal, across issuers and environments.
///
/// The caller must authorize logout-all or password reset before calling.
pub async fn revoke_all(
    tx: &Transaction<'_>,
    principal: &PrincipalId,
) -> Result<(), IdentityError> {
    lock_principal(tx, principal).await?;
    tx.execute("UPDATE identity.password_logins SET revoked_at=clock_timestamp() WHERE principal_id=$1::text::uuid AND revoked_at IS NULL", &[&principal.as_str()]).await.map_err(|error| database(&error))?;
    Ok(())
}

/// Remove at most 100 absolutely expired families and their consumed evidence.
pub async fn prune_expired(tx: &Transaction<'_>, issuer: &str) -> Result<u64, IdentityError> {
    tx.execute("DELETE FROM identity.password_logins WHERE id IN (SELECT id FROM identity.password_logins WHERE issuer=$1 AND expires_at<=clock_timestamp() ORDER BY expires_at,id LIMIT 100 FOR UPDATE SKIP LOCKED)", &[&issuer]).await.map_err(|error| database(&error))
}
