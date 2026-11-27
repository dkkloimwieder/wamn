//! Envelope-encrypted NKey seeds and the environment account's claim revision.
//! The account's session lock spans SQL changes and resolver publication.

use std::collections::BTreeMap;

use nkeys::{KeyPair, KeyPairType};
use sqlx::{Connection as _, PgConnection, Postgres, Transaction};
use wamn_envelope::encryption::{Encrypted, Envelope};
use wamn_registry::{EnvName, OrgId, ProjectId};
use zeroize::Zeroizing;

use crate::nats::{Account, AccountRevision, EnvLimits, Error, Key, Limits, env_limits};

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
    claim_limits: sqlx::types::Json<Limits>,
    reserved_disk_bytes: i64,
    published: bool,
}

enum PreparedEnv {
    Existing {
        account: StoredEnv,
        resolver_revision: Option<AccountRevision>,
    },
    New {
        subject: String,
        packed: Vec<u8>,
    },
}

pub(super) fn database_error(source: sqlx::Error) -> Error {
    Error::new("access NATS key storage", source)
}

async fn lock(tx: &mut Transaction<'_, Postgres>, scope: &str) -> Result<(), Error> {
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
    client: &mut PgConnection,
    envelope: &Envelope,
    issuer: &str,
    key_name: &str,
    key_type: KeyPairType,
) -> Result<Key, Error> {
    let scope = Scope::default();
    if let Some(row) = read(client, issuer, key_name, scope).await? {
        return open(envelope, row).await;
    }
    let pair = KeyPair::new(key_type);
    let packed = seal(envelope, &pair).await?;
    let mut tx = client.begin().await.map_err(database_error)?;
    bind_actor(&mut tx).await?;
    let issued_at = sqlx::query_scalar(
        "INSERT INTO identity.nats_key
            (issuer, kind, public_key, private_key_envelope)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (issuer, kind, org, env) DO NOTHING RETURNING issued_at",
    )
    .bind(issuer)
    .bind(key_name)
    .bind(pair.public_key())
    .bind(packed)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    if let Some(issued_at) = issued_at {
        return Ok(Key { pair, issued_at });
    }
    let row = read(client, issuer, key_name, scope)
        .await?
        .ok_or_else(|| database_error(sqlx::Error::RowNotFound))?;
    open(envelope, row).await
}

const ENV_LIMITS: &str = "nats_connections AS connections, nats_consumers AS consumers,
    nats_subscriptions AS subscriptions, nats_payload_bytes AS payload_bytes,
    nats_streams_per_project AS streams_per_project, nats_disk_bytes_per_project AS disk_bytes_per_project";

pub(super) async fn accounts_to_refresh(
    client: &mut PgConnection,
    issuer: &str,
) -> Result<Vec<(String, String)>, Error> {
    #[derive(sqlx::FromRow)]
    struct Candidate {
        org: String,
        env: String,
        project_count: i32,
        claim_limits: sqlx::types::Json<Limits>,
        published: bool,
        active: bool,
        needs_revocation: bool,
        #[sqlx(flatten)]
        desired: EnvLimits,
    }

    let rows = sqlx::query_as::<_, Candidate>(sqlx::AssertSqlSafe(format!(
        "SELECT e.org, e.env, k.project_count, k.claim_limits, k.published, e.active, {ENV_LIMITS},
                EXISTS (
                    SELECT FROM identity.nats_user u JOIN registry.project_env d USING (org, env, project)
                    JOIN registry.project p ON p.org = u.org AND p.id = u.project
                    WHERE u.issuer = k.issuer AND u.org = k.org AND u.env = k.env
                      AND u.revoked_at IS NULL
                      AND (NOT d.active OR NOT p.active OR d.instance_suffix <> u.instance_suffix)
                ) AS needs_revocation
         FROM registry.env e JOIN identity.nats_key k USING (org, env)
         WHERE k.issuer = $1 AND k.kind = 'env'"
    )))
    .bind(issuer)
    .fetch_all(client)
    .await
    .map_err(database_error)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            !row.published
                || row.needs_revocation
                || (row.active
                    && !env_limits(&row.desired, row.project_count)
                        .is_ok_and(|limits| row.claim_limits.0 == limits))
        })
        .map(|row| (row.org, row.env))
        .collect())
}

async fn read_env(
    client: &mut PgConnection,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<Option<StoredEnv>, Error> {
    sqlx::query_as(
        "SELECT public_key, issued_at, project_count, claim_limits, reserved_disk_bytes, published
         FROM identity.nats_key
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
    project_count: Option<i32>,
    file_store_bytes: i64,
    observe: impl AsyncFnOnce(&str, Limits, Option<Limits>) -> Result<Option<AccountRevision>, Error>,
) -> Result<Option<(Account, Limits, BTreeMap<String, i64>)>, Error> {
    let scope = Scope {
        org: Some(org.as_str()),
        env: Some(env.as_str()),
    };
    let mut tx = client.begin().await.map_err(database_error)?;
    let existing = read_env(&mut tx, issuer, org, env).await?;
    let Some(count) = project_count.or_else(|| existing.as_ref().map(|key| key.project_count))
    else {
        tx.commit().await.map_err(database_error)?;
        return Ok(None);
    };
    if count == 0 && existing.is_none() {
        tx.commit().await.map_err(database_error)?;
        return Ok(None);
    }
    let desired: Option<EnvLimits> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ENV_LIMITS} FROM registry.env e JOIN registry.org o ON o.id = e.org
         WHERE e.org = $1 AND e.env = $2 AND e.active AND o.active"
    )))
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    let limits = match (&existing, desired) {
        (Some(account), _) if project_count.is_none() && !account.published => {
            account.claim_limits.0
        }
        (_, Some(desired)) => env_limits(&desired, count)?,
        (_, None) if project_count.is_some() => return Err(Error::scope_refused()),
        _ => {
            tx.commit().await.map_err(database_error)?;
            return Ok(None);
        }
    };
    if project_count.is_none()
        && existing
            .as_ref()
            .is_some_and(|account| account.published && account.claim_limits.0 == limits)
    {
        tx.commit().await.map_err(database_error)?;
        return Ok(None);
    }
    let revocations = revoked_users(&mut tx, issuer, org, env).await?;
    let stored_signer = read(&mut tx, issuer, "env-signing", scope).await?;
    tx.commit().await.map_err(database_error)?;

    let prepared = if let Some(account) = existing {
        let committed = account.claim_limits.0;
        let previous = (account.project_count > 0
            && (account.published || account.reserved_disk_bytes > committed.disk_storage))
            .then_some(Limits {
                disk_storage: account.reserved_disk_bytes,
                ..committed
            });
        let resolver_revision = observe(&account.public_key, limits, previous).await?;
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
    let reserved = expected.map_or(0, |account| account.reserved_disk_bytes);
    let observed_disk = match &prepared {
        PreparedEnv::Existing {
            resolver_revision: Some(revision),
            ..
        } => revision.limits().disk_storage,
        _ => 0,
    };
    // An unacknowledged decrease cannot free bytes for another environment.
    let reservation = limits.disk_storage.max(reserved).max(observed_disk);
    if reservation > reserved {
        reserve_storage(&mut tx, reservation - reserved, file_store_bytes).await?;
    } else {
        bind_actor(&mut tx).await?;
    }

    let (subject, issued_at) = match prepared {
        PreparedEnv::Existing {
            account,
            resolver_revision,
        } if reservation == reserved
            && account.project_count == count
            && account.claim_limits.0 == limits
            && resolver_revision.as_ref().is_none_or(|revision| {
                revision.iat < account.issued_at
                    || (revision.iat == account.issued_at
                        && revision.limits() == limits
                        && revision.revocations() == &revocations)
            }) =>
        {
            if account.published
                && resolver_revision
                    .as_ref()
                    .is_none_or(|revision| revision.iat < account.issued_at)
            {
                sqlx::query(
                    "UPDATE identity.nats_key SET published = false
                     WHERE issuer = $1 AND kind = 'env' AND org = $2 AND env = $3",
                )
                .bind(issuer)
                .bind(org.as_str())
                .bind(env.as_str())
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
            }
            (account.public_key, account.issued_at)
        }
        PreparedEnv::Existing {
            account,
            resolver_revision,
        } => {
            let issued_at = sqlx::query_scalar(
                "UPDATE identity.nats_key
                 SET issued_at = GREATEST(issued_at + 1, $4::bigint + 1, floor(EXTRACT(EPOCH FROM clock_timestamp()))::bigint),
                     project_count = $5, claim_limits = $6, reserved_disk_bytes = $7, published = false
                 WHERE issuer = $1 AND kind = 'env' AND org = $2 AND env = $3
                 RETURNING issued_at",
            )
            .bind(issuer)
            .bind(org.as_str())
            .bind(env.as_str())
            .bind(resolver_revision.map_or(account.issued_at, |revision| revision.iat))
            .bind(count)
            .bind(sqlx::types::Json(limits))
            .bind(reservation)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
            (account.public_key, issued_at)
        }
        PreparedEnv::New { subject, packed } => {
            let issued_at = sqlx::query_scalar(
                "INSERT INTO identity.nats_key
                    (issuer, kind, org, env, public_key, private_key_envelope, project_count,
                     claim_limits, reserved_disk_bytes, published)
                 VALUES ($1, 'env', $2, $3, $4, $5, $6, $7, $8, false) RETURNING issued_at",
            )
            .bind(issuer)
            .bind(org.as_str())
            .bind(env.as_str())
            .bind(&subject)
            .bind(packed)
            .bind(count)
            .bind(sqlx::types::Json(limits))
            .bind(reservation)
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
    Ok(Some((
        Account {
            subject,
            signer,
            issued_at,
        },
        limits,
        revocations,
    )))
}

async fn reserve_storage(
    tx: &mut Transaction<'_, Postgres>,
    increase: i64,
    file_store_bytes: i64,
) -> Result<(), Error> {
    // All environments share the file store. This lock covers only SQL writes.
    lock(tx, "wamn NATS file store").await?;
    let available: bool = sqlx::query_scalar(
        "SELECT COALESCE(sum(reserved_disk_bytes), 0) + $1::bigint <= $2::bigint
         FROM identity.nats_key WHERE kind = 'env'",
    )
    .bind(increase)
    .bind(file_store_bytes)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    if !available {
        return Err(Error::limit_refused("NATS file store capacity exceeded"));
    }
    Ok(())
}

pub(super) async fn acknowledge(
    client: &mut PgConnection,
    issuer: &str,
    (org, env): (&OrgId, &EnvName),
    disk_bytes: i64,
) -> Result<(), Error> {
    let mut tx = client.begin().await.map_err(database_error)?;
    bind_actor(&mut tx).await?;
    sqlx::query(
        "UPDATE identity.nats_key SET published = true, reserved_disk_bytes = $4
         WHERE issuer = $1 AND kind = 'env' AND org = $2 AND env = $3
           AND (NOT published OR reserved_disk_bytes <> $4)",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .bind(disk_bytes)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)
}

/// Stage revocations and mark the corresponding account revision for publication.
pub(super) async fn stage_revocations(
    client: &mut PgConnection,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<(), Error> {
    let mut tx = client.begin().await.map_err(database_error)?;
    let updated = sqlx::query(
        "UPDATE identity.nats_user u SET revoked_at = u.issued_at
         FROM registry.project_env d JOIN registry.project p ON p.org = d.org AND p.id = d.project
         WHERE (u.org, u.env, u.project) = (d.org, d.env, d.project)
           AND u.issuer = $1 AND u.org = $2 AND u.env = $3
           AND u.revoked_at IS NULL
           AND (NOT p.active OR NOT d.active OR d.instance_suffix <> u.instance_suffix)",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() > 0 {
        bind_actor(&mut tx).await?;
        sqlx::query(
            "UPDATE identity.nats_key SET published = false,
             issued_at = GREATEST(issued_at + 1, floor(EXTRACT(EPOCH FROM clock_timestamp()))::bigint)
             WHERE issuer = $1 AND org = $2 AND env = $3 AND kind = 'env'",
        )
        .bind(issuer)
        .bind(org.as_str())
        .bind(env.as_str())
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    }
    tx.commit().await.map_err(database_error)
}

async fn revoked_users(
    client: &mut PgConnection,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<BTreeMap<String, i64>, Error> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT public_key, revoked_at FROM identity.nats_user
         WHERE issuer = $1 AND org = $2 AND env = $3 AND revoked_at IS NOT NULL",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_all(client)
    .await
    .map_err(database_error)?;
    Ok(rows.into_iter().collect())
}

pub(super) async fn record_user(
    client: &mut PgConnection,
    issuer: &str,
    (org, env, project): (&OrgId, &EnvName, &ProjectId),
    public_key: &str,
    issued_at: i64,
) -> Result<bool, Error> {
    let updated = sqlx::query(
        "INSERT INTO identity.nats_user (issuer, org, env, project, public_key, instance_suffix, issued_at)
         SELECT $1, $2, $3, $4, $5, d.instance_suffix, $6
         FROM registry.project_env d JOIN registry.project p ON p.org = d.org AND p.id = d.project
         JOIN registry.env e ON e.org = d.org AND e.env = d.env JOIN registry.org o ON o.id = d.org
         WHERE (d.org, d.env, d.project) = ($2, $3, $4) AND d.active AND p.active AND e.active AND o.active
         ON CONFLICT (issuer, org, env, public_key) DO UPDATE
           SET issued_at = GREATEST(identity.nats_user.issued_at, EXCLUDED.issued_at)
           WHERE identity.nats_user.project = EXCLUDED.project AND identity.nats_user.revoked_at IS NULL
                 AND identity.nats_user.instance_suffix = EXCLUDED.instance_suffix",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .bind(project.as_str())
    .bind(public_key)
    .bind(issued_at)
    .execute(client)
    .await
    .map_err(database_error)?;
    Ok(updated.rows_affected() != 0)
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
