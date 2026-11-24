//! Envelope-encrypted NKey seeds and the environment account's claim revision.

use nkeys::{KeyPair, KeyPairType};
use sqlx::{Connection as _, PgConnection, Postgres, Transaction};
use wamn_envelope::encryption::{Encrypted, Envelope};
use wamn_registry::{EnvName, OrgId};
use zeroize::Zeroizing;

use wamn_authentication::nats::{Account, Error, Key};

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

pub(super) fn database_error(source: sqlx::Error) -> Error {
    Error::new("access NATS key storage", source)
}

pub(super) async fn lock(tx: &mut Transaction<'_, Postgres>, scope: &str) -> Result<(), Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 1376598821))")
        .bind(scope)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    super::session_keys::bind_identity_service(tx)
        .await
        .map_err(|source| Error::new("bind NATS key storage authority", source))
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
        "INSERT INTO identity.nats_keys
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

pub(super) async fn ensure_environment(
    client: &mut PgConnection,
    envelope: &Envelope,
    issuer: &str,
    (org, env): (&OrgId, &EnvName),
    project_count: i32,
    max_projects: i64,
    limits_match: impl AsyncFnOnce(&str, i64) -> Result<bool, Error>,
) -> Result<Account, Error> {
    let mut tx = client.begin().await.map_err(database_error)?;
    lock(&mut tx, &format!("{issuer} nats accounts")).await?;
    let (total, current): (i64, i32) = sqlx::query_as(
        "SELECT COALESCE(sum(project_count), 0),
                COALESCE(max(project_count) FILTER (WHERE org = $2 AND env = $3), 0)
         FROM identity.nats_keys WHERE issuer = $1 AND kind = 'environment'",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    if project_count > current
        && total - i64::from(current) + i64::from(project_count) > max_projects
    {
        return Err(Error::project_limit(max_projects));
    }

    let existing: Option<(String, i64)> = sqlx::query_as(
        "SELECT public_key, issued_at FROM identity.nats_keys
         WHERE issuer = $1 AND kind = 'environment' AND org = $2 AND env = $3",
    )
    .bind(issuer)
    .bind(org.as_str())
    .bind(env.as_str())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;

    let (subject, issued_at) = if let Some((subject, issued_at)) = existing {
        let matches = limits_match(&subject, issued_at).await?;
        let issued_at = if matches && project_count == current {
            issued_at
        } else {
            // The full resolver breaks same-second ties on JWT bytes.
            sqlx::query_scalar(
                "UPDATE identity.nats_keys
                 SET issued_at = GREATEST(issued_at + 1, floor(EXTRACT(EPOCH FROM clock_timestamp()))::bigint),
                     project_count = $4
                 WHERE issuer = $1 AND kind = 'environment' AND org = $2 AND env = $3
                 RETURNING issued_at",
            )
            .bind(issuer)
            .bind(org.as_str())
            .bind(env.as_str())
            .bind(project_count)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?
        };
        (subject, issued_at)
    } else {
        let pair = KeyPair::new_account();
        let packed = seal(envelope, &pair).await?;
        let subject = pair.public_key();
        let issued_at = sqlx::query_scalar(
            "INSERT INTO identity.nats_keys
                (issuer, kind, org, env, public_key, private_key_envelope, project_count)
             VALUES ($1, 'environment', $2, $3, $4, $5, $6) RETURNING issued_at",
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
    };
    let scope = Scope {
        org: Some(org.as_str()),
        env: Some(env.as_str()),
    };
    let signer = ensure_key(
        &mut tx,
        envelope,
        issuer,
        "environment-signing",
        scope,
        KeyPairType::Account,
    )
    .await?
    .pair;
    tx.commit().await.map_err(database_error)?;
    Ok(Account {
        subject,
        signer,
        issued_at,
    })
}

pub(super) async fn retire_environment(
    client: &mut PgConnection,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<Option<(String, i64)>, Error> {
    let mut tx = client.begin().await.map_err(database_error)?;
    lock(&mut tx, &format!("{issuer} nats accounts")).await?;
    let row = sqlx::query_as(
        "UPDATE identity.nats_keys
         SET issued_at = CASE WHEN project_count = 0 THEN issued_at ELSE
             GREATEST(issued_at + 1, floor(EXTRACT(EPOCH FROM clock_timestamp()))::bigint) END,
             project_count = 0
         WHERE issuer = $1 AND kind = 'environment' AND org = $2 AND env = $3
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

pub(super) async fn environment_signer(
    client: &mut PgConnection,
    envelope: &Envelope,
    issuer: &str,
    org: &OrgId,
    env: &EnvName,
) -> Result<Option<Account>, Error> {
    let row: Option<(String, i64, Vec<u8>, String)> = sqlx::query_as(
        "SELECT a.public_key, a.issued_at, s.private_key_envelope, s.public_key
         FROM identity.nats_keys a
         JOIN identity.nats_keys s USING (issuer, org, env)
         WHERE a.issuer = $1 AND a.kind = 'environment' AND s.kind = 'environment-signing'
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
        "SELECT private_key_envelope, public_key, issued_at FROM identity.nats_keys
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
