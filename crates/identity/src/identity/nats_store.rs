//! Envelope-encrypted NKey seeds and the environment account's claim revision.
//! The account's session lock spans SQL changes and resolver publication.

use nkeys::{KeyPair, KeyPairType};
use sqlx::{Connection as _, PgConnection, Postgres, Transaction};
use wamn_envelope::encryption::{Encrypted, Envelope};
use wamn_registry::{EnvName, OrgId};
use zeroize::Zeroizing;

use crate::nats::{Account, Error, Key};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Scope<'a> {
    pub org: Option<&'a str>,
    pub env: Option<&'a str>,
}

struct StoredKey {
    seed: Vec<u8>,
    public: String,
    issued_at: i64,
}

#[derive(PartialEq, Eq, sqlx::FromRow)]
struct StoredEnv {
    public_key: String,
    issued_at: i64,
    project_count: i32,
}

enum PreparedEnv {
    Existing {
        account: StoredEnv,
        resolver_revision: Option<i64>,
    },
    New {
        subject: String,
        packed: Vec<u8>,
    },
}

pub(super) fn database_error(source: sqlx::Error) -> Error {
    Error::new("access NATS key storage", source)
}

pub(super) async fn lock(tx: &mut Transaction<'_, Postgres>, scope: &str) -> Result<(), Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 1376598821))")
        .bind(scope)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    bind_actor(tx).await
}

async fn bind_actor(tx: &mut Transaction<'_, Postgres>) -> Result<(), Error> {
    super::session_keys::bind_identity_service(tx)
        .await
        .map_err(|source| Error::new("bind NATS key storage authority", source))
}

pub(super) async fn with_env_lock<T>(
    mut client: PgConnection,
    issuer: &str,
    (org, env): (&OrgId, &EnvName),
    work: impl AsyncFnOnce(&mut PgConnection) -> Result<T, Error>,
) -> Result<T, Error> {
    // This direct connection is owned here. Cancellation or a failed unlock
    // drops the socket, so the session can never be reused with a held lock.
    sqlx::query("SET lock_timeout = '10s'")
        .execute(&mut client)
        .await
        .map_err(database_error)?;
    let key: i64 = sqlx::query_scalar(
        "SELECT hashtextextended(
            jsonb_build_array('wamn-nats-env', $1::text, $2::text, $3::text)::text, 1376598821)",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_one(&mut client)
    .await
    .map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut client)
        .await
        .map_err(|source| Error::new("lock NATS environment account", source))?;

    let result = work(&mut client).await;
    let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .fetch_one(&mut client)
        .await
        .map_err(|source| Error::new("unlock NATS environment account", source))?;
    if !unlocked {
        return Err(Error::new(
            "unlock NATS environment account",
            std::io::Error::other("the session no longer holds the lock"),
        ));
    }
    result
}

pub(super) async fn platform_key(
    tx: &mut Transaction<'_, Postgres>,
    envelope: &Envelope,
    issuer: &str,
    key_name: &str,
    key_type: KeyPairType,
) -> Result<Key, Error> {
    ensure_key(tx, envelope, issuer, key_name, Scope::default(), key_type).await
}

async fn ensure_key(
    client: &mut PgConnection,
    envelope: &Envelope,
    issuer: &str,
    key_name: &str,
    scope: Scope<'_>,
    key_type: KeyPairType,
) -> Result<Key, Error> {
    if let Some(row) = read(client, issuer, key_name, scope).await? {
        return open(envelope, row).await;
    }
    let pair = KeyPair::new(key_type);
    let packed = seal(envelope, &pair).await?;
    let issued_at = sqlx::query_scalar(
        "INSERT INTO identity.nats_key
            (issuer, kind, org, env, public_key, private_key_envelope)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING issued_at",
    )
    .bind(issuer)
    .bind(key_name)
    .bind(scope.org)
    .bind(scope.env)
    .bind(pair.public_key())
    .bind(packed)
    .fetch_one(client)
    .await
    .map_err(database_error)?;
    Ok(Key { pair, issued_at })
}

async fn read_env(
    client: &mut PgConnection,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<Option<StoredEnv>, Error> {
    sqlx::query_as(
        "SELECT public_key, issued_at, project_count FROM identity.nats_key
         WHERE issuer = $1 AND kind = 'env' AND org = $2 AND env = $3",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_optional(client)
    .await
    .map_err(database_error)
}

pub(super) async fn ensure_env(
    client: &mut PgConnection,
    envelope: &Envelope,
    issuer: &str,
    (org, env): (&OrgId, &EnvName),
    project_count: i32,
    max_projects: i64,
    revision_to_replace: impl AsyncFnOnce(&str, i64) -> Result<Option<i64>, Error>,
) -> Result<Account, Error> {
    let scope = Scope {
        org: Some(org.as_str()),
        env: Some(env.as_str()),
    };
    let mut tx = client.begin().await.map_err(database_error)?;
    let existing = read_env(&mut tx, issuer, org, env).await?;
    let stored_signer = read(&mut tx, issuer, "env-signing", scope).await?;
    tx.commit().await.map_err(database_error)?;

    let prepared = if let Some(account) = existing {
        let resolver_revision = revision_to_replace(&account.public_key, account.issued_at).await?;
        PreparedEnv::Existing {
            account,
            resolver_revision,
        }
    } else {
        let pair = KeyPair::new_account();
        let packed = seal(envelope, &pair).await?;
        PreparedEnv::New {
            subject: pair.public_key(),
            packed,
        }
    };
    let (signer, new_signer) = if let Some(row) = stored_signer {
        (open(envelope, row).await?.pair, None)
    } else {
        let pair = KeyPair::new_account();
        let packed = seal(envelope, &pair).await?;
        (pair, Some(packed))
    };

    let mut tx = client.begin().await.map_err(database_error)?;
    let expected = match &prepared {
        PreparedEnv::Existing { account, .. } => Some(account),
        PreparedEnv::New { .. } => None,
    };
    if read_env(&mut tx, issuer, org, env).await?.as_ref() != expected {
        return Err(Error::new(
            "compare NATS environment account revision",
            std::io::Error::other("the stored account changed under its lock"),
        ));
    }
    let current = expected.map_or(0, |account| account.project_count);
    if project_count > current {
        reserve_projects(&mut tx, issuer, project_count - current, max_projects).await?;
    } else {
        bind_actor(&mut tx).await?;
    }

    let (subject, issued_at) = match prepared {
        PreparedEnv::Existing {
            account,
            resolver_revision: None,
        } if project_count == current => (account.public_key, account.issued_at),
        PreparedEnv::Existing {
            account,
            resolver_revision,
        } => {
            // Advance past both the committed and observed revisions.
            let issued_at = sqlx::query_scalar(
                "UPDATE identity.nats_key
                 SET issued_at = GREATEST(issued_at + 1, $5::bigint + 1, floor(EXTRACT(EPOCH FROM clock_timestamp()))::bigint),
                     project_count = $4
                 WHERE issuer = $1 AND kind = 'env' AND org = $2 AND env = $3
                 RETURNING issued_at",
            )
            .bind(issuer)
            .bind(org.as_str())
            .bind(env.as_str())
            .bind(project_count)
            .bind(resolver_revision.unwrap_or(account.issued_at))
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
            (account.public_key, issued_at)
        }
        PreparedEnv::New { subject, packed } => {
            let issued_at = sqlx::query_scalar(
                "INSERT INTO identity.nats_key
                    (issuer, kind, org, env, public_key, private_key_envelope, project_count)
                 VALUES ($1, 'env', $2, $3, $4, $5, $6) RETURNING issued_at",
            )
            .bind(issuer)
            .bind(org.as_str())
            .bind(env.as_str())
            .bind(&subject)
            .bind(packed)
            .bind(project_count)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
            (subject, issued_at)
        }
    };
    if let Some(packed) = new_signer {
        sqlx::query(
            "INSERT INTO identity.nats_key
                (issuer, kind, org, env, public_key, private_key_envelope)
             VALUES ($1, 'env-signing', $2, $3, $4, $5)",
        )
        .bind(issuer)
        .bind(org.as_str())
        .bind(env.as_str())
        .bind(signer.public_key())
        .bind(packed)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    }
    tx.commit().await.map_err(database_error)?;
    Ok(Account {
        subject,
        signer,
        issued_at,
    })
}

async fn reserve_projects(
    tx: &mut Transaction<'_, Postgres>,
    issuer: &str,
    increase: i32,
    max_projects: i64,
) -> Result<(), Error> {
    // The installation-wide lock covers only the aggregate SQL reservation.
    lock(tx, &format!("{issuer} nats accounts")).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(project_count), 0)
         FROM identity.nats_key WHERE issuer = $1 AND kind = 'env'",
    )
    .bind(issuer)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    if total + i64::from(increase) > max_projects {
        return Err(Error::project_limit(max_projects));
    }
    Ok(())
}

pub(super) async fn retire_env(
    client: &mut PgConnection,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<Option<(String, i64)>, Error> {
    let mut tx = client.begin().await.map_err(database_error)?;
    bind_actor(&mut tx).await?;
    let row = sqlx::query_as(
        "UPDATE identity.nats_key
         SET issued_at = CASE WHEN project_count = 0 THEN issued_at ELSE
             GREATEST(issued_at + 1, floor(EXTRACT(EPOCH FROM clock_timestamp()))::bigint) END,
             project_count = 0
         WHERE issuer = $1 AND kind = 'env' AND org = $2 AND env = $3
         RETURNING public_key, issued_at",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(row)
}

pub(super) async fn env_signer(
    client: &mut PgConnection,
    envelope: &Envelope,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<Option<Account>, Error> {
    let row: Option<(String, i64, Vec<u8>, String)> = sqlx::query_as(
        "SELECT a.public_key, a.issued_at, s.private_key_envelope, s.public_key
         FROM identity.nats_key a
         JOIN identity.nats_key s USING (issuer, org, env)
         WHERE a.issuer = $1 AND a.kind = 'env' AND s.kind = 'env-signing'
               AND a.org = $2 AND a.env = $3 AND a.project_count > 0",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_optional(client)
    .await
    .map_err(database_error)?;
    let Some((subject, issued_at, seed, public)) = row else {
        return Ok(None);
    };
    let signer = open(
        envelope,
        StoredKey {
            seed,
            public,
            issued_at,
        },
    )
    .await?
    .pair;
    Ok(Some(Account {
        subject,
        signer,
        issued_at,
    }))
}

async fn read(
    client: &mut PgConnection,
    issuer: &str,
    key_name: &str,
    scope: Scope<'_>,
) -> Result<Option<StoredKey>, Error> {
    let row: Option<(Vec<u8>, String, i64)> = sqlx::query_as(
        "SELECT private_key_envelope, public_key, issued_at FROM identity.nats_key
         WHERE issuer = $1 AND kind = $2
               AND org IS NOT DISTINCT FROM $3 AND env IS NOT DISTINCT FROM $4",
    )
    .bind(issuer)
    .bind(key_name)
    .bind(scope.org)
    .bind(scope.env)
    .fetch_optional(client)
    .await
    .map_err(database_error)?;
    Ok(row.map(|(seed, public, issued_at)| StoredKey {
        seed,
        public,
        issued_at,
    }))
}

async fn seal(envelope: &Envelope, pair: &KeyPair) -> Result<Vec<u8>, Error> {
    let seed = Zeroizing::new(
        pair.seed()
            .map_err(|source| Error::new("encode NATS seed", source))?,
    );
    let encrypted = envelope
        .encrypt(seed.as_bytes())
        .await
        .map_err(|source| Error::new("encrypt NATS seed", source))?;
    serde_json::to_vec(&encrypted).map_err(|source| Error::new("encode NATS seed envelope", source))
}

async fn open(envelope: &Envelope, row: StoredKey) -> Result<Key, Error> {
    let encrypted: Encrypted = serde_json::from_slice(&row.seed)
        .map_err(|source| Error::new("decode NATS seed envelope", source))?;
    let plaintext = envelope
        .decrypt(&encrypted)
        .await
        .map_err(|source| Error::new("decrypt NATS seed", source))?;
    let seed = std::str::from_utf8(plaintext.expose())
        .map_err(|source| Error::new("decode NATS seed", source))?;
    let pair = KeyPair::from_seed(seed).map_err(|source| Error::new("decode NATS seed", source))?;
    if pair.public_key() != row.public {
        return Err(Error::new(
            "verify stored NATS key",
            std::io::Error::other("NKey differs from stored public key"),
        ));
    }
    Ok(Key {
        pair,
        issued_at: row.issued_at,
    })
}
