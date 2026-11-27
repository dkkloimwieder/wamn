//! Shared OAuth tokens, with provider I/O between short SQL transactions.

use std::time::Duration;

use crate::oauth_token::{Available, Error, Fetch, Refresh, Token};
use anyhow::Context as _;
use sqlx::{Connection as _, PgConnection, Postgres, Row as _, Transaction};
use tokio::time::{Instant, sleep, timeout, timeout_at};
use uuid::Uuid;
use wamn_app::MAX_HOST_CALL_DURATION;
use wamn_envelope::encryption::{Encrypted, Envelope};

use super::{ConnectionEvent, EventOutcome, EventType, ProjectDatabase};

const LEASE_MARGIN: Duration = Duration::from_secs(10);
const LEASE_DURATION: Duration = MAX_HOST_CALL_DURATION.saturating_add(LEASE_MARGIN);
/// Time between reads when another process holds an OAuth lease.
pub const OAUTH_LEASE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A committed claim, identified by its exact expiration in PostgreSQL.
#[derive(Debug)]
pub struct OAuthLease {
    connection: Uuid,
    until: String,
    work_deadline: Instant,
}

impl OAuthLease {
    /// Claim the token row while the owning transaction holds the connection row.
    /// The caller commits immediately and releases its database connection.
    pub async fn claim(
        tx: &mut Transaction<'_, Postgres>,
        connection: Uuid,
    ) -> sqlx::Result<Option<Self>> {
        // The lease also covers the first fetch or authorization, before a token exists.
        sqlx::query(
            "INSERT INTO platform.oauth_token
                 (connection_id,ciphertext,nonce,wrapped_key,key_id,expires_at,updated_at)
             VALUES ($1,'','','','','-infinity','-infinity')
             ON CONFLICT (connection_id) DO NOTHING",
        )
        .bind(connection)
        .execute(&mut **tx)
        .await?;

        let work_deadline = Instant::now() + LEASE_DURATION - Duration::from_secs(1);
        let until: Option<String> = sqlx::query_scalar(
            "WITH claim_time AS MATERIALIZED (SELECT clock_timestamp() AS claimed_at)
             UPDATE platform.oauth_token
                SET refresh_lease_until = claimed_at + make_interval(secs => $2)
               FROM claim_time
              WHERE connection_id=$1 AND refresh_lease_until < claimed_at
              RETURNING extract(epoch FROM refresh_lease_until)::text",
        )
        .bind(connection)
        .bind(LEASE_DURATION.as_secs_f64())
        .fetch_optional(&mut **tx)
        .await?;
        Ok(until.map(|until| Self {
            connection,
            until,
            work_deadline,
        }))
    }

    /// The remote work must finish with time left to publish.
    pub fn work_deadline(&self) -> Instant {
        self.work_deadline
    }

    /// Store the token and clear the lease if the claim is still live.
    pub async fn write_token(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        encrypted: &Encrypted,
        remaining: f64,
    ) -> sqlx::Result<bool> {
        let result = sqlx::query(
            "UPDATE platform.oauth_token SET ciphertext=$3,nonce=$4,wrapped_key=$5,key_id=$6,
                    expires_at=clock_timestamp()+$7::double precision*interval '1 second',
                    updated_at=clock_timestamp(),refresh_lease_until='-infinity'
             WHERE connection_id=$1 AND extract(epoch FROM refresh_lease_until)=$2::text::numeric
               AND refresh_lease_until > clock_timestamp()",
        )
        .bind(self.connection)
        .bind(&self.until)
        .bind(&encrypted.ciphertext)
        .bind(&encrypted.nonce)
        .bind(&encrypted.wrapped_key)
        .bind(&encrypted.key_id)
        .bind(remaining)
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn clear(&self, tx: &mut Transaction<'_, Postgres>) -> sqlx::Result<bool> {
        let result = sqlx::query(
            "UPDATE platform.oauth_token SET refresh_lease_until='-infinity'
             WHERE connection_id=$1 AND extract(epoch FROM refresh_lease_until)=$2::text::numeric",
        )
        .bind(self.connection)
        .bind(&self.until)
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Record an outcome and return the lease in one bounded transaction.
    pub async fn record_failure(
        &self,
        database: &mut PgConnection,
        event: &ConnectionEvent,
        outcome: EventOutcome,
    ) -> sqlx::Result<()> {
        let mut tx = database.begin().await?;
        sqlx::raw_sql("SET LOCAL statement_timeout = '750ms'")
            .execute(&mut *tx)
            .await?;
        let current = lock_active_generation(&mut tx, self.connection).await?;
        let owned = self.clear(&mut tx).await?;
        let outcome = match event.event_type {
            EventType::OAuthCallback => Some(outcome),
            _ if owned && current == Some(event.generation) => Some(outcome),
            _ if owned => Some(EventOutcome::Superseded),
            _ => None,
        };
        if let Some(outcome) = outcome {
            event.append(&mut tx, outcome).await?;
        }
        tx.commit().await
    }
}

enum Preparation {
    Waiting(String),
    Unauthorized,
    Cooling,
    Ready {
        stored: Stored,
        lease: Option<OAuthLease>,
        refreshed: bool,
    },
}

pub(super) async fn get_token(
    database: &ProjectDatabase<'_>,
    request: &Refresh<'_>,
    envelope: &Envelope,
    fetch: Fetch<'_>,
) -> anyhow::Result<Result<Available, Error>> {
    let mut waited_for = None;
    let mut force_claim = false;
    loop {
        anyhow::ensure!(
            Instant::now() < request.deadline,
            "OAuth refresh wait timed out"
        );
        let prepared = timeout_at(
            request.deadline,
            prepare_refresh(database, request, waited_for.as_deref(), force_claim),
        )
        .await
        .context("OAuth refresh wait timed out")??;
        let (stored, lease, refreshed) = match prepared {
            Preparation::Waiting(revision) => {
                waited_for.get_or_insert(revision);
                sleep(
                    OAUTH_LEASE_POLL_INTERVAL
                        .min(request.deadline.saturating_duration_since(Instant::now())),
                )
                .await;
                continue;
            }
            Preparation::Unauthorized => return Ok(Err(Error::authorization_required(request))),
            Preparation::Cooling => anyhow::bail!("OAuth refresh retry window"),
            Preparation::Ready {
                stored,
                lease,
                refreshed,
            } => (stored, lease, refreshed),
        };

        let deadline = lease.as_ref().map_or(request.deadline, |lease| {
            request.deadline.min(lease.work_deadline())
        });
        let reusable = stored.reusable(refreshed);
        let old = timeout_at(deadline, async {
            match stored.encrypted {
                Some(encrypted) => decrypt(envelope, &encrypted).await.map(Some),
                None => Ok(None),
            }
        })
        .await
        .context("OAuth token decryption timed out")
        .and_then(|token| token);
        let old = match old {
            Ok(Some(mut token)) if reusable => {
                if let Some(access) = token
                    .access_token
                    .take_if(|access| request.rejected != Some(access.as_str()))
                {
                    let current = database
                        .oauth_transaction(async |tx| {
                            let current = is_current(tx, request).await?;
                            match &lease {
                                Some(lease) => Ok(lease.clear(tx).await? && current),
                                None => Ok(current),
                            }
                        })
                        .await?;
                    anyhow::ensure!(current, "OAuth token superseded");
                    return Ok(Ok(Available { token: access }));
                }
                Ok(Some(token))
            }
            old => old,
        };
        if let Some(lease) = lease {
            return refresh(database, request, envelope, &lease, old, fetch)
                .await
                .map(Ok);
        }
        // A waiter only uses the previous owner's result. A failed read of a
        // regular token must acquire the lease before it can record a failure.
        anyhow::ensure!(!refreshed, "OAuth refresh did not produce a usable token");
        force_claim = true;
    }
}

async fn prepare_refresh(
    database: &ProjectDatabase<'_>,
    request: &Refresh<'_>,
    waited_for: Option<&str>,
    force_claim: bool,
) -> anyhow::Result<Preparation> {
    database
        .oauth_transaction(async |tx| {
            anyhow::ensure!(
                is_current(tx, request).await?,
                "OAuth connection generation changed"
            );
            let stored = read_token(tx, request).await?;
            let refreshed = waited_for.is_some_and(|revision| revision != stored.revision);
            let prepared = if stored.leased {
                Preparation::Waiting(stored.revision)
            } else if stored.encrypted.is_none() && request.authorization_code {
                Preparation::Unauthorized
            } else if stored.cooling {
                Preparation::Cooling
            } else if refreshed
                || (!force_claim && request.rejected.is_none() && stored.reusable(false))
            {
                Preparation::Ready {
                    stored,
                    lease: None,
                    refreshed,
                }
            } else if let Some(lease) = OAuthLease::claim(tx, request.connection).await? {
                Preparation::Ready {
                    stored,
                    lease: Some(lease),
                    refreshed: false,
                }
            } else {
                Preparation::Waiting(stored.revision)
            };
            Ok(prepared)
        })
        .await
}

async fn refresh(
    database: &ProjectDatabase<'_>,
    request: &Refresh<'_>,
    envelope: &Envelope,
    lease: &OAuthLease,
    old: anyhow::Result<Option<Token>>,
    fetch: Fetch<'_>,
) -> anyhow::Result<Available> {
    let event = refresh_event(request);
    let result: anyhow::Result<Option<Available>> = async {
        let (encrypted, access, seconds, fetched_at) =
            timeout_at(request.deadline.min(lease.work_deadline()), async {
                fetch_and_encrypt(envelope, old?, fetch).await
            })
            .await
            .context("OAuth refresh timed out")??;
        let published = database
            .oauth_transaction(async |tx| {
                wamn_postgres::bind_transaction_identity(
                    tx,
                    &request.principal,
                    "",
                    "",
                    None,
                    None,
                )
                .await?;
                if !is_current(tx, request).await? {
                    lease.clear(tx).await?;
                    event.append(tx, EventOutcome::Superseded).await?;
                    return Ok(false);
                }
                let remaining = (seconds - fetched_at.elapsed().as_secs_f64()).max(0.0);
                if !lease.write_token(tx, &encrypted, remaining).await? {
                    return Ok(false);
                }
                event.append(tx, EventOutcome::Succeeded).await?;
                Ok(true)
            })
            .await?;
        Ok(published.then_some(Available { token: access }))
    }
    .await;
    if result.is_err() {
        let record = async {
            let mut guard = database.oauth_connection().await?;
            lease
                .record_failure(guard.connection(), &event, EventOutcome::Failed)
                .await?;
            guard.repool();
            Ok::<_, anyhow::Error>(())
        };
        if !timeout(Duration::from_secs(1), record)
            .await
            .is_ok_and(|result| result.is_ok())
        {
            tracing::warn!("OAuth refresh failure could not be recorded");
        }
    }
    result?.context("OAuth refresh superseded")
}

fn refresh_event(request: &Refresh<'_>) -> ConnectionEvent {
    ConnectionEvent {
        connection: request.connection,
        generation: request.generation,
        event_type: EventType::TokenRefresh,
        principal: Some(request.principal),
    }
}

async fn is_current(
    tx: &mut Transaction<'_, Postgres>,
    request: &Refresh<'_>,
) -> sqlx::Result<bool> {
    Ok(lock_active_generation(tx, request.connection).await? == Some(request.generation))
}

/// Read the active generation and keep it fixed until the SQL transaction ends.
pub async fn lock_active_generation(
    tx: &mut Transaction<'_, Postgres>,
    connection: Uuid,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT active_generation FROM platform.connection
         WHERE id=$1 AND active FOR NO KEY UPDATE",
    )
    .bind(connection)
    .fetch_optional(&mut **tx)
    .await
}

struct Stored {
    encrypted: Option<Encrypted>,
    seconds_left: f64,
    healthy: bool,
    cooling: bool,
    leased: bool,
    revision: String,
}

impl Stored {
    fn reusable(&self, refreshed: bool) -> bool {
        let margin = if refreshed { 0.0 } else { 60.0 };
        self.healthy && self.seconds_left > margin
    }
}

async fn read_token(
    tx: &mut Transaction<'_, Postgres>,
    request: &Refresh<'_>,
) -> sqlx::Result<Stored> {
    sqlx::query(
        "SELECT CASE WHEN t.refresh_lease_until < statement_timestamp() AND t.updated_at <> '-infinity'
                     THEN t.ciphertext END AS ciphertext,
                t.nonce,t.wrapped_key,t.key_id,
                coalesce(t.refresh_lease_until >= statement_timestamp(), false) AS leased,
                extract(epoch FROM coalesce(t.updated_at, '-infinity'))::text AS revision,
                coalesce(extract(epoch FROM t.expires_at)::double precision
                         - extract(epoch FROM clock_timestamp())::double precision, 0) AS seconds_left,
                NOT coalesce(r.failed, false) AS healthy,
                coalesce(r.failed AND r.occurred_at > clock_timestamp() - interval '30 seconds', false) AS cooling
         FROM platform.connection c
         LEFT JOIN platform.oauth_token t ON t.connection_id=c.id
         LEFT JOIN LATERAL (
             SELECT NOT o.success AS failed, e.occurred_at
             FROM platform.connection_event e
             JOIN platform.connection_event_outcome o ON o.code=e.outcome
             WHERE e.connection_id=c.id AND e.generation=c.active_generation
               AND e.event_type='token_refresh'
             ORDER BY e.occurred_at DESC, e.id DESC LIMIT 1
         ) r ON t.updated_at IS NULL OR r.occurred_at >= t.updated_at
         WHERE c.id=$1",
    )
    .bind(request.connection)
    .try_map(|row: sqlx::postgres::PgRow| {
        let encrypted = match row.try_get::<Option<Vec<u8>>, _>("ciphertext")? {
            Some(ciphertext) => Some(Encrypted {
                ciphertext,
                nonce: row.try_get("nonce")?,
                wrapped_key: row.try_get("wrapped_key")?,
                key_id: row.try_get("key_id")?,
            }),
            None => None,
        };
        Ok(Stored {
            encrypted,
            seconds_left: row.try_get("seconds_left")?,
            healthy: row.try_get("healthy")?,
            cooling: row.try_get("cooling")?,
            leased: row.try_get("leased")?,
            revision: row.try_get("revision")?,
        })
    })
    .fetch_one(&mut **tx)
    .await
}

async fn decrypt(envelope: &Envelope, encrypted: &Encrypted) -> anyhow::Result<Token> {
    let plaintext = envelope.decrypt(encrypted).await?;
    serde_json::from_slice(plaintext.expose()).context("decode OAuth token")
}

async fn fetch_and_encrypt(
    envelope: &Envelope,
    old: Option<Token>,
    fetch: Fetch<'_>,
) -> anyhow::Result<(Encrypted, String, f64, Instant)> {
    let previous_refresh = old.as_ref().and_then(|token| token.refresh_token.clone());
    let fetched_at = Instant::now();
    let mut fetched = fetch(old)
        .await
        .map_err(|_| anyhow::anyhow!("OAuth provider unavailable"))?;
    let seconds = std::time::Duration::from_secs(fetched.expires_in.unwrap_or(0)).as_secs_f64();
    if fetched.token.refresh_token.is_none() {
        fetched.token.refresh_token = previous_refresh;
    }
    let encrypted = envelope
        .encrypt(&serde_json::to_vec(&fetched.token)?)
        .await
        .context("OAuth encryption unavailable")?;
    let access = fetched
        .token
        .access_token
        .context("OAuth access token missing")?;
    Ok((encrypted, access, seconds, fetched_at))
}
