//! PostgreSQL input for the event reader.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use pg_walstream::{
    CancellationToken, ColumnValue, EventStream, EventType, LogicalReplicationStream,
    RelationColumn, ReplicationError, ReplicationStreamConfig, RetryConfig, RowData, StreamingMode,
};
use postgres_protocol::escape::escape_identifier;
use sqlx::{ConnectOptions as _, Connection as _, Row as _};
use wamn_event::reader::{
    RelationClassification, ReplicationFrame, ReplicationInput, SessionFailure, SessionFate,
};
use wamn_event::{Image, Op, UnavailableColumn};

/// A verified connection and progress shared by one reader's sessions.
pub struct ReplicationSource {
    connection: String,
    name: String,
    last_advance_ms: AtomicI64,
}

impl fmt::Debug for ReplicationSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplicationSource")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl ReplicationSource {
    /// Wait for the provisioned replication position and prepare its connection.
    pub async fn prepare(input: &str, name: String) -> anyhow::Result<Self> {
        let options = wait_for_replication(input, &name).await?;
        let mut input = options.to_url_lossy();
        // SQLx adds a certificate-file prefix and a statement-cache setting.
        // The replication client expects libpq connection options.
        let pairs: Vec<_> = input
            .query_pairs()
            .filter(|(name, _)| name != "statement-cache-capacity")
            .map(|(name, value)| {
                let value = if matches!(name.as_ref(), "sslrootcert" | "sslcert" | "sslkey")
                    && let Some(path) = value.strip_prefix("file: ")
                {
                    path.to_owned()
                } else {
                    value.into_owned()
                };
                (name.into_owned(), value)
            })
            .collect();
        input.query_pairs_mut().clear().extend_pairs(pairs);
        // libpq expects percent-encoded spaces in file paths.
        if let Some(query) = input.query().filter(|query| query.contains('+')) {
            input.set_query(Some(&query.replace('+', "%20")));
        }
        Ok(Self {
            connection: input.into(),
            name,
            last_advance_ms: AtomicI64::new(0),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Monitor WAL retention on a separate query connection until cancelled.
    pub fn start_monitor(
        self: &Arc<Self>,
        interval: Duration,
        warn_minimum: i64,
        token: CancellationToken,
    ) {
        self.note_progress();
        if interval.is_zero() {
            return;
        }
        let source = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                if let Err(error) = poll_retention(&source, warn_minimum).await {
                    tracing::warn!(
                        target: "wamn::event_reader",
                        { "slot" = %source.name, error = %error },
                        "slot-headroom poll failed (transient) — will retry"
                    );
                }
                tokio::select! {
                    () = token.cancelled() => return,
                    () = tokio::time::sleep(interval) => {}
                }
            }
        });
    }

    fn note_progress(&self) {
        self.last_advance_ms
            .store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
    }

    /// Open a session on the existing publication and replication position.
    pub async fn open(
        &self,
        feedback_interval: Duration,
        token: CancellationToken,
    ) -> Result<ReplicationSession<'_>, SessionFailure> {
        open_session(self, feedback_interval, token)
            .await
            .map_err(session_failure)
    }
}

/// An open replication session with its source identity and timeline history.
pub struct ReplicationSession<'a> {
    source: &'a ReplicationSource,
    stream: EventStream,
    identity: pg_walstream::ServerIdentity,
    history: Vec<pg_walstream::TimelineSwitch>,
    relations: HashMap<u32, Relation>,
}

struct Relation {
    columns: Vec<RelationColumn>,
    replica_key: Arc<[Arc<str>]>,
}

impl Relation {
    fn unavailable(&self, old: Option<&RowData>, new: Option<&RowData>) -> Vec<UnavailableColumn> {
        let mut unavailable = Vec::new();
        // pg_walstream skips unchanged TOAST values. A key-only old tuple
        // carries only replica identity values. FULL marks every column as key.
        for (image, row) in [(Image::Old, old), (Image::New, new)] {
            let Some(row) = row else { continue };
            if row.len() == self.columns.len() {
                continue;
            }
            for column in &self.columns {
                if (image == Image::New || column.is_key) && row.get(&column.name).is_none() {
                    unavailable.push(UnavailableColumn {
                        image,
                        column: column.name.to_string(),
                    });
                }
            }
        }
        unavailable
    }
}

impl fmt::Debug for ReplicationSession<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplicationSession")
            .field("source", self.source)
            .finish_non_exhaustive()
    }
}

impl ReplicationSession<'_> {
    /// Stop reading and release the replication connection.
    pub async fn shutdown(&mut self) {
        let _ = self.stream.shutdown().await;
    }

    fn row(
        &self,
        op: Op,
        schema: Arc<str>,
        table: Arc<str>,
        relation_oid: u32,
        old: Option<RowData>,
        new: Option<RowData>,
    ) -> Result<ReplicationFrame<RowData>, SessionFailure> {
        let relation = self.relations.get(&relation_oid).ok_or_else(|| {
            session_failure(ReplicationError::Protocol(format!(
                "CDC row arrived without a relation frame for OID {relation_oid}"
            )))
        })?;
        Ok(ReplicationFrame::Row {
            op,
            schema,
            table,
            relation_oid,
            unavailable: relation.unavailable(old.as_ref(), new.as_ref()),
            old,
            new,
            replica_key: Arc::clone(&relation.replica_key),
        })
    }
}

impl ReplicationInput for ReplicationSession<'_> {
    type Row = RowData;

    async fn next_frame(&mut self) -> Result<Option<ReplicationFrame<Self::Row>>, SessionFailure> {
        loop {
            let event = match self.stream.next_event().await {
                Ok(event) => event,
                Err(ReplicationError::Cancelled(_)) => return Ok(None),
                Err(error) => return Err(session_failure(error)),
            };
            let frame = match event.event_type {
                EventType::Begin {
                    transaction_id,
                    commit_timestamp,
                    ..
                } => ReplicationFrame::Begin {
                    txid: transaction_id,
                    commit_ts: commit_timestamp,
                },
                EventType::Commit {
                    commit_lsn,
                    end_lsn,
                    ..
                } => ReplicationFrame::Commit {
                    commit_lsn: commit_lsn.value(),
                    end_lsn: end_lsn.value(),
                },
                EventType::Insert {
                    schema,
                    table,
                    relation_oid,
                    data,
                } => self.row(Op::Insert, schema, table, relation_oid, None, Some(data))?,
                EventType::Update {
                    schema,
                    table,
                    relation_oid,
                    old_data,
                    new_data,
                    ..
                } => self.row(
                    Op::Update,
                    schema,
                    table,
                    relation_oid,
                    old_data,
                    Some(new_data),
                )?,
                EventType::Delete {
                    schema,
                    table,
                    relation_oid,
                    old_data,
                    ..
                } => self.row(
                    Op::Delete,
                    schema,
                    table,
                    relation_oid,
                    Some(old_data),
                    None,
                )?,
                EventType::Message {
                    flags,
                    prefix,
                    content,
                    ..
                } => ReplicationFrame::Message {
                    flags,
                    prefix,
                    content,
                },
                EventType::Truncate { tables, .. } => ReplicationFrame::Truncate { tables },
                EventType::Keepalive { wal_end } => ReplicationFrame::Keepalive {
                    wal_end: wal_end.value(),
                },
                EventType::Relation {
                    relation_id,
                    columns,
                    ..
                } => {
                    let replica_key = columns
                        .iter()
                        .filter(|column| column.is_key)
                        .map(|column| Arc::clone(&column.name))
                        .collect();
                    self.relations.insert(
                        relation_id,
                        Relation {
                            columns,
                            replica_key,
                        },
                    );
                    continue;
                }
                EventType::Type { .. } | EventType::Origin { .. } => {
                    continue;
                }
                other => {
                    // Streaming and two-phase frames are disabled.
                    tracing::warn!(target: "wamn_cdc_reader", ?other, "unexpected replication frame — skipped");
                    continue;
                }
            };
            return Ok(Some(frame));
        }
    }

    async fn relation_classification(
        &mut self,
        schema: &str,
        relation_oid: u32,
    ) -> Result<RelationClassification, SessionFailure> {
        resolve_relation_classification(&self.source.connection, schema, relation_oid)
            .await
            .map_err(session_failure)
    }

    async fn key_columns(&mut self, relation_oid: u32) -> Result<Vec<String>, SessionFailure> {
        // pgoutput marks the replica identity, which can use a different index
        // from the primary key. It does not mark generated identity columns.
        let lookup = async {
            let mut client = wamn_postgres::connect(&self.source.connection).await?;
            let columns = sqlx::query_scalar(
                "SELECT attribute.attname::text FROM pg_catalog.pg_attribute AS attribute
                 WHERE attribute.attrelid = $1 AND attribute.attnum > 0 AND NOT attribute.attisdropped
                   AND (attribute.attidentity <> '' OR EXISTS (
                      SELECT FROM pg_catalog.pg_constraint AS pk
                      WHERE pk.conrelid = attribute.attrelid AND pk.contype = 'p'
                        AND attribute.attnum = ANY(pk.conkey)
                   ))",
            )
            .bind(sqlx::postgres::types::Oid(relation_oid))
            .fetch_all(&mut client)
            .await?;
            anyhow::Ok(columns)
        };
        lookup.await.map_err(|source| SessionFailure {
            fate: SessionFate::Reopen,
            source: source.context(format!(
                "read CDC key columns for relation OID {relation_oid}"
            )),
        })
    }

    fn decode_row(
        row: &Self::Row,
    ) -> Result<serde_json::Map<String, serde_json::Value>, SessionFailure> {
        row_to_map(row).map_err(session_failure)
    }

    fn origin(&self, commit_lsn: u64) -> (u64, u32) {
        let timeline = self
            .history
            .iter()
            .find(|ancestor| commit_lsn < ancestor.switchpoint().value())
            .map_or(
                self.identity.timeline(),
                pg_walstream::TimelineSwitch::timeline,
            );
        (self.identity.system_id(), timeline)
    }

    fn confirmed_position(&self) -> u64 {
        self.stream.get_feedback_lsn().0
    }

    fn confirm(&mut self, position: u64) {
        self.stream.update_flushed_lsn(position);
        self.stream.update_applied_lsn(position);
        self.source.note_progress();
    }
}

fn session_failure(error: ReplicationError) -> SessionFailure {
    SessionFailure {
        fate: classify(&error),
        source: error.into(),
    }
}

/// Classify a pg_walstream error. Invalidation keywords are validated across
/// ALL variants (the server message may surface as a connection error).
fn classify(e: &ReplicationError) -> SessionFate {
    let msg = e.to_string().to_ascii_lowercase();
    if msg.contains("invalidat")
        || msg.contains("can no longer be used")
        || (msg.contains("replication slot") && msg.contains("does not exist"))
    {
        return SessionFate::CaptureGap;
    }
    match e {
        ReplicationError::Cancelled(_) => SessionFate::Cancelled,
        ReplicationError::ReplicationSlot(_) => SessionFate::CaptureGap,
        ReplicationError::Authentication(_) | ReplicationError::Config(_) => SessionFate::Fatal,
        _ => SessionFate::Reopen,
    }
}

/// Resolve one WAL relation OID against the two closed package classifications:
/// a publishable entity or an explicitly excluded internal relation.
fn relation_classification_lookup_sql(schema: &str) -> String {
    format!(
        "SELECT entity.package_id AS entity_package_id, entity.entity_id, \
                excluded.package_id AS excluded_package_id, excluded.relation_id \
           FROM (SELECT $1::oid AS relation_oid) AS lookup \
           LEFT JOIN {}.wamn_entities AS entity USING (relation_oid) \
           LEFT JOIN {}.wamn_cdc_exclusions AS excluded USING (relation_oid)",
        escape_identifier(schema),
        escape_identifier(schema),
    )
}

/// Typed refusal emitted before an unmapped managed relation can reach NATS.
const UNMAPPED_MANAGED_RELATION_REFUSAL: &str = "cdc-unmapped-managed-relation";

/// Operator remedy paired with [`UNMAPPED_MANAGED_RELATION_REFUSAL`].
const UNMAPPED_MANAGED_RELATION_REMEDY: &str =
    "reconcile the installed package relation classifications before resuming CDC publication";

/// Resolve one relation OID to its package-owned CDC classification over a
/// short-lived SQL connection (the CDC role's
/// grants cover both maps). An absent row or map is a non-retryable typed refusal;
/// connection/query failure remains transient and reopens the session.
async fn resolve_relation_classification(
    connection: &str,
    schema: &str,
    relation_oid: u32,
) -> Result<RelationClassification, ReplicationError> {
    let mut client = wamn_postgres::connect(connection)
        .await
        .map_err(|e| ReplicationError::TransientConnection(format!("relation-map connect: {e}")))?;

    match sqlx::query(sqlx::AssertSqlSafe(relation_classification_lookup_sql(
        schema,
    )))
    .bind(sqlx::postgres::types::Oid(relation_oid))
    .fetch_one(&mut client)
    .await
    {
        Ok(row) => {
            let entity_package = row
                .try_get::<Option<String>, _>("entity_package_id")
                .map_err(|error| ReplicationError::Config(error.to_string()))?
                .map(wamn_package::PackageName::try_from)
                .transpose()
                .map_err(|error| {
                    ReplicationError::Config(format!("decode entity_package_id: {error}"))
                })?;
            let entity_id = row.get::<Option<String>, _>("entity_id");
            let excluded_package = row
                .try_get::<Option<String>, _>("excluded_package_id")
                .map_err(|error| ReplicationError::Config(error.to_string()))?
                .map(wamn_package::PackageName::try_from)
                .transpose()
                .map_err(|error| {
                    ReplicationError::Config(format!("decode excluded_package_id: {error}"))
                })?;
            let excluded_relation = row.get::<Option<String>, _>("relation_id");
            match (
                entity_package,
                entity_id,
                excluded_package,
                excluded_relation,
            ) {
                (Some(package_id), Some(entity_id), None, None) => {
                    Ok(RelationClassification::Entity {
                        package_id,
                        entity_id,
                    })
                }
                (None, None, Some(package_id), Some(excluded_id)) => {
                    Ok(RelationClassification::Excluded {
                        package_id,
                        relation_id: excluded_id,
                    })
                }
                (None, None, None, None) => Err(ReplicationError::Config(format!(
                    "{UNMAPPED_MANAGED_RELATION_REFUSAL}: schema={schema:?} relation_oid={relation_oid}; remedy: {UNMAPPED_MANAGED_RELATION_REMEDY}"
                ))),
                _ => Err(ReplicationError::Config(format!(
                    "{UNMAPPED_MANAGED_RELATION_REFUSAL}: schema={schema:?} relation_oid={relation_oid} has conflicting classifications; remedy: {UNMAPPED_MANAGED_RELATION_REMEDY}"
                ))),
            }
        }
        Err(error)
            if error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref()
                == Some("42P01") =>
        {
            Err(ReplicationError::Config(format!(
                "{UNMAPPED_MANAGED_RELATION_REFUSAL}: schema={schema:?} has incomplete CDC relation maps; remedy: {UNMAPPED_MANAGED_RELATION_REMEDY}"
            )))
        }
        Err(e) => Err(ReplicationError::TransientConnection(format!(
            "relation-map lookup: {e}"
        ))),
    }
}

/// pgoutput text row → the envelope's column→value map. Values stay in text
/// representation (string or null). The relation metadata identifies unchanged
/// TOAST columns, which are already absent from `RowData`.
///
/// The v3 wire carries each value as a JSON string, so it must be valid UTF-8.
/// A non-UTF-8 value means the source database is not UTF-8-encoded — refuse it
/// LOUDLY (a `Config` error, classified `Fatal`: non-retryable, the reader
/// exits and PostgreSQL retains WAL) rather than corrupt it with a lossy `U+FFFD`
/// substitution (R19). Postgres validates text on input, so a UTF-8 source
/// never reaches this path; hitting it requires the operator to fix the encoding.
fn row_to_map(
    row: &RowData,
) -> Result<serde_json::Map<String, serde_json::Value>, ReplicationError> {
    let mut map = serde_json::Map::with_capacity(row.len());
    for (name, value) in row.iter() {
        let v = match value {
            ColumnValue::Null => serde_json::Value::Null,
            other => {
                let s = std::str::from_utf8(other.as_bytes()).map_err(|e| {
                    ReplicationError::Config(format!(
                        "non-UTF-8 value in column {name} ({e}) — the v3 wire carries \
                         pgoutput text as JSON strings; refusing rather than corrupting \
                         it (R19). The source database encoding must be UTF-8."
                    ))
                })?;
                serde_json::Value::String(s.to_owned())
            }
        };
        map.insert(name.to_string(), v);
    }
    Ok(map)
}

async fn wait_for_replication(
    input: &str,
    name: &str,
) -> anyhow::Result<sqlx::postgres::PgConnectOptions> {
    let timeout = Duration::from_secs(120);
    let wait = async {
        let options = wamn_postgres::connect_options(input).await?;
        let mut client = sqlx::PgConnection::connect_with(&options).await?;
        while !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT FROM pg_catalog.pg_replication_slots WHERE slot_name = $1)",
        )
        .bind(name)
        .fetch_one(&mut client)
        .await?
        {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        anyhow::Ok(options)
    };
    tokio::time::timeout(timeout, wait)
        .await
        .with_context(|| {
            format!(
                "replication slot {name} did not appear within {} seconds",
                timeout.as_secs()
            )
        })?
        .with_context(|| format!("read replication slot {name} during startup"))
}

async fn open_session(
    source: &ReplicationSource,
    feedback_interval: Duration,
    token: CancellationToken,
) -> Result<ReplicationSession<'_>, ReplicationError> {
    let url =
        replication_url(&source.connection).map_err(|e| ReplicationError::Config(e.to_string()))?;
    let cfg = ReplicationStreamConfig::new(
        source.name.clone(),
        source.name.clone(),
        2,
        // Off: whole transactions, post-commit, in commit order — nothing
        // uncommitted is ever published (giant txns spill server-side).
        StreamingMode::Off,
        feedback_interval,
        Duration::from_secs(30),
        Duration::from_secs(30),
        RetryConfig::default(),
    )
    // Deliver logical-decoding Message frames (the `wamn.causation` stamp the
    // wamn:postgres plugin emits per run-owned txn — wamn-l5i9.12): off by
    // default, so the reader must opt in for `drain` to see them.
    .with_messages(true)
    .with_existing_slot_required(true);
    let mut stream = LogicalReplicationStream::new(&url, cfg).await?;
    let identity = *stream.server_identity();
    let history = stream.timeline_history().await?;
    // Provisioning alone creates the replication position. Its absence stops capture.
    stream.start(None).await?;
    Ok(ReplicationSession {
        source,
        stream: stream.into_stream(token),
        identity,
        history,
        relations: HashMap::new(),
    })
}

/// WAL-retention headroom, derived from `pg_replication_slots` (E2
/// backstop). The ladder maps onto Postgres' own `wal_status` semantics plus a
/// `safe_wal_size` early-warning minimum so the alert fires BEFORE the status
/// leaves `reserved`.
#[derive(Debug, PartialEq, Eq)]
enum RetentionHealth {
    /// `reserved` with headroom above the warn minimum.
    Healthy,
    /// Still `reserved` but `safe_wal_size` fell below the warn minimum — the
    /// early warning before the status degrades.
    HeadroomLow,
    /// `extended`: past `max_wal_size`, retained only by `max_slot_wal_keep_size`.
    Extended,
    /// `unreserved`: WAL may be removed at the next checkpoint — gap imminent.
    Unreserved,
    /// `lost`: capture is invalidated — the v3 §11 capture-gap incident.
    Lost,
}

/// Map `wal_status` + `safe_wal_size` to a health level. A NULL/unknown
/// status is treated conservatively as `HeadroomLow` (surfaced, never silently
/// healthy). `safe_wal_size` is NULL once the status is not `reserved`, so the
/// minimum test only applies on the `reserved` branch.
fn classify_retention(
    wal_status: &str,
    safe_wal_size: Option<i64>,
    warn_minimum_bytes: i64,
) -> RetentionHealth {
    match wal_status {
        "reserved" => match safe_wal_size {
            Some(n) if n < warn_minimum_bytes => RetentionHealth::HeadroomLow,
            _ => RetentionHealth::Healthy,
        },
        "extended" => RetentionHealth::Extended,
        "unreserved" => RetentionHealth::Unreserved,
        "lost" => RetentionHealth::Lost,
        _ => RetentionHealth::HeadroomLow,
    }
}

/// Seconds since the reader last advanced its confirmed LSN — the
/// `confirmed_lsn_age_seconds` gauge (E2). Clamped at 0.
fn confirmed_lsn_age_seconds(last_lsn_advance_ms: &AtomicI64) -> i64 {
    let last = last_lsn_advance_ms.load(Ordering::Relaxed);
    (chrono::Utc::now().timestamp_millis() - last).max(0) / 1000
}

/// Poll WAL retention over a query connection and emit its health and progress.
async fn poll_retention(source: &ReplicationSource, warn_minimum: i64) -> anyhow::Result<()> {
    let mut client = wamn_postgres::connect(source.connection.as_str())
        .await
        .context("slot-monitor connect")?;

    let row = sqlx::query(
        "SELECT wal_status::text, safe_wal_size, \
             (pg_current_wal_lsn() - restart_lsn)::bigint \
             FROM pg_replication_slots WHERE slot_name = $1",
    )
    .bind(&source.name)
    .fetch_optional(&mut client)
    .await
    .context("slot-monitor query")?;
    let confirmed_lsn_age_seconds = confirmed_lsn_age_seconds(&source.last_advance_ms);
    let Some(row) = row else {
        tracing::error!(
            target: "wamn::event_reader",
            event = "CDC_SLOT_MISSING",
            "slot" = source.name.as_str(),
            confirmed_lsn_age_seconds,
            "slot vanished under the reader — capture gap imminent (v3 §11)"
        );
        return Ok(());
    };
    let wal_status: Option<String> = row.get(0);
    let safe_wal_size: Option<i64> = row.get(1);
    let lag_bytes: Option<i64> = row.get(2);
    let status = wal_status.as_deref().unwrap_or("unknown");
    match classify_retention(status, safe_wal_size, warn_minimum) {
        RetentionHealth::Healthy => tracing::info!(
            target: "wamn::event_reader", event = "cdc_slot_health", "slot" = source.name.as_str(), wal_status = status,
            slot_safe_wal_bytes = ?safe_wal_size, slot_wal_lag_bytes = ?lag_bytes,
            confirmed_lsn_age_seconds, "slot headroom healthy"
        ),
        RetentionHealth::HeadroomLow => tracing::warn!(
            target: "wamn::event_reader", event = "CDC_SLOT_WAL_LOW", "slot" = source.name.as_str(), wal_status = status,
            slot_safe_wal_bytes = ?safe_wal_size, slot_wal_lag_bytes = ?lag_bytes,
            confirmed_lsn_age_seconds,
            "slot WAL headroom low — still reserved; act BEFORE it leaves reserved"
        ),
        RetentionHealth::Extended => tracing::warn!(
            target: "wamn::event_reader", event = "CDC_SLOT_WAL_EXTENDED", "slot" = source.name.as_str(), wal_status = status,
            slot_safe_wal_bytes = ?safe_wal_size, slot_wal_lag_bytes = ?lag_bytes,
            confirmed_lsn_age_seconds,
            "slot past max_wal_size — retained only by max_slot_wal_keep_size"
        ),
        RetentionHealth::Unreserved => tracing::error!(
            target: "wamn::event_reader", event = "CDC_SLOT_WAL_UNRESERVED", "slot" = source.name.as_str(), wal_status = status,
            slot_safe_wal_bytes = ?safe_wal_size, slot_wal_lag_bytes = ?lag_bytes,
            confirmed_lsn_age_seconds,
            "slot WAL no longer safe — invalidation imminent; fix JetStream, do NOT drop the slot (v3 §11)"
        ),
        RetentionHealth::Lost => tracing::error!(
            target: "wamn::event_reader", event = "CDC_SLOT_INVALIDATED", "slot" = source.name.as_str(), wal_status = status,
            confirmed_lsn_age_seconds,
            "slot invalidated — capture GAP (v3 §11)"
        ),
    }
    Ok(())
}

fn replication_url(input: &str) -> anyhow::Result<String> {
    let mut input = url::Url::parse(input)?;
    input
        .query_pairs_mut()
        .append_pair("replication", "database");
    Ok(input.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_metadata_identifies_toast_without_marking_a_null_or_a_missing_old_image() {
        let relation = Relation {
            columns: [("id", true), ("memo", false), ("note", false)]
                .into_iter()
                .map(|(name, is_key)| RelationColumn {
                    name: name.into(),
                    is_key,
                    type_id: 25,
                    type_modifier: -1,
                })
                .collect(),
            replica_key: ["id".into()].into(),
        };
        let new = RowData::from_pairs(vec![
            ("id", ColumnValue::text("7")),
            ("note", ColumnValue::Null),
        ]);
        let unavailable = [UnavailableColumn {
            image: Image::New,
            column: "memo".into(),
        }];
        assert_eq!(relation.unavailable(None, Some(&new)), unavailable);

        let old = RowData::from_pairs(vec![("id", ColumnValue::text("6"))]);
        assert_eq!(relation.unavailable(Some(&old), Some(&new)), unavailable);
    }

    #[test]
    fn classify_routes_the_session_fates() {
        use SessionFate::*;
        assert_eq!(
            classify(&ReplicationError::Cancelled("x".into())),
            Cancelled
        );
        assert_eq!(
            classify(&ReplicationError::Authentication("x".into())),
            Fatal
        );
        assert_eq!(classify(&ReplicationError::Config("x".into())), Fatal);
        assert_eq!(
            classify(&ReplicationError::TransientConnection("reset".into())),
            Reopen
        );
        // Invalidation keywords win regardless of variant (v3 §11 incident).
        assert_eq!(
            classify(&ReplicationError::ReplicationConnection(
                "ERROR: this slot has been invalidated because of wal_removed".into()
            )),
            CaptureGap
        );
        assert_eq!(
            classify(&ReplicationError::Protocol(
                "slot can no longer be used".into()
            )),
            CaptureGap
        );
    }

    #[test]
    fn row_to_map_refuses_non_utf8_rather_than_corrupting() {
        // A non-UTF-8 value (a non-UTF-8 source database) cannot be a JSON
        // string on the frozen v3 wire. The reader REFUSES loudly — a `Config`
        // error, classified `Fatal` (non-retryable) — instead of the old lossy
        // `U+FFFD` substitution (R19).
        let row = RowData::from_pairs(vec![
            ("id", ColumnValue::text("7")),
            (
                "bad",
                ColumnValue::text_bytes(bytes::Bytes::from_static(&[0xff, 0xfe])),
            ),
        ]);
        let err = row_to_map(&row).expect_err("non-UTF-8 must not map lossily");
        assert!(matches!(err, ReplicationError::Config(_)));
        assert_eq!(classify(&err), SessionFate::Fatal);
    }
}
