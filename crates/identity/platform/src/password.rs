//! Human password enrollment and authentication, without HTTP or session lifecycle.
//!
//! Writers own a transaction and bind the actor. Enrollment serializes on the
//! principal row, then consumes every outstanding invitation atomically. The
//! service must share one password-work budget across all its requests.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString},
};
use ring::rand::{SecureRandom as _, SystemRandom};
use sha2::{Digest as _, Sha256};
use tokio::sync::Semaphore;
use tokio_postgres::{Client, GenericClient, Transaction};
use zeroize::Zeroizing;

use crate::{AuthenticatedPrincipal, PrincipalId, decode_principal};

/// Maximum UTF-8 password input; at least 64 Unicode characters fit.
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Minimum enrollment length, counted as Unicode characters without trimming.
pub const MIN_PASSWORD_CHARACTERS: usize = 15;
/// Argon2id memory in KiB, using the OWASP minimum profile.
pub const PASSWORD_MEMORY_KIB: u32 = 19 * 1024;
/// Two simultaneous jobs use at most 38 MiB of Argon2 working memory.
pub const MAX_PASSWORD_JOBS: usize = 2;
const INVITATION_PREFIX: &str = "wamn_inv_";
const INVITATION_PURPOSE: &str = "invitation";

/// Failure classes inside the password owner, not HTTP or WIT outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordErrorKind {
    /// Enrollment input violates password policy.
    Policy,
    /// The credential or principal cannot perform this operation.
    Refused,
    /// All bounded password workers are occupied.
    Busy,
    /// Identity storage or password worker infrastructure failed.
    Infrastructure,
}

/// Password failure with fixed context and an optional internal source.
pub struct PasswordError {
    kind: PasswordErrorKind,
    operation: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}
impl PasswordError {
    /// Return the internal failure class.
    pub fn kind(&self) -> PasswordErrorKind {
        self.kind
    }
}
impl fmt::Display for PasswordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.operation)
    }
}
impl fmt::Debug for PasswordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordError")
            .field("kind", &self.kind)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}
impl Error for PasswordError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
fn failure(kind: PasswordErrorKind, operation: &'static str) -> PasswordError {
    PasswordError {
        kind,
        operation,
        source: None,
    }
}
fn infrastructure(
    operation: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> PasswordError {
    PasswordError {
        kind: PasswordErrorKind::Infrastructure,
        operation,
        source: Some(Box::new(source)),
    }
}
fn database(source: &tokio_postgres::Error) -> PasswordError {
    // DbError Display contains row-bearing DETAIL/HINT. Never retain it in the
    // error chain, which callers often log in full.
    let source =
        std::io::Error::other(source.code().map_or("database failure".to_owned(), |code| {
            format!("SQLSTATE {}", code.code())
        }));
    infrastructure("password database operation failed", source)
}

/// Owned password input that redacts diagnostics and erases its bytes on drop.
pub struct Password(Zeroizing<String>);
impl Password {
    /// Take ownership without trimming or normalizing the password.
    pub fn new(value: String) -> Result<Self, PasswordError> {
        let value = Zeroizing::new(value);
        if value.is_empty() || value.len() > MAX_PASSWORD_BYTES {
            return Err(failure(
                PasswordErrorKind::Policy,
                "password length refused",
            ));
        }
        Ok(Self(value))
    }
}
impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// Shared admission budget for hashing and verification, with no waiting queue.
#[derive(Clone, Debug)]
pub struct PasswordWork {
    permits: Arc<Semaphore>,
}
/// Create once per identity service and clone for each request handler.
pub fn password_work() -> PasswordWork {
    PasswordWork {
        permits: Arc::new(Semaphore::new(MAX_PASSWORD_JOBS)),
    }
}
fn argon() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(PASSWORD_MEMORY_KIB, 2, 1, Some(32))
            .expect("fixed Argon2 parameters are valid"),
    )
}
impl PasswordWork {
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, PasswordError> + Send + 'static,
    ) -> Result<T, PasswordError> {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| failure(PasswordErrorKind::Busy, "password workers busy"))?;
        tokio::task::spawn_blocking(move || {
            // Cancellation of the async caller must not free this budget while
            // the blocking worker still runs.
            let _permit = permit;
            work()
        })
        .await
        .map_err(|source| infrastructure("password worker failed", source))?
    }
    async fn hash(&self, password: Password) -> Result<String, PasswordError> {
        self.run(move || {
            let mut salt = [0u8; 16];
            SystemRandom::new().fill(&mut salt).map_err(|_| {
                failure(
                    PasswordErrorKind::Infrastructure,
                    "password salt generation failed",
                )
            })?;
            let salt = SaltString::encode_b64(&salt).expect("fixed salt fits base64 buffer");
            argon()
                .hash_password(password.0.as_bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|source| infrastructure("password hashing failed", source))
        })
        .await
    }
    async fn verify(&self, password: Password, hash: String) -> Result<bool, PasswordError> {
        self.run(move || {
            let hash = PasswordHash::new(&hash)
                .map_err(|source| infrastructure("stored password hash refused", source))?;
            let params = Params::try_from(&hash)
                .map_err(|source| infrastructure("stored password parameters refused", source))?;
            if hash.algorithm.as_str() != "argon2id"
                || hash.version != Some(19)
                || params != *argon().params()
                || hash.salt.is_none()
                || hash.hash.is_none()
            {
                return Err(failure(
                    PasswordErrorKind::Infrastructure,
                    "stored password profile refused",
                ));
            }
            match argon().verify_password(password.0.as_bytes(), &hash) {
                Ok(()) => Ok(true),
                Err(argon2::password_hash::Error::Password) => Ok(false),
                Err(source) => Err(infrastructure("password verification failed", source)),
            }
        })
        .await
    }
}

/// A one-time invitation secret, never persisted in bearer form.
pub struct Invitation(Zeroizing<String>);
impl Invitation {
    /// Supply this secret to the intended recipient once; never log it.
    pub fn secret(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invitation(<redacted>)")
    }
}
fn token_hash(secret: &str) -> Option<Vec<u8>> {
    let suffix = secret.strip_prefix(INVITATION_PREFIX)?;
    if suffix.len() != 64
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    Some(Sha256::digest(secret.as_bytes()).to_vec())
}
async fn bind_actor(tx: &Transaction<'_>, actor: &PrincipalId) -> Result<(), PasswordError> {
    tx.execute(
        "SELECT set_config('app.user_id', $1, true)",
        &[&actor.as_str()],
    )
    .await
    .map_err(|source| database(&source))?;
    Ok(())
}
async fn lock_unenrolled(
    tx: &Transaction<'_>,
    principal: &PrincipalId,
) -> Result<bool, PasswordError> {
    let row = tx.query_opt("SELECT status = 'active' AND kind = 'human' FROM identity.principals WHERE id = $1::text::uuid FOR UPDATE", &[&principal.as_str()]).await.map_err(|source| database(&source))?;
    if !row.is_some_and(|row| row.get::<_, bool>(0)) {
        return Ok(false);
    }
    Ok(!tx.query_one("SELECT EXISTS (SELECT 1 FROM identity.password_credentials WHERE principal_id = $1::text::uuid)", &[&principal.as_str()]).await.map_err(|source| database(&source))?.get::<_, bool>(0))
}

/// Issue an invitation for an active, unenrolled human under an authorized actor.
///
/// The caller must authenticate and authorize the operator before calling this
/// library. The database client must hold identity-writer authority.
pub async fn issue_invitation(
    client: &mut Client,
    actor: &PrincipalId,
    principal: &PrincipalId,
) -> Result<Invitation, PasswordError> {
    let mut bytes = [0u8; 32];
    SystemRandom::new().fill(&mut bytes).map_err(|_| {
        failure(
            PasswordErrorKind::Infrastructure,
            "invitation entropy failed",
        )
    })?;
    let secret = Invitation(Zeroizing::new(format!(
        "{INVITATION_PREFIX}{}",
        hex::encode(bytes)
    )));
    let hash = token_hash(secret.secret()).expect("generated invitation is valid");
    let tx = client
        .transaction()
        .await
        .map_err(|source| database(&source))?;
    if !lock_unenrolled(&tx, principal).await? {
        return Err(failure(PasswordErrorKind::Refused, "invitation refused"));
    }
    bind_actor(&tx, actor).await?;
    tx.execute("INSERT INTO identity.password_tokens (token_hash, principal_id, purpose, expires_at) VALUES ($1, $2::text::uuid, $3, clock_timestamp() + interval '24 hours')", &[&hash, &principal.as_str(), &INVITATION_PURPOSE]).await.map_err(|source| database(&source))?;
    tx.commit().await.map_err(|source| database(&source))?;
    Ok(secret)
}
async fn usable_invitation(
    client: &(impl GenericClient + Sync),
    principal: &PrincipalId,
    hash: &[u8],
) -> Result<bool, PasswordError> {
    client.query_one("SELECT EXISTS (SELECT 1 FROM identity.password_tokens t JOIN identity.principals p ON p.id = t.principal_id WHERE t.token_hash = $1 AND t.principal_id = $2::text::uuid AND t.purpose = $3 AND t.consumed_at IS NULL AND t.expires_at > clock_timestamp() AND p.status = 'active' AND p.kind = 'human')", &[&hash, &principal.as_str(), &INVITATION_PURPOSE]).await.map(|row| row.get(0)).map_err(|source| database(&source))
}

/// Server-supplied SHA-256 digests of disallowed passwords, compared exactly.
///
/// The service loads its deployment-approved common/compromised password list.
/// An empty list is refused. This type performs no network or filesystem access.
#[derive(Debug)]
pub struct PasswordBlocklist(HashSet<[u8; 32]>);
impl PasswordBlocklist {
    /// Build from the service's complete local list of password digests.
    pub fn new(digests: impl IntoIterator<Item = [u8; 32]>) -> Result<Self, PasswordError> {
        let digests: HashSet<_> = digests.into_iter().collect();
        if digests.is_empty() {
            return Err(failure(
                PasswordErrorKind::Policy,
                "password blocklist is empty",
            ));
        }
        Ok(Self(digests))
    }
}
fn enrollment_policy(
    password: &Password,
    blocked: &PasswordBlocklist,
) -> Result<(), PasswordError> {
    if password.0.chars().count() < MIN_PASSWORD_CHARACTERS
        || blocked
            .0
            .contains(&<[u8; 32]>::from(Sha256::digest(password.0.as_bytes())))
    {
        return Err(failure(
            PasswordErrorKind::Policy,
            "password policy refused",
        ));
    }
    Ok(())
}

/// Establish a human's first password using a correctly bound invitation.
///
/// Expensive hashing precedes the principal lock. The transaction rechecks
/// the invitation and active, unenrolled principal after acquiring that lock.
pub async fn enroll_password(
    client: &mut Client,
    work: &PasswordWork,
    blocked: &PasswordBlocklist,
    principal: &PrincipalId,
    secret: &str,
    password: Password,
) -> Result<(), PasswordError> {
    enrollment_policy(&password, blocked)?;
    let digest = token_hash(secret)
        .ok_or_else(|| failure(PasswordErrorKind::Refused, "invitation refused"))?;
    if !usable_invitation(client, principal, &digest).await? {
        return Err(failure(PasswordErrorKind::Refused, "invitation refused"));
    }
    let hash = work.hash(password).await?;
    store_enrollment(client, principal, &digest, &hash).await
}

async fn store_enrollment(
    client: &mut Client,
    principal: &PrincipalId,
    token_digest: &[u8],
    hash: &str,
) -> Result<(), PasswordError> {
    let tx = client
        .transaction()
        .await
        .map_err(|source| database(&source))?;
    if !lock_unenrolled(&tx, principal).await?
        || !usable_invitation(&tx, principal, token_digest).await?
    {
        return Err(failure(PasswordErrorKind::Refused, "invitation refused"));
    }
    bind_actor(&tx, principal).await?;
    tx.execute("INSERT INTO identity.password_credentials (principal_id, password_hash) VALUES ($1::text::uuid, $2)", &[&principal.as_str(), &hash]).await.map_err(|source| database(&source))?;
    tx.execute("UPDATE identity.password_tokens SET consumed_at = clock_timestamp() WHERE principal_id = $1::text::uuid AND consumed_at IS NULL", &[&principal.as_str()]).await.map_err(|source| database(&source))?;
    tx.commit().await.map_err(|source| database(&source))
}

/// Authenticate a password without establishing environment membership or roles.
///
/// The service still owns source/account throttling and all admission checks.
/// Unknown, disabled, and unenrolled accounts perform the same bounded hash work.
pub async fn authenticate_password(
    client: &(impl GenericClient + Sync),
    work: &PasswordWork,
    email: &str,
    password: Password,
) -> Result<Option<AuthenticatedPrincipal>, PasswordError> {
    let row = client.query_opt("SELECT p.id::text, p.kind, p.subject, p.display_name, p.status, c.password_hash FROM identity.principals p JOIN identity.password_credentials c ON c.principal_id = p.id WHERE p.email = $1 AND p.kind = 'human'", &[&email]).await.map_err(|source| database(&source))?;
    let Some(row) = row else {
        work.hash(password).await?;
        return Ok(None);
    };
    let hash: String = row.get(5);
    let valid = work.verify(password, hash).await?;
    if !valid || row.get::<_, &str>(4) != "active" {
        return Ok(None);
    }
    let principal = decode_principal(&row)
        .map_err(|source| infrastructure("stored password principal refused", source))?;
    Ok(Some(AuthenticatedPrincipal { principal }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_policy_counts_characters_and_refuses_blocked_values() {
        let blocked =
            PasswordBlocklist::new([Sha256::digest(b"a commonly compromised password").into()])
                .unwrap();
        assert!(PasswordBlocklist::new([]).is_err());
        assert!(Password::new("x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
        assert!(enrollment_policy(&Password::new("界".repeat(14)).unwrap(), &blocked).is_err());
        assert!(enrollment_policy(&Password::new("界".repeat(15)).unwrap(), &blocked).is_ok());
        assert!(enrollment_policy(&Password::new("界".repeat(64)).unwrap(), &blocked).is_ok());
        assert!(
            enrollment_policy(
                &Password::new("a commonly compromised password".into()).unwrap(),
                &blocked
            )
            .is_err()
        );
        let password = Password::new("  spaces remain part of me  ".into()).unwrap();
        assert_eq!(&*password.0, "  spaces remain part of me  ");
        assert!(!format!("{password:?}").contains("spaces"));
    }

    #[tokio::test]
    async fn hash_round_trip_salts_and_bounds_stored_work() {
        let work = password_work();
        let hash = work
            .hash(Password::new("a sufficiently long password".into()).unwrap())
            .await
            .unwrap();
        let other = work
            .hash(Password::new("a sufficiently long password".into()).unwrap())
            .await
            .unwrap();
        assert_ne!(hash, other);
        assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(
            work.verify(
                Password::new("a sufficiently long password".into()).unwrap(),
                hash.clone()
            )
            .await
            .unwrap()
        );
        assert!(
            !work
                .verify(
                    Password::new("wrong password".into()).unwrap(),
                    hash.clone()
                )
                .await
                .unwrap()
        );
        let oversized = hash.replace("m=19456", "m=4194304");
        assert_eq!(
            work.verify(Password::new("test".into()).unwrap(), oversized)
                .await
                .unwrap_err()
                .kind(),
            PasswordErrorKind::Infrastructure
        );
        let wrong_version = hash.replace("v=19", "v=16");
        assert!(
            work.verify(Password::new("test".into()).unwrap(), wrong_version)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cancelled_hash_call_keeps_its_budget_until_worker_finishes() {
        let work = password_work();
        let mut tasks = Vec::new();
        let mut releases = Vec::new();
        for _ in 0..MAX_PASSWORD_JOBS {
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            releases.push(release);
            let owned = work.clone();
            tasks.push(tokio::spawn(async move {
                owned
                    .run(move || {
                        started.send(()).unwrap();
                        wait.recv().unwrap();
                        Ok(())
                    })
                    .await
            }));
            ready.await.unwrap();
        }
        for task in tasks {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        }
        assert_eq!(
            work.run(|| Ok(())).await.unwrap_err().kind(),
            PasswordErrorKind::Busy
        );
        for release in releases {
            release.send(()).unwrap();
        }
        let permits = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            work.permits
                .acquire_many(u32::try_from(MAX_PASSWORD_JOBS).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        drop(permits);
        work.run(|| Ok(())).await.unwrap();
    }
}
