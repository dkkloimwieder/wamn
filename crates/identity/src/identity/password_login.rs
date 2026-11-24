//! Revocable password logins and single-use renewal credentials.
//!
//! Callers bind their actor in the transaction and test current environment
//! authority before issuance or rotation. Lock the principal before verifying
//! a password, then create the login in that same transaction. Commit a refused
//! rotation: consumed-credential replay revokes the family as part of refusal.
//! A successful rotation and access-token signing share one transaction.

use crate::password_login::{ABSOLUTE_LIFETIME, INACTIVITY_LIFETIME, Login, Renewal};
use crate::principal::AuthenticatedPrincipal;
use sqlx::Row as _;
use sqlx::postgres::PgRow as Row;
use sqlx::types::Uuid;
use wamn_authentication::identity_error::{IdentityError, IdentityErrorType};
use wamn_identity_protocol::{
    PrincipalId,
    secret::{SecretToken, SecretType},
};

use super::resolve_principal;

fn database(error: &sqlx::Error) -> IdentityError {
    IdentityError::new(
        IdentityErrorType::Database,
        format!(
            "password login storage failed ({})",
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .unwrap_or_else(|| "connection".into())
        ),
    )
}
fn decode(row: &Row) -> Result<Login, IdentityError> {
    Ok(Login {
        id: row.get(0),
        principal: PrincipalId::from(
            row.try_get::<Uuid, _>(1)
                .map_err(|error| database(&error))?,
        ),
        authenticated_at: row.get(2),
        expires_at: row.get(3),
    })
}
async fn credential(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    login: Login,
) -> Result<Renewal, IdentityError> {
    let token = SecretToken::mint(SecretType::Renewal)?;
    let digest = token.digest();
    let secret = token.into_secret();
    sqlx::query(
        "INSERT INTO identity.renewal_credentials (token_hash,login_id) VALUES ($1,$2::text::uuid)",
    )
    .bind(digest.to_string())
    .bind(&login.id)
    .execute(&mut **tx)
    .await
    .map(|result| result.rows_affected())
    .map_err(|error| database(&error))?;
    Ok(Renewal::new(login, secret))
}

/// Lock an active user through password verification and login creation.
///
/// Reset, revocation and renewal take this same lock before writing credentials.
/// A false result still locks an existing disabled principal until transaction end.
pub async fn lock_principal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &PrincipalId,
) -> Result<bool, IdentityError> {
    Ok(sqlx::query("SELECT identity.lock_password_principal($1)")
        .bind(Uuid::from(*principal))
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| database(&error))?
        .get(0))
}

/// Create one login after password and environment authorization succeed.
///
/// The caller must lock the principal before password verification, not just
/// here, so reset cannot commit between verification and creation.
pub async fn create_login(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &PrincipalId,
    issuer: &str,
    audience: &str,
) -> Result<Option<Renewal>, IdentityError> {
    if !lock_principal(tx, principal).await? {
        return Ok(None);
    }
    let row = sqlx::query("WITH instant AS (SELECT date_trunc('second',clock_timestamp()) AS at) \
         INSERT INTO identity.password_logins (principal_id,issuer,audience,authenticated_at,expires_at,renewal_expires_at) \
         SELECT $1,$2,$3,at,at+($4::bigint*interval '1 second'),at+($5::bigint*interval '1 second') FROM instant \
         RETURNING id::text,principal_id,extract(epoch FROM authenticated_at)::bigint,extract(epoch FROM expires_at)::bigint").bind(Uuid::from(*principal)).bind(issuer).bind(audience).bind(ABSOLUTE_LIFETIME).bind(INACTIVITY_LIFETIME).fetch_one(&mut **tx).await.map_err(|error| database(&error))?;
    Ok(Some(credential(tx, decode(&row)?).await?))
}

// Lookup before locking reveals only the principal to lock. Every credential
// fact is re-read in a separate statement after the principal lock is held.
async fn lookup(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    issuer: &str,
    audience: &str,
    digest: &wamn_digest::Digest,
) -> Result<Option<(Login, bool, bool)>, IdentityError> {
    let digest = digest.to_string();
    let Some(principal) = sqlx::query_scalar::<_, Uuid>("SELECT l.principal_id FROM identity.renewal_credentials c JOIN identity.password_logins l ON l.id=c.login_id \
         WHERE c.token_hash=$1 AND l.issuer=$2 AND l.audience=$3").bind(digest.as_str()).bind(issuer).bind(audience).fetch_optional(&mut **tx).await.map_err(|error| database(&error))? else { return Ok(None) };
    let principal = PrincipalId::from(principal);
    if !lock_principal(tx, &principal).await? {
        return Ok(None);
    }
    let row = sqlx::query("SELECT l.id::text,l.principal_id,extract(epoch FROM l.authenticated_at)::bigint,extract(epoch FROM l.expires_at)::bigint, \
         c.consumed_at IS NOT NULL, l.revoked_at IS NULL AND l.expires_at>clock_timestamp() AND l.renewal_expires_at>clock_timestamp() \
         FROM identity.renewal_credentials c JOIN identity.password_logins l ON l.id=c.login_id \
         WHERE c.token_hash=$1 AND l.issuer=$2 AND l.audience=$3 FOR UPDATE OF l").bind(digest.as_str()).bind(issuer).bind(audience).fetch_optional(&mut **tx).await.map_err(|error| database(&error))?;
    row.map(|row| Ok((decode(&row)?, row.get(4), row.get(5))))
        .transpose()
}

/// Find the principal for a renewal credential before opening its write transaction.
pub async fn renewal_principal(
    connection: &mut sqlx::PgConnection,
    issuer: &str,
    audience: &str,
    secret: &str,
) -> Result<Option<PrincipalId>, IdentityError> {
    let Ok(token) = SecretToken::parse(SecretType::Renewal, secret) else {
        return Ok(None);
    };
    let actor = wamn_identity_protocol::PlatformComponent::IdentityService.principal_id();
    let mut transaction = super::transaction::read_connection(connection, &actor)
        .await
        .map_err(|error| database(&error))?;
    let principal = sqlx::query_scalar::<_, Uuid>("SELECT l.principal_id FROM identity.renewal_credentials c JOIN identity.password_logins l ON l.id=c.login_id WHERE c.token_hash=$1 AND l.issuer=$2 AND l.audience=$3")
        .bind(token.digest().to_string()).bind(issuer).bind(audience)
        .fetch_optional(&mut *transaction).await.map_err(|error| database(&error))?;
    transaction
        .commit()
        .await
        .map_err(|error| database(&error))?;
    Ok(principal.map(PrincipalId::from))
}

/// Authenticate a renewal under the principal lock. Commit refusals that revoke a replayed family.
pub async fn authenticate_renewal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &PrincipalId,
    issuer: &str,
    audience: &str,
    secret: &str,
) -> Result<Option<AuthenticatedPrincipal>, IdentityError> {
    wamn_postgres::bind_transaction_identity(tx, principal, "", "", None, None)
        .await
        .map_err(|error| database(&error))?;
    let Some(digest) = SecretToken::parse(SecretType::Renewal, secret)
        .ok()
        .map(|token| token.digest())
    else {
        return Ok(None);
    };
    let Some((login, consumed, usable)) = lookup(tx, issuer, audience, &digest).await? else {
        return Ok(None);
    };
    if &login.principal != principal {
        return Ok(None);
    }
    if consumed {
        revoke_family(tx, &login).await?;
        return Ok(None);
    }
    if !usable {
        return Ok(None);
    }
    Ok(resolve_principal(tx, &login.principal)
        .await?
        .map(AuthenticatedPrincipal::admitted))
}

/// Consume a credential once and create its replacement in the same transaction.
///
/// `None` refuses authentication. Commit that outcome to retain replay revocation.
/// On success, authorize current environment access and sign before committing.
pub async fn rotate_login(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    issuer: &str,
    audience: &str,
    secret: &str,
) -> Result<Option<Renewal>, IdentityError> {
    let Some(digest) = SecretToken::parse(SecretType::Renewal, secret)
        .ok()
        .map(|token| token.digest())
    else {
        return Ok(None);
    };
    let Some((login, consumed, usable)) = lookup(tx, issuer, audience, &digest).await? else {
        return Ok(None);
    };
    if consumed {
        revoke_family(tx, &login).await?;
        return Ok(None);
    }
    if !usable {
        return Ok(None);
    }
    sqlx::query(
        "UPDATE identity.renewal_credentials SET consumed_at=clock_timestamp() WHERE token_hash=$1",
    )
    .bind(digest.to_string())
    .execute(&mut **tx)
    .await
    .map(|result| result.rows_affected())
    .map_err(|error| database(&error))?;
    sqlx::query("UPDATE identity.password_logins SET renewal_expires_at=LEAST(expires_at,clock_timestamp()+($2::bigint*interval '1 second')) WHERE id=$1::text::uuid").bind(&login.id).bind(INACTIVITY_LIFETIME).execute(&mut **tx).await.map(|result| result.rows_affected()).map_err(|error| database(&error))?;
    Ok(Some(credential(tx, login).await?))
}
async fn revoke_family(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    login: &Login,
) -> Result<(), IdentityError> {
    sqlx::query("UPDATE identity.password_logins SET revoked_at=COALESCE(revoked_at,clock_timestamp()) WHERE id=$1::text::uuid").bind(&login.id).execute(&mut **tx).await.map(|result| result.rows_affected()).map_err(|error| database(&error))?;
    Ok(())
}

/// Revoke the credential's family, including an already consumed credential.
pub async fn revoke_login(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    issuer: &str,
    audience: &str,
    secret: &str,
) -> Result<(), IdentityError> {
    if let Some(digest) = SecretToken::parse(SecretType::Renewal, secret)
        .ok()
        .map(|token| token.digest())
        && let Some((login, _, _)) = lookup(tx, issuer, audience, &digest).await?
    {
        revoke_family(tx, &login).await?;
    }
    Ok(())
}

/// Revoke every family for a principal, across issuers and environments.
///
/// The caller must authorize logout-all or password reset before calling.
pub async fn revoke_all(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &PrincipalId,
) -> Result<(), IdentityError> {
    lock_principal(tx, principal).await?;
    sqlx::query("UPDATE identity.password_logins SET revoked_at=clock_timestamp() WHERE principal_id=$1 AND revoked_at IS NULL").bind(Uuid::from(*principal)).execute(&mut **tx).await.map(|result| result.rows_affected()).map_err(|error| database(&error))?;
    Ok(())
}

/// Remove at most 100 absolutely expired families and their consumed evidence.
pub async fn prune_expired(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    issuer: &str,
) -> Result<u64, IdentityError> {
    sqlx::query("DELETE FROM identity.password_logins WHERE id IN (SELECT id FROM identity.password_logins WHERE issuer=$1 AND expires_at<=clock_timestamp() ORDER BY expires_at,id LIMIT 100 FOR UPDATE SKIP LOCKED)").bind(issuer).execute(&mut **tx).await.map(|result| result.rows_affected()).map_err(|error| database(&error))
}
