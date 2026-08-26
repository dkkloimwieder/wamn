//! Private native effect-ledger statements.
//!
//! This module deliberately exports no SQL or general writer interface. The
//! non-default native feature keeps private statements beside their opaque
//! connection-backed adapter.
//!
//! The effect-attempt APIs remain unmounted in production; their callers are
//! live gates for the later activation owner.

use std::time::SystemTime;

use crate::effect_writer_credential::{
    EffectWriterCredentialScope, parse_effect_writer_credential, validate_effect_writer_credential,
};
use crate::queue::serialize_effect_intent_sql;
use deadpool_postgres::{Manager, Pool};
use tokio_postgres::NoTls;
use wamn_pg_core::Sql;

/// Host-held project-environment identity used to bind the private credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectWriterScope<'a> {
    /// Host-injected tenant claim used for every private writer transaction.
    pub tenant_id: &'a str,
    pub org: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    pub database: &'a str,
    pub schema: &'a str,
}

/// Caller-owned immutable facts for one effect attempt.
///
/// `current_plan_hash` originates with the GUEST, and nothing in this crate binds
/// it to the run — the statements here gate only on the run being leased and
/// running. That binding is the host's, and it must stay strictly UPSTREAM of
/// [`EffectWriterClient::begin_attempt`]: owner ruling `wamn-0h0g.15.66` restored
/// it as a pre-check so no attempt row can ever exist carrying a plan hash outside
/// the run's release closure, which is what keeps the ledger audit-clean. An
/// attempt inserted before that check would defeat it, because the row is
/// immutable and cannot be withdrawn.
///
/// The trusted HTTP effect already does this (`authorize_plan_closure` in
/// `crates/platform/runtime/src/plugins/connection_http.rs`). Whoever activates a
/// production caller for this writer (`wamn-0h0g.5.4`) inherits the same
/// obligation; there is no caller today.
#[derive(Debug, Clone, Copy)]
pub struct BeginEffectAttempt<'a> {
    pub run_id: &'a str,
    pub root_plan_hash: &'a str,
    pub current_plan_hash: &'a str,
    pub frame_id: i64,
    pub parent_frame_id: Option<i64>,
    pub call_site_id: Option<&'a str>,
    pub local_node_id: &'a str,
    pub source_artifact_hash: &'a str,
    pub requirement_name: &'a str,
    pub occurrence: i32,
    pub seq: i32,
    pub generation_fact_kind: &'a str,
    pub connection_name: Option<&'a str>,
    pub connection_generation: Option<&'a str>,
    pub credential_generation: Option<&'a str>,
    pub verified_author_principal: Option<&'a str>,
    pub verified_publisher_principal: Option<&'a str>,
    pub attempt_deadline_at: &'a str,
    pub attempt_input_ref: &'a str,
}

/// Attempt identity accepted by the dispatch and outcome builders.
#[derive(Debug, Clone, Copy)]
pub struct EffectAttemptId<'a> {
    pub attempt_id: &'a str,
}

/// Caller-owned immutable outcome facts.
#[derive(Debug, Clone, Copy)]
pub struct RecordEffectOutcome<'a> {
    pub attempt: EffectAttemptId<'a>,
    pub outcome_status: &'a str,
}

/// Stable internal failure categories for the private native adapter boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectWriterErrorKind {
    Credential,
    DivergentRetry,
    MissingAttempt,
    MissingDispatch,
    RunNotRunnable,
    Storage,
}

/// Contextual private writer failure; no database type escapes this crate.
#[derive(Debug)]
pub struct EffectWriterError {
    kind: EffectWriterErrorKind,
    operation: &'static str,
    attempt_id: Option<String>,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl EffectWriterError {
    /// Stable private-adapter failure category.
    pub fn kind(&self) -> EffectWriterErrorKind {
        self.kind
    }

    fn new(kind: EffectWriterErrorKind, operation: &'static str) -> Self {
        Self {
            kind,
            operation,
            attempt_id: None,
            source: None,
        }
    }

    fn with_attempt(mut self, attempt_id: impl Into<String>) -> Self {
        self.attempt_id = Some(attempt_id.into());
        self
    }

    fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
}

impl std::fmt::Display for EffectWriterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "effect writer {} failed: {:?}",
            self.operation, self.kind
        )?;
        if let Some(attempt_id) = &self.attempt_id {
            write!(formatter, " (attempt {attempt_id})")?;
        }
        Ok(())
    }
}

impl std::error::Error for EffectWriterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// Opaque connection-backed writer for the three typed ledger operations.
///
/// The constructor accepts only the strict fixed-Secret document plus the
/// host-held expected scope. It exposes neither a URL nor its private pool.
#[derive(Clone)]
pub struct EffectWriterClient {
    pool: Pool,
    tenant_id: String,
    schema: String,
}

impl std::fmt::Debug for EffectWriterClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EffectWriterClient")
            .finish_non_exhaustive()
    }
}

impl EffectWriterClient {
    /// Validate the exact credential document and construct its private pool.
    pub async fn from_secret_document(
        document: &[u8],
        expected: EffectWriterScope<'_>,
        now: SystemTime,
    ) -> Result<Self, EffectWriterError> {
        let credential = parse_effect_writer_credential(document).map_err(|source| {
            EffectWriterError::new(EffectWriterErrorKind::Credential, "parse credential")
                .with_source(source)
        })?;
        let expected_role = credential.role().to_string();
        let expected_database = expected.database.to_string();
        let tenant_id = expected.tenant_id.to_string();
        if !valid_schema(expected.schema) {
            return Err(EffectWriterError::new(
                EffectWriterErrorKind::Credential,
                "validate host schema identity",
            ));
        }
        let schema = expected.schema.to_string();
        let expected = EffectWriterCredentialScope {
            tenant: expected.tenant_id.to_string(),
            org: expected.org.to_string(),
            project: expected.project.to_string(),
            environment: expected.environment.to_string(),
            database: expected.database.to_string(),
        };
        validate_effect_writer_credential(&credential, &expected, now).map_err(|source| {
            EffectWriterError::new(EffectWriterErrorKind::Credential, "validate credential")
                .with_source(source)
        })?;
        let config: tokio_postgres::Config = credential.url().parse().map_err(|source| {
            EffectWriterError::new(
                EffectWriterErrorKind::Credential,
                "parse database authority",
            )
            .with_source(source)
        })?;
        let manager = Manager::new(config, NoTls);
        let pool = Pool::builder(manager)
            .max_size(4)
            .build()
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "construct private pool")
                    .with_source(source)
            })?;
        let connection = pool.get().await.map_err(|source| {
            EffectWriterError::new(
                EffectWriterErrorKind::Credential,
                "authenticate private connection",
            )
            .with_source(source)
        })?;
        let authority = connection
            .query_one("SELECT current_user::text, current_database()::text", &[])
            .await
            .map_err(|source| {
                EffectWriterError::new(
                    EffectWriterErrorKind::Credential,
                    "verify private connection authority",
                )
                .with_source(source)
            })?;
        let actual_role: String = authority.get(0);
        let actual_database: String = authority.get(1);
        if actual_role != expected_role || actual_database != expected_database {
            return Err(EffectWriterError::new(
                EffectWriterErrorKind::Credential,
                "private connection authority mismatch",
            ));
        }
        drop(connection);
        Ok(Self {
            pool,
            tenant_id,
            schema,
        })
    }

    /// Insert an attempt or verify every caller fact after a concurrent winner.
    pub async fn begin_attempt(
        &self,
        attempt: BeginEffectAttempt<'_>,
    ) -> Result<EffectAttempt, EffectWriterError> {
        let mut connection = self.pool.get().await.map_err(|source| {
            EffectWriterError::new(
                EffectWriterErrorKind::Storage,
                "checkout private connection",
            )
            .with_source(source)
        })?;
        let transaction = connection
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "begin ledger transaction")
                    .with_source(source)
            })?;
        transaction
            .query_one(
                bind_writer_authority().text(),
                &[&self.tenant_id, &self.schema],
            )
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "bind ledger authority")
                    .with_source(source)
            })?;
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 20] = [
            &self.tenant_id,
            &attempt.run_id,
            &attempt.root_plan_hash,
            &attempt.current_plan_hash,
            &attempt.frame_id,
            &attempt.parent_frame_id,
            &attempt.call_site_id,
            &attempt.local_node_id,
            &attempt.source_artifact_hash,
            &attempt.requirement_name,
            &attempt.occurrence,
            &attempt.seq,
            &attempt.generation_fact_kind,
            &attempt.connection_name,
            &attempt.connection_generation,
            &attempt.credential_generation,
            &attempt.verified_author_principal,
            &attempt.verified_publisher_principal,
            &attempt.attempt_deadline_at,
            &attempt.attempt_input_ref,
        ];
        transaction
            .query_one(serialize_effect_intent_sql().as_str(), &[&attempt.run_id])
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "fence effect attempt")
                    .with_source(source)
            })?;
        let existing = transaction
            .query_opt(verify_effect_attempt().text(), &params)
            .await
            .map_err(|source| {
                EffectWriterError::new(
                    EffectWriterErrorKind::Storage,
                    "verify existing effect attempt",
                )
                .with_source(source)
            })?;
        let row = match existing {
            Some(row) => row,
            None => {
                let coordinate_params: [&(dyn tokio_postgres::types::ToSql + Sync); 5] = [
                    &self.tenant_id,
                    &attempt.run_id,
                    &attempt.frame_id,
                    &attempt.local_node_id,
                    &attempt.occurrence,
                ];
                let coordinate_exists: bool = transaction
                    .query_one(
                        effect_attempt_coordinate_exists().text(),
                        &coordinate_params,
                    )
                    .await
                    .map_err(|source| {
                        EffectWriterError::new(
                            EffectWriterErrorKind::Storage,
                            "classify existing effect coordinate",
                        )
                        .with_source(source)
                    })?
                    .get(0);
                if coordinate_exists {
                    return Err(EffectWriterError::new(
                        EffectWriterErrorKind::DivergentRetry,
                        "verify effect-attempt retry",
                    ));
                }

                let runnable: bool = transaction
                    .query_one(effect_run_is_runnable().text(), &[&attempt.run_id])
                    .await
                    .map_err(|source| {
                        EffectWriterError::new(
                            EffectWriterErrorKind::Storage,
                            "validate runnable effect run",
                        )
                        .with_source(source)
                    })?
                    .get(0);
                if !runnable {
                    return Err(EffectWriterError::new(
                        EffectWriterErrorKind::RunNotRunnable,
                        "validate runnable effect run",
                    ));
                }

                let inserted = transaction
                    .query_opt(begin_effect_attempt().text(), &params)
                    .await
                    .map_err(|source| {
                        EffectWriterError::new(
                            EffectWriterErrorKind::Storage,
                            "insert effect attempt",
                        )
                        .with_source(source)
                    })?;
                match inserted {
                    Some(row) => row,
                    None => transaction
                        .query_opt(verify_effect_attempt().text(), &params)
                        .await
                        .map_err(|source| {
                            EffectWriterError::new(
                                EffectWriterErrorKind::Storage,
                                "verify effect-attempt retry",
                            )
                            .with_source(source)
                        })?
                        .ok_or_else(|| {
                            EffectWriterError::new(
                                EffectWriterErrorKind::DivergentRetry,
                                "verify effect-attempt retry",
                            )
                        })?,
                }
            }
        };
        let result = EffectAttempt {
            attempt_id: row.get(0),
            attempt_started_at: row.get(1),
            created_at: row.get(2),
        };
        transaction.commit().await.map_err(|source| {
            EffectWriterError::new(EffectWriterErrorKind::Storage, "commit effect attempt")
                .with_source(source)
        })?;
        Ok(result)
    }

    /// Acquire the unique dispatch permit.
    ///
    /// `None` means the named attempt exists but its dispatch permit was
    /// already acquired; it is never permission to send. A nonexistent
    /// attempt is a typed [`EffectWriterErrorKind::MissingAttempt`] refusal.
    pub async fn acquire_dispatch(
        &self,
        attempt: EffectAttemptId<'_>,
    ) -> Result<Option<EffectDispatchPermit>, EffectWriterError> {
        let mut connection = self.pool.get().await.map_err(|source| {
            EffectWriterError::new(
                EffectWriterErrorKind::Storage,
                "checkout private connection",
            )
            .with_source(source)
        })?;
        let transaction = connection
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "begin ledger transaction")
                    .with_source(source)
            })?;
        transaction
            .query_one(
                bind_writer_authority().text(),
                &[&self.tenant_id, &self.schema],
            )
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "bind ledger authority")
                    .with_source(source)
            })?;
        let row = transaction
            .query_opt(
                acquire_effect_dispatch().text(),
                &[&self.tenant_id, &attempt.attempt_id],
            )
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "acquire effect dispatch")
                    .with_attempt(attempt.attempt_id)
                    .with_source(source)
            })?;
        let permit = match row {
            Some(row) => Some(EffectDispatchPermit {
                attempt_id: row.get(0),
                dispatched_at: row.get(1),
            }),
            None => {
                let exists: bool = transaction
                    .query_one(
                        effect_attempt_exists().text(),
                        &[&self.tenant_id, &attempt.attempt_id],
                    )
                    .await
                    .map_err(|source| {
                        EffectWriterError::new(
                            EffectWriterErrorKind::Storage,
                            "classify effect dispatch refusal",
                        )
                        .with_attempt(attempt.attempt_id)
                        .with_source(source)
                    })?
                    .get(0);
                if !exists {
                    return Err(EffectWriterError::new(
                        EffectWriterErrorKind::MissingAttempt,
                        "acquire effect dispatch",
                    )
                    .with_attempt(attempt.attempt_id));
                }
                None
            }
        };
        transaction.commit().await.map_err(|source| {
            EffectWriterError::new(EffectWriterErrorKind::Storage, "commit effect dispatch")
                .with_attempt(attempt.attempt_id)
                .with_source(source)
        })?;
        Ok(permit)
    }

    /// Insert an outcome or verify its exact facts after a concurrent winner.
    pub async fn record_outcome(
        &self,
        outcome: RecordEffectOutcome<'_>,
    ) -> Result<EffectOutcome, EffectWriterError> {
        let mut connection = self.pool.get().await.map_err(|source| {
            EffectWriterError::new(
                EffectWriterErrorKind::Storage,
                "checkout private connection",
            )
            .with_source(source)
        })?;
        let transaction = connection
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "begin ledger transaction")
                    .with_source(source)
            })?;
        transaction
            .query_one(
                bind_writer_authority().text(),
                &[&self.tenant_id, &self.schema],
            )
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "bind ledger authority")
                    .with_source(source)
            })?;
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] = [
            &self.tenant_id,
            &outcome.attempt.attempt_id,
            &outcome.outcome_status,
        ];
        let inserted = transaction
            .query_opt(record_effect_outcome().text(), &params)
            .await
            .map_err(|source| {
                EffectWriterError::new(EffectWriterErrorKind::Storage, "insert effect outcome")
                    .with_attempt(outcome.attempt.attempt_id)
                    .with_source(source)
            })?;
        let row = match inserted {
            Some(row) => row,
            None => {
                let verified = transaction
                    .query_opt(verify_effect_outcome().text(), &params)
                    .await
                    .map_err(|source| {
                        EffectWriterError::new(
                            EffectWriterErrorKind::Storage,
                            "verify effect-outcome retry",
                        )
                        .with_attempt(outcome.attempt.attempt_id)
                        .with_source(source)
                    })?;
                if let Some(row) = verified {
                    row
                } else {
                    let dispatch_exists: bool = transaction
                        .query_one(
                            effect_dispatch_exists().text(),
                            &[&self.tenant_id, &outcome.attempt.attempt_id],
                        )
                        .await
                        .map_err(|source| {
                            EffectWriterError::new(
                                EffectWriterErrorKind::Storage,
                                "classify effect outcome refusal",
                            )
                            .with_attempt(outcome.attempt.attempt_id)
                            .with_source(source)
                        })?
                        .get(0);
                    if !dispatch_exists {
                        return Err(EffectWriterError::new(
                            EffectWriterErrorKind::MissingDispatch,
                            "record effect outcome",
                        )
                        .with_attempt(outcome.attempt.attempt_id));
                    }
                    return Err(EffectWriterError::new(
                        EffectWriterErrorKind::DivergentRetry,
                        "verify effect-outcome retry",
                    )
                    .with_attempt(outcome.attempt.attempt_id));
                }
            }
        };
        let result = EffectOutcome {
            dispatched_at: row.get(0),
            recorded_at: row.get(1),
        };
        transaction.commit().await.map_err(|source| {
            EffectWriterError::new(EffectWriterErrorKind::Storage, "commit effect outcome")
                .with_attempt(outcome.attempt.attempt_id)
                .with_source(source)
        })?;
        Ok(result)
    }
}

fn valid_schema(schema: &str) -> bool {
    let bytes = schema.as_bytes();
    (1..=63).contains(&bytes.len())
        && matches!(bytes[0], b'A'..=b'Z' | b'a'..=b'z' | b'_')
        && bytes
            .iter()
            .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

/// Server-minted identity and timestamps of one immutable attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectAttempt {
    pub attempt_id: String,
    pub attempt_started_at: String,
    pub created_at: String,
}

/// The only positive dispatch authorization returned by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDispatchPermit {
    pub attempt_id: String,
    pub dispatched_at: String,
}

/// Server timestamps of one immutable recorded outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectOutcome {
    pub dispatched_at: String,
    pub recorded_at: String,
}

/// Bind the trusted host authority for one transaction.
fn bind_writer_authority() -> Sql {
    Sql::new(
        "SELECT pg_catalog.set_config('app.tenant', $1::text, true), \
                pg_catalog.set_config( \
                    'search_path', \
                    pg_catalog.quote_ident($2::text) || ', pg_catalog, pg_temp', true)",
        2,
    )
}

/// Recheck runnable queue authority after the shared effect-intent fence.
///
/// Lease-owner/generation matching remains the activation boundary owned by
/// `.5.4`; this prevents creation after claim-time terminalization and rejects
/// runs without a currently live execution lease.
fn effect_run_is_runnable() -> Sql {
    Sql::new(
        r#"SELECT EXISTS (
    SELECT 1
      FROM runs AS runnable_run
      JOIN run_queue AS queue
        ON queue.tenant_id = runnable_run.tenant_id
       AND queue.run_id = runnable_run.run_id
     WHERE runnable_run.tenant_id = current_setting('app.tenant', true)
       AND runnable_run.run_id = $1::text
       AND runnable_run.status = 'running'
       AND queue.lease_owner IS NOT NULL
       AND queue.lease_expires_at > statement_timestamp()
)"#,
        1,
    )
}

/// Begin one immutable attempt, or classify a retry against every caller fact.
///
/// A newly inserted row returns its server-minted attempt id and timestamps. An
/// existing coordinate returns the same server values with `identical-retry`
/// only when the complete proposed row is equal; `divergent` is a refusal for
/// the eventual private adapter. No live database behavior is proven here.
pub(crate) fn begin_effect_attempt() -> Sql {
    Sql::new(
        r#"INSERT INTO effect_attempts
      (tenant_id, run_id, root_plan_hash, current_plan_hash, frame_id,
       parent_frame_id, call_site_id, local_node_id, source_artifact_hash,
       requirement_name, occurrence, seq, generation_fact_kind,
       connection_name, connection_generation, credential_generation,
       verified_author_principal, verified_publisher_principal,
       attempt_deadline_at, attempt_input_ref)
VALUES ($1::text, $2::text, $3::text, $4::text, $5::bigint,
        $6::bigint, $7::text, $8::text, $9::text, $10::text,
        $11::int, $12::int, $13::text, $14::text, $15::text,
        $16::text, $17::text, $18::text, $19::text::timestamptz, $20::text)
  ON CONFLICT (tenant_id, run_id, frame_id, local_node_id, occurrence)
      DO NOTHING
RETURNING attempt_id::text, attempt_started_at::text, created_at::text"#,
        20,
    )
}

/// Verify a retry in a statement after the insert/winner wait has completed.
pub(crate) fn verify_effect_attempt() -> Sql {
    Sql::new(
        r#"SELECT attempt_id::text, attempt_started_at::text, created_at::text
  FROM effect_attempts
 WHERE tenant_id = $1::text
   AND run_id = $2::text
   AND frame_id = $5::bigint
   AND local_node_id = $8::text
   AND occurrence = $11::int
   AND ROW(tenant_id, run_id, root_plan_hash, current_plan_hash, frame_id,
           parent_frame_id, call_site_id, local_node_id, source_artifact_hash,
           requirement_name, occurrence, seq, generation_fact_kind,
           connection_name, connection_generation, credential_generation,
           verified_author_principal, verified_publisher_principal,
           attempt_deadline_at, attempt_input_ref)
       IS NOT DISTINCT FROM
       ROW($1::text, $2::text, $3::text, $4::text, $5::bigint,
           $6::bigint, $7::text, $8::text, $9::text, $10::text,
           $11::int, $12::int, $13::text, $14::text, $15::text,
           $16::text, $17::text, $18::text,
           $19::text::timestamptz, $20::text)"#,
        20,
    )
}

/// Distinguish a divergent retry from a new coordinate before authorization.
fn effect_attempt_coordinate_exists() -> Sql {
    Sql::new(
        "SELECT EXISTS (SELECT 1 FROM effect_attempts \
          WHERE tenant_id = $1::text AND run_id = $2::text \
            AND frame_id = $3::bigint AND local_node_id = $4::text \
            AND occurrence = $5::int)",
        5,
    )
}

/// Acquire the sole dispatch permit for an attempt.
///
/// Coordinates are copied from the referenced attempt inside PostgreSQL. Only
/// a row returned by this insert represents permission to perform the effect.
pub(crate) fn acquire_effect_dispatch() -> Sql {
    Sql::new(
        r#"INSERT INTO effect_attempt_dispatches
    (tenant_id, attempt_id, attempt_started_at,
     run_id, frame_id, local_node_id, occurrence)
SELECT tenant_id, attempt_id, attempt_started_at,
       run_id, frame_id, local_node_id, occurrence
  FROM effect_attempts
 WHERE tenant_id = $1::text
   AND attempt_id = $2::text::uuid
ON CONFLICT DO NOTHING
RETURNING attempt_id::text, dispatched_at::text"#,
        2,
    )
}

/// Classify a zero-row dispatch insert without turning it into permission.
pub(crate) fn effect_attempt_exists() -> Sql {
    Sql::new(
        "SELECT EXISTS (SELECT 1 FROM effect_attempts \
          WHERE tenant_id = $1::text AND attempt_id = $2::text::uuid)",
        2,
    )
}

/// Record one immutable outcome, or classify an exact/divergent retry.
///
/// The dispatch timestamp is copied from the referenced permit. The eventual
/// adapter must accept only `inserted` and `identical-retry` rows.
pub(crate) fn record_effect_outcome() -> Sql {
    Sql::new(
        r#"INSERT INTO effect_attempt_outcomes
      (tenant_id, attempt_id, dispatched_at, outcome_status)
  SELECT dispatch.tenant_id, dispatch.attempt_id, dispatch.dispatched_at, $3::text
    FROM effect_attempt_dispatches AS dispatch
   WHERE dispatch.tenant_id = $1::text
     AND dispatch.attempt_id = $2::text::uuid
  ON CONFLICT (tenant_id, attempt_id) DO NOTHING
RETURNING dispatched_at::text, recorded_at::text"#,
        3,
    )
}

/// Verify an outcome retry in a fresh statement snapshot after conflict wait.
pub(crate) fn verify_effect_outcome() -> Sql {
    Sql::new(
        r#"SELECT outcome.dispatched_at::text, outcome.recorded_at::text
  FROM effect_attempt_outcomes AS outcome
  JOIN effect_attempt_dispatches AS dispatch
    ON dispatch.tenant_id = outcome.tenant_id
   AND dispatch.attempt_id = outcome.attempt_id
 WHERE outcome.tenant_id = $1::text
   AND outcome.attempt_id = $2::text::uuid
   AND outcome.outcome_status IS NOT DISTINCT FROM $3::text
   AND outcome.dispatched_at IS NOT DISTINCT FROM dispatch.dispatched_at"#,
        3,
    )
}

/// Distinguish a missing dispatch from a divergent existing outcome.
pub(crate) fn effect_dispatch_exists() -> Sql {
    Sql::new(
        "SELECT EXISTS (SELECT 1 FROM effect_attempt_dispatches \
          WHERE tenant_id = $1::text AND attempt_id = $2::text::uuid)",
        2,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_effect_dispatch, begin_effect_attempt, bind_writer_authority,
        effect_attempt_coordinate_exists, effect_attempt_exists, effect_dispatch_exists,
        effect_run_is_runnable, record_effect_outcome, valid_schema, verify_effect_attempt,
        verify_effect_outcome,
    };

    #[test]
    fn attempt_builder_compares_all_caller_facts_and_reuses_server_facts() {
        let authority = bind_writer_authority();
        assert_eq!(authority.arity(), 2);
        assert!(
            authority
                .text()
                .contains("pg_catalog.quote_ident($2::text) || ', pg_catalog, pg_temp'")
        );
        let statement = begin_effect_attempt();
        assert_eq!(statement.arity(), 20);
        assert!(statement.text().contains("attempt_id::text"));
        assert!(statement.text().contains("attempt_started_at::text"));
        assert!(statement.text().contains("created_at::text"));
        assert!(!statement.text().contains("attempt_key"));
        assert!(!statement.text().contains("wamn_run."));
        let verify = verify_effect_attempt();
        assert_eq!(verify.arity(), 20);
        assert!(verify.text().contains("IS NOT DISTINCT FROM"));
        assert!(verify.text().contains("attempt_deadline_at"));
        assert!(verify.text().contains("attempt_input_ref"));
        assert!(!verify.text().contains("wamn_run."));
    }

    #[test]
    fn attempt_writer_rechecks_a_live_running_queue_row_after_the_fence() {
        let statement = effect_run_is_runnable();
        assert_eq!(statement.arity(), 1);
        assert!(statement.text().contains("JOIN run_queue AS queue"));
        assert!(statement.text().contains("runnable_run.status = 'running'"));
        assert!(statement.text().contains("queue.lease_owner IS NOT NULL"));
        assert!(
            statement
                .text()
                .contains("queue.lease_expires_at > statement_timestamp()")
        );

        // wamn-hopk R5: the statement ORDER was asserted by locating call sites
        // by byte offset in this file's own source. Deleted; the generated-SQL
        // pins above stay, because those pin a builder's output, not source text.
        assert_eq!(effect_attempt_coordinate_exists().arity(), 5);
    }

    #[test]
    fn dispatch_permit_is_only_a_returned_new_coordinate_row() {
        let statement = acquire_effect_dispatch();
        assert_eq!(statement.arity(), 2);
        assert!(statement.text().contains(
            "SELECT tenant_id, attempt_id, attempt_started_at,\n       run_id, frame_id, local_node_id, occurrence"
        ));
        assert!(statement.text().contains("ON CONFLICT DO NOTHING"));
        assert!(
            statement
                .text()
                .contains("RETURNING attempt_id::text, dispatched_at::text")
        );
        assert_eq!(effect_attempt_exists().arity(), 2);
    }

    #[test]
    fn outcome_builder_accepts_only_an_identical_retry() {
        let statement = record_effect_outcome();
        assert_eq!(statement.arity(), 3);
        assert!(
            statement
                .text()
                .contains("ON CONFLICT (tenant_id, attempt_id) DO NOTHING")
        );
        assert!(statement.text().contains("recorded_at::text"));
        let verify = verify_effect_outcome();
        assert_eq!(verify.arity(), 3);
        assert!(
            verify
                .text()
                .contains("outcome_status IS NOT DISTINCT FROM")
        );
        assert!(verify.text().contains("dispatched_at IS NOT DISTINCT FROM"));
        assert_eq!(effect_dispatch_exists().arity(), 2);
    }

    #[test]
    fn writer_statements_use_only_the_host_bound_search_path() {
        let authority = bind_writer_authority();
        assert!(authority.text().contains("'app.tenant', $1::text, true"));
        assert!(
            authority
                .text()
                .contains("pg_catalog.quote_ident($2::text) || ', pg_catalog, pg_temp'")
        );
        for statement in [
            begin_effect_attempt(),
            verify_effect_attempt(),
            effect_attempt_coordinate_exists(),
            effect_run_is_runnable(),
            acquire_effect_dispatch(),
            effect_attempt_exists(),
            record_effect_outcome(),
            verify_effect_outcome(),
            effect_dispatch_exists(),
        ] {
            assert!(!statement.text().contains("wamn_run."));
            assert!(!statement.text().contains("search_path"));
        }
    }

    #[test]
    fn host_schema_identity_is_canonical_before_it_is_bound() {
        for accepted in ["wamn_run", "wamn_runner_demo", "_internal", "Mixed42"] {
            assert!(valid_schema(accepted), "valid schema {accepted}");
        }
        for refused in ["", "42bad", "bad-name", "bad.name", "é", "a b"] {
            assert!(!valid_schema(refused), "invalid schema {refused:?}");
        }
        assert!(valid_schema(&format!("a{}", "z".repeat(62))));
        assert!(!valid_schema(&format!("a{}", "z".repeat(63))));
    }
}
