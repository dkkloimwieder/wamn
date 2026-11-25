//! User password enrollment and authentication, without HTTP or session lifecycle.
//!
//! Writers own a transaction and bind the actor. Enrollment serializes on the
//! principal row, then consumes every outstanding invitation atomically. The
//! service must share one password-work budget across all its requests.

use crate::password::{
    INVITATION_PURPOSE, Invitation, Password, PasswordError, PasswordErrorType, PasswordWork,
    RESET_PURPOSE, RefusalCause, enrollment_policy, failure, infrastructure, refused,
    refused_because, secret_type, token_hash,
};
use crate::principal::{AuthenticatedPrincipal, Principal};
use sqlx::PgConnection as Client;
use sqlx::types::Uuid;
use sqlx::{Connection as _, Row as _};
use wamn_authentication::identity_error::IdentityError;
use wamn_identity_protocol::{PrincipalId, secret::SecretToken};

use super::decode_principal;

fn database(source: &sqlx::Error) -> PasswordError {
    // DbError Display contains row-bearing DETAIL/HINT. Never retain it in the
    // error chain, which callers often log in full.
    let source = std::io::Error::other(
        source
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .map_or("database failure".to_owned(), |code| {
                format!("SQLSTATE {code}")
            }),
    );
    infrastructure("password database operation failed", source)
}

async fn bind_actor(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &PrincipalId,
    operation: Option<&str>,
) -> Result<(), PasswordError> {
    wamn_postgres::bind_transaction_identity(tx, actor, "", "", operation, None)
        .await
        .map_err(|source| database(&source))
}
/// Lock the principal, and return the cause and its reason when it cannot
/// take this operation.
async fn lock_account(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &PrincipalId,
    enrolled: bool,
) -> Result<Option<(RefusalCause, Option<&'static str>)>, PasswordError> {
    let row = sqlx::query("SELECT identity.lock_password_principal($1)")
        .bind(Uuid::from(*principal))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|source| database(&source))?;
    if !row.is_some_and(|row| row.get::<bool, _>(0)) {
        return Ok(Some((RefusalCause::NoSuchAccount, None)));
    }
    let has_password = sqlx::query(
        "SELECT EXISTS (SELECT 1 FROM identity.password_credentials WHERE principal_id = $1)",
    )
    .bind(Uuid::from(*principal))
    .fetch_one(&mut **tx)
    .await
    .map_err(|source| database(&source))?
    .get::<bool, _>(0);
    Ok(match (enrolled, has_password) {
        (false, true) => Some((RefusalCause::AlreadyEnrolled, None)),
        (true, false) => Some((RefusalCause::NoSuchAccount, Some("no-password"))),
        _ => None,
    })
}

/// Issue an invitation for an active, unenrolled user under an authorized actor.
///
/// The caller must authenticate and authorize the operator before calling this
/// library. The database client must hold identity-writer authority.
pub async fn issue_invitation(
    client: &mut Client,
    actor: &PrincipalId,
    principal: &PrincipalId,
) -> Result<Invitation, PasswordError> {
    issue_token(client, actor, principal, INVITATION_PURPOSE, None).await
}

/// Issue a reset credential for an active user who already has a password.
pub async fn issue_reset(
    client: &mut Client,
    actor: &PrincipalId,
    principal: &PrincipalId,
    email: &str,
) -> Result<Invitation, PasswordError> {
    issue_token(client, actor, principal, RESET_PURPOSE, Some(email)).await
}

async fn issue_token(
    client: &mut Client,
    actor: &PrincipalId,
    principal: &PrincipalId,
    purpose: &str,
    expected_email: Option<&str>,
) -> Result<Invitation, PasswordError> {
    let lifetime: i64 = if purpose == INVITATION_PURPOSE {
        86400
    } else {
        900
    };
    let token = SecretToken::mint(secret_type(purpose)).map_err(|_| {
        failure(
            PasswordErrorType::Infrastructure,
            "email credential entropy failed",
        )
    })?;
    let hash = token.digest();
    let secret = token.into_secret();
    let mut tx = client.begin().await.map_err(|source| database(&source))?;
    bind_actor(&mut tx, actor, None).await?;
    if let Some((cause, reason)) =
        lock_account(&mut tx, principal, purpose == RESET_PURPOSE).await?
    {
        return Err(refused_because("invitation refused", cause, reason));
    }
    if let Some(email) = expected_email {
        let current: String = sqlx::query("SELECT email FROM identity.principals WHERE id=$1")
            .bind(Uuid::from(*principal))
            .fetch_one(&mut *tx)
            .await
            .map_err(|source| database(&source))?
            .get(0);
        if current != email {
            return Err(refused_because(
                "recovery address changed",
                RefusalCause::NoSuchAccount,
                Some("address-changed"),
            ));
        }
    }
    let expires_at: String = sqlx::query("INSERT INTO identity.password_tokens (token_hash, principal_id, purpose, expires_at) VALUES ($1, $2, $3, clock_timestamp() + $4::bigint * interval '1 second') RETURNING to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI')").bind(hash.to_string()).bind(Uuid::from(*principal)).bind(purpose).bind(lifetime).fetch_one(&mut *tx).await.map_err(|source| database(&source))?.get(0);
    tx.commit().await.map_err(|source| database(&source))?;
    Ok(Invitation::new(secret, expires_at))
}
/// Return the cause when the token cannot establish this principal's password.
async fn usable_token(
    client: &mut sqlx::PgConnection,
    principal: &PrincipalId,
    hash: &wamn_digest::Digest,
    purpose: &str,
) -> Result<Option<RefusalCause>, PasswordError> {
    let row = sqlx::query("SELECT t.consumed_at IS NOT NULL, t.expires_at <= clock_timestamp(), p.active AND p.type = 'user' FROM identity.password_tokens t JOIN identity.principals p ON p.id = t.principal_id WHERE t.token_hash = $1 AND t.principal_id = $2 AND t.purpose = $3").bind(hash.to_string()).bind(Uuid::from(*principal)).bind(purpose).fetch_optional(&mut *client).await.map_err(|source| database(&source))?;
    let Some(row) = row else {
        return Ok(Some(RefusalCause::UnknownSecret));
    };
    Ok(if row.get::<bool, _>(0) {
        Some(RefusalCause::Consumed)
    } else if row.get::<bool, _>(1) {
        Some(RefusalCause::Expired)
    } else if !row.get::<bool, _>(2) {
        Some(RefusalCause::NoSuchAccount)
    } else {
        None
    })
}

/// Establish a user's first password using a correctly bound invitation.
///
/// Expensive hashing precedes the principal lock. The transaction tests again
/// the invitation and active, unenrolled principal after acquiring that lock.
pub async fn enroll_password(
    client: &mut Client,
    work: &PasswordWork,
    principal: &PrincipalId,
    secret: &str,
    password: Password,
) -> Result<(), PasswordError> {
    establish_password(
        client,
        work,
        principal,
        secret,
        password,
        INVITATION_PURPOSE,
    )
    .await
}

/// Replace a password, consume all email credentials, and revoke renewal families atomically.
/// The caller sends notification after success and requires ordinary login.
pub async fn reset_password(
    client: &mut Client,
    work: &PasswordWork,
    principal: &PrincipalId,
    secret: &str,
    password: Password,
) -> Result<(), PasswordError> {
    establish_password(client, work, principal, secret, password, RESET_PURPOSE).await?;
    // The reset revoked every login of the principal (store_password).
    Ok(())
}

async fn establish_password(
    client: &mut Client,
    work: &PasswordWork,
    principal: &PrincipalId,
    secret: &str,
    password: Password,
    purpose: &str,
) -> Result<(), PasswordError> {
    enrollment_policy(&password)?;
    let digest = token_hash(secret, purpose)
        .ok_or_else(|| refused("invitation refused", RefusalCause::UnknownSecret))?;
    let mut read = client
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .await
        .map_err(|source| database(&source))?;
    bind_actor(&mut read, principal, None).await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *read)
        .await
        .map_err(|source| database(&source))?;
    let cause = usable_token(&mut read, principal, &digest, purpose).await?;
    read.commit().await.map_err(|source| database(&source))?;
    if let Some(cause) = cause {
        return Err(refused("invitation refused", cause));
    }
    let hash = work.hash(password).await?;
    store_password(client, principal, &digest, &hash, purpose).await
}

async fn store_password(
    client: &mut Client,
    principal: &PrincipalId,
    token_digest: &wamn_digest::Digest,
    hash: &str,
    purpose: &str,
) -> Result<(), PasswordError> {
    let mut tx = client.begin().await.map_err(|source| database(&source))?;
    let operation = if purpose == RESET_PURPOSE {
        "admin:password-reset"
    } else {
        "admin:password-enroll"
    };
    bind_actor(&mut tx, principal, Some(operation)).await?;
    if let Some((cause, reason)) =
        lock_account(&mut tx, principal, purpose == RESET_PURPOSE).await?
    {
        return Err(refused_because("invitation refused", cause, reason));
    }
    if let Some(cause) = usable_token(&mut tx, principal, token_digest, purpose).await? {
        return Err(refused("invitation refused", cause));
    }
    if purpose == RESET_PURPOSE {
        sqlx::query(
            "UPDATE identity.password_credentials SET password_hash=$2 WHERE principal_id=$1",
        )
        .bind(Uuid::from(*principal))
        .bind(hash)
        .execute(&mut *tx)
        .await
        .map(|result| result.rows_affected())
        .map_err(|source| database(&source))?;
        // reset_password records the drop of this revoke after the commit.
        super::password_login::revoke_all(&mut tx, principal)
            .await
            .map_err(|source| infrastructure("reset revocation failed", source))?;
    } else {
        sqlx::query("INSERT INTO identity.password_credentials (principal_id, password_hash) VALUES ($1, $2)").bind(Uuid::from(*principal)).bind(hash).execute(&mut *tx).await.map(|result| result.rows_affected()).map_err(|source| database(&source))?;
    }
    sqlx::query("UPDATE identity.password_tokens SET consumed_at = clock_timestamp() WHERE principal_id = $1 AND consumed_at IS NULL").bind(Uuid::from(*principal)).execute(&mut *tx).await.map(|result| result.rows_affected()).map_err(|source| database(&source))?;
    tx.commit().await.map_err(|source| database(&source))
}

struct PasswordCandidate {
    hash: String,
    active: bool,
    principal: Result<Principal, IdentityError>,
}

async fn password_candidate(
    client: &mut Client,
    email: &str,
) -> Result<Option<PasswordCandidate>, PasswordError> {
    let row = sqlx::query("SELECT p.id, p.type, p.subject, p.display_name, p.active, c.password_hash FROM identity.principals p JOIN identity.password_credentials c ON c.principal_id = p.id WHERE p.email = $1 AND p.type = 'user'")
        .bind(email)
        .fetch_optional(client)
        .await
        .map_err(|source| database(&source))?;
    Ok(row.map(|row| PasswordCandidate {
        hash: row.get(5),
        active: row.get(4),
        principal: decode_principal(&row),
    }))
}

/// Authenticate with the caller's password budget and active user records.
pub async fn authenticate_password(
    client: &mut Client,
    work: &PasswordWork,
    email: &str,
    password: Password,
) -> Result<Option<AuthenticatedPrincipal>, PasswordError> {
    let Some(candidate) = password_candidate(client, email).await? else {
        work.hash(password).await?;
        return Ok(None);
    };
    let valid = work.verify(password, candidate.hash).await?;
    if !valid || !candidate.active {
        return Ok(None);
    }
    candidate
        .principal
        .map(AuthenticatedPrincipal::admitted)
        .map(Some)
        .map_err(|source| infrastructure("stored password principal refused", source))
}
