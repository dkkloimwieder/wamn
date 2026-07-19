//! The `event-reader` subcommand (wamn-l5i9.10, D19 v3 §4): the CDC reader —
//! one pg_walstream session for ONE project-env, publishing row events onto
//! the org+env `EVT_` JetStream stream.
//!
//! Dispatcher-family NATIVE service (the v3 posture exception: it holds the
//! R8b **replication** credential — `WAMN_CDC_URL`, the plain libpq URL from
//! the `wamn-cdc-…` Secret; the reader appends `sslmode` +
//! `replication=database` itself). What it streams comes from its
//! `registry.event_readers` registration (publication / slot / stream — read
//! from the ROW, never derived).
//!
//! Load-bearing semantics:
//!
//! - **Confirmed LSN advances ONLY on JetStream ack**, at transaction
//!   granularity: every row event of a txn is published (`Nats-Msg-Id =
//!   <project>_<env>:<lsn>`) and acked before the `Commit` frame advances the
//!   feedback LSN to the commit's end. JetStream unreachable ⇒ the publish
//!   retries forever ⇒ the LSN holds ⇒ WAL is retained — delayed, never lost.
//!   A crash mid-txn redelivers the whole txn; the msg-id dedupe absorbs the
//!   published prefix (at-least-once → exactly-once on the stream).
//! - **Commit order**: `StreamingMode::Off` — the server delivers whole
//!   transactions in commit order; sequential publish preserves it, so stream
//!   order == commit order per DB.
//! - **The reader NEVER creates the slot.** `enable-cdc-project-env` created
//!   it (WAL pinned from enable); a MISSING or INVALIDATED slot is a capture
//!   GAP — a first-class incident (v3 §11): the reader refuses to start (or
//!   dies) loudly instead of silently re-creating and resuming from "now".
//!   Recovery is operator-driven: re-enable CDC + replay/backfill assessment.
//! - **Session re-open** (S-CDC-1 finding F2): the crate's inner retry can be
//!   shorter than a real primary-less window, so a session-level re-open loop
//!   wraps the drain; re-opens are counted and logged. The slot admits one
//!   consumer, so exclusivity is structural (the lease that elects WHICH
//!   replica holds the session is deferred — run replicas=1).
//! - **Entity keying** (wamn-l5i9.11): each relation resolves to its stable
//!   catalog entity id via the OID-keyed `wamn_entities` map (maintained by
//!   publish/migrate-catalog in the DDL's transaction) — resolved lazily per
//!   session, never invalidated (OIDs survive renames), so envelopes and
//!   subjects stay keyed on the entity id across `ALTER TABLE RENAME` (R9b).
//!   Unmapped tables publish with the table-name fallback, `entity` absent.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, bail};
use async_nats::header::{HeaderMap, NATS_MESSAGE_ID};
use async_nats::jetstream;
use clap::Args;
use pg_walstream::{
    CancellationToken, ColumnValue, EventStream, EventType, LogicalReplicationStream,
    ReplicationError, ReplicationStreamConfig, RetryConfig, RowData, StreamingMode,
};
use tokio_postgres::NoTls;

use wamn_event_wire::{Envelope, Op, msg_id, stream_subjects, subject};
use wamn_registry::sql::select_event_reader_sql;

#[derive(Debug, Args)]
pub struct EventReaderArgs {
    /// Org id (the project-env must be CDC-enabled and registered).
    #[arg(long)]
    pub org: String,

    /// Project id.
    #[arg(long)]
    pub project: String,

    /// Environment slug.
    #[arg(long)]
    pub env: String,

    /// Postgres URL to the T1 system DB (`wamn_system`) — reads this
    /// project-env's `registry.event_readers` registration (SELECT only).
    #[arg(long, env = "WAMN_SYSTEM_URL")]
    pub system_database_url: String,

    /// The replication credential: the `wamn-cdc-…` Secret's `url` — a PLAIN
    /// libpq URL (no query string); the reader appends `sslmode` +
    /// `replication=database` itself.
    #[arg(long, env = "WAMN_CDC_URL")]
    pub cdc_url: String,

    /// Data-plane NATS (JetStream) the events publish to.
    #[arg(
        long,
        env = "WAMN_EVT_NATS_URL",
        default_value = "nats://evt-nats.wamn-system:4222"
    )]
    pub nats_url: String,

    /// sslmode appended to both the walsender and the preflight connection.
    #[arg(long, default_value = "disable")]
    pub sslmode: String,

    /// Replicas for the `EVT_` stream when this reader has to create it
    /// (get-or-create; an existing stream keeps its config).
    #[arg(long, default_value_t = 3)]
    pub stream_replicas: usize,

    /// JetStream duplicate window (the `Nats-Msg-Id` dedupe horizon), seconds.
    #[arg(long, default_value_t = 120)]
    pub dup_window_secs: u64,

    /// Standby-status feedback interval, seconds (how often the confirmed
    /// LSN reaches the server).
    #[arg(long, default_value_t = 5)]
    pub feedback_secs: u64,
}

/// What a session error means for the service.
#[derive(Debug, PartialEq, Eq)]
enum SessionFate {
    /// Shutdown was requested — exit cleanly.
    Cancelled,
    /// The slot is gone/invalidated: a capture GAP — die loudly (v3 §11).
    SlotIncident,
    /// Misconfiguration that a retry cannot fix.
    Fatal,
    /// Transient (connection loss, switchover) — re-open the session (F2).
    Reopen,
}

/// Classify a pg_walstream error. Invalidation keywords are checked across
/// ALL variants (the server message may surface as a connection error).
fn classify(e: &ReplicationError) -> SessionFate {
    let msg = e.to_string().to_ascii_lowercase();
    if msg.contains("invalidat") || msg.contains("can no longer be used") {
        return SessionFate::SlotIncident;
    }
    match e {
        ReplicationError::Cancelled(_) => SessionFate::Cancelled,
        ReplicationError::Authentication(_) | ReplicationError::Config(_) => SessionFate::Fatal,
        _ => SessionFate::Reopen,
    }
}

/// `<plain url>?sslmode=…&replication=database` — the walsender connection.
/// The Secret's url is contractually plain (no query string); refuse anything
/// else rather than guess how to merge.
fn walsender_url(plain: &str, sslmode: &str) -> anyhow::Result<String> {
    if plain.contains('?') {
        bail!("the CDC url must be a plain libpq URL without a query string (R8b Secret contract)");
    }
    Ok(format!("{plain}?sslmode={sslmode}&replication=database"))
}

/// `<plain url>?sslmode=…` — the ordinary SQL connection for the preflight.
fn preflight_url(plain: &str, sslmode: &str) -> anyhow::Result<String> {
    if plain.contains('?') {
        bail!("the CDC url must be a plain libpq URL without a query string (R8b Secret contract)");
    }
    Ok(format!("{plain}?sslmode={sslmode}"))
}

/// The decode-time OID → entity-id lookup (wamn-l5i9.11): one row of the
/// `wamn_entities` map `publish-catalog`/`migrate-catalog` maintain in the
/// same transaction as the DDL. Queried by the RELATION OID, which survives
/// `ALTER TABLE RENAME` — so the resolution is timeless under catch-up (a
/// session decoding pre-rename backlog resolves identically).
fn entity_lookup_sql(schema: &str) -> String {
    format!(
        "SELECT entity_id FROM \"{}\".wamn_entities WHERE relation_oid = $1",
        schema.replace('"', "\"\"")
    )
}

/// Resolve one relation OID to its catalog entity id over a short-lived SQL
/// connection (the preflight-style credential — the CDC role's grants cover
/// the map). `Ok(None)` = unmapped (no row, or the map table does not exist —
/// an env from before wamn-l5i9.11): the event publishes with the table-name
/// fallback. A connection/query failure is a transient session error — the
/// re-open loop re-preflights and the fresh session re-resolves.
async fn resolve_entity(
    args: &EventReaderArgs,
    schema: &str,
    relation_oid: u32,
) -> Result<Option<String>, ReplicationError> {
    let url = preflight_url(&args.cdc_url, &args.sslmode)
        .map_err(|e| ReplicationError::Config(e.to_string()))?;
    let (client, conn) = tokio_postgres::connect(&url, NoTls)
        .await
        .map_err(|e| ReplicationError::TransientConnection(format!("entity-map connect: {e}")))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    match client
        .query_opt(entity_lookup_sql(schema).as_str(), &[&relation_oid])
        .await
    {
        Ok(row) => Ok(row.map(|r| r.get(0))),
        // 42P01 undefined_table: no map in this env — everything is unmapped,
        // exactly the pre-.11 behavior.
        Err(e) if e.code() == Some(&tokio_postgres::error::SqlState::UNDEFINED_TABLE) => {
            tracing::warn!(
                schema,
                "no wamn_entities map in this env — publishing unmapped"
            );
            Ok(None)
        }
        Err(e) => Err(ReplicationError::TransientConnection(format!(
            "entity-map lookup: {e}"
        ))),
    }
}

/// pgoutput text row → the envelope's column→value map. Values stay in text
/// representation (string or null); an unchanged TOAST column is ABSENT from
/// the source `RowData`, so it stays absent here (distinguishable from NULL).
fn row_to_map(row: &RowData) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::with_capacity(row.len());
    for (name, value) in row.iter() {
        let v = match value {
            ColumnValue::Null => serde_json::Value::Null,
            other => {
                serde_json::Value::String(String::from_utf8_lossy(other.as_bytes()).into_owned())
            }
        };
        map.insert(name.to_string(), v);
    }
    map
}

/// This project-env's registration, straight off `registry.event_readers`.
struct Registration {
    publication: String,
    slot: String,
    stream: String,
    enabled: bool,
}

async fn read_registration(args: &EventReaderArgs) -> anyhow::Result<Registration> {
    let (client, conn) = tokio_postgres::connect(&args.system_database_url, NoTls)
        .await
        .context("connect to the system DB (--system-database-url)")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let row = client
        .query_opt(
            select_event_reader_sql(),
            &[&args.org, &args.project, &args.env],
        )
        .await
        .context("read registry.event_readers")?
        .with_context(|| {
            format!(
                "no event-reader registration for {}/{}/{} — run enable-cdc-project-env first",
                args.org, args.project, args.env
            )
        })?;
    Ok(Registration {
        publication: row.get(0),
        slot: row.get(1),
        stream: row.get(2),
        enabled: row.get(5),
    })
}

/// Verify the slot EXISTS and is healthy over an ordinary SQL connection,
/// and log the resume position. Absent or invalidated ⇒ the v3 §11 incident.
async fn preflight_slot(args: &EventReaderArgs, slot: &str) -> anyhow::Result<()> {
    let url = preflight_url(&args.cdc_url, &args.sslmode)?;
    let (client, conn) = tokio_postgres::connect(&url, NoTls)
        .await
        .context("preflight: connect the CDC credential to the project-env DB")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let row = client
        .query_opt(
            "SELECT active, confirmed_flush_lsn::text, wal_status::text, invalidation_reason::text \
             FROM pg_replication_slots WHERE slot_name = $1",
            &[&slot],
        )
        .await
        .context("preflight: read pg_replication_slots")?;
    let Some(row) = row else {
        bail!(
            "CAPTURE GAP (slot incident): replication slot {slot} does not exist — \
             the reader never creates slots; re-enable CDC and assess the gap (v3 §11)"
        );
    };
    let active: bool = row.get(0);
    let confirmed: Option<String> = row.get(1);
    let wal_status: Option<String> = row.get(2);
    let invalidation: Option<String> = row.get(3);
    if invalidation.is_some() || wal_status.as_deref() == Some("lost") {
        bail!(
            "CAPTURE GAP (slot incident): slot {slot} invalidated \
             (wal_status={wal_status:?}, reason={invalidation:?}) — re-enable CDC and \
             assess the gap (v3 §11)"
        );
    }
    tracing::info!(
        slot,
        active,
        confirmed_flush_lsn = confirmed.as_deref().unwrap_or("-"),
        wal_status = wal_status.as_deref().unwrap_or("-"),
        "preflight: slot healthy (resume position = confirmed LSN)"
    );
    Ok(())
}

async fn open_session(
    args: &EventReaderArgs,
    reg: &Registration,
    token: CancellationToken,
) -> Result<EventStream, ReplicationError> {
    let url = walsender_url(&args.cdc_url, &args.sslmode)
        .map_err(|e| ReplicationError::Config(e.to_string()))?;
    let cfg = ReplicationStreamConfig::new(
        reg.slot.clone(),
        reg.publication.clone(),
        2,
        // Off: whole transactions, post-commit, in commit order — nothing
        // uncommitted is ever published (giant txns spill server-side).
        StreamingMode::Off,
        Duration::from_secs(args.feedback_secs),
        Duration::from_secs(30),
        Duration::from_secs(30),
        RetryConfig::default(),
    );
    let mut stream = LogicalReplicationStream::new(&url, cfg).await?;
    // No ensure_replication_slot: the preflight proved existence; creating
    // here would turn a dropped slot into a SILENT gap.
    stream.start(None).await?;
    Ok(stream.into_stream(token))
}

pub async fn run(args: EventReaderArgs) -> anyhow::Result<()> {
    let token = CancellationToken::new();
    let t = token.clone();
    // PID 1 gets no default signal disposition (the dispatcher precedent).
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        t.cancel();
    });
    run_with_token(args, token).await
}

/// The service body, cancellation injected — the live gate drives this
/// directly (abort = the crash drill, cancel = clean shutdown).
pub async fn run_with_token(args: EventReaderArgs, token: CancellationToken) -> anyhow::Result<()> {
    let reg = read_registration(&args).await?;
    if !reg.enabled {
        bail!(
            "event-reader registration for {}/{}/{} is disabled",
            args.org,
            args.project,
            args.env
        );
    }
    tracing::info!(
        publication = reg.publication,
        slot = reg.slot,
        stream = reg.stream,
        "registration loaded"
    );

    let client = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect data-plane NATS at {}", args.nats_url))?;
    let js = jetstream::new(client);
    js.get_or_create_stream(jetstream::stream::Config {
        name: reg.stream.clone(),
        subjects: vec![stream_subjects(&args.org, &args.env)],
        storage: jetstream::stream::StorageType::File,
        num_replicas: args.stream_replicas,
        retention: jetstream::stream::RetentionPolicy::Limits,
        duplicate_window: Duration::from_secs(args.dup_window_secs),
        ..Default::default()
    })
    .await
    .map_err(|e| anyhow::anyhow!("get-or-create stream {}: {e}", reg.stream))?;

    let mut reopens: u64 = 0;
    let mut consecutive_failures: u32 = 0;
    loop {
        if token.is_cancelled() {
            return Ok(());
        }
        // Absent/invalidated slot = incident — checked before EVERY session
        // so a slot dropped mid-life is caught on the re-open path too.
        preflight_slot(&args, &reg.slot).await?;

        let mut stream = match open_session(&args, &reg, token.clone()).await {
            Ok(s) => s,
            Err(e) => match classify(&e) {
                SessionFate::Cancelled => return Ok(()),
                SessionFate::SlotIncident => {
                    bail!("CAPTURE GAP (slot incident) opening the session: {e} (v3 §11)")
                }
                SessionFate::Fatal => return Err(anyhow::anyhow!(e).context("open session")),
                SessionFate::Reopen => {
                    consecutive_failures += 1;
                    if consecutive_failures >= 10 {
                        bail!("session open failed {consecutive_failures}x in a row: {e}");
                    }
                    tracing::warn!(error = %e, consecutive_failures, "session open failed; retrying");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            },
        };
        consecutive_failures = 0;
        tracing::info!(reopens, "walsender session open; draining");

        match drain(&mut stream, &args, &token, &js).await {
            Ok(()) => {
                let _ = stream.shutdown().await;
                tracing::info!(reopens, "shutdown requested; exiting cleanly");
                return Ok(());
            }
            Err(e) => {
                let _ = stream.shutdown().await;
                if token.is_cancelled() {
                    return Ok(());
                }
                match classify(&e) {
                    SessionFate::Cancelled => return Ok(()),
                    SessionFate::SlotIncident => {
                        bail!("CAPTURE GAP (slot incident) mid-stream: {e} (v3 §11)")
                    }
                    SessionFate::Fatal => return Err(anyhow::anyhow!(e).context("drain")),
                    SessionFate::Reopen => {
                        reopens += 1;
                        tracing::warn!(error = %e, reopens, "stream severed; re-opening the session");
                    }
                }
            }
        }
    }
}

/// One transaction frame, stamped onto its row envelopes.
struct Txn {
    txid: u32,
    commit_ts: chrono::DateTime<chrono::Utc>,
}

/// Drain the session until cancelled (`Ok`) or severed (`Err`). Publishes row
/// events sequentially (ack-awaited), advances the feedback LSN only at
/// `Commit` — and only once every row of the txn is acked.
async fn drain(
    stream: &mut EventStream,
    args: &EventReaderArgs,
    token: &CancellationToken,
    js: &jetstream::Context,
) -> Result<(), ReplicationError> {
    let mut txn: Option<Txn> = None;
    let mut published: u64 = 0;
    let mut deduped: u64 = 0;
    // The per-SESSION OID → entity-id cache (wamn-l5i9.11): resolved lazily at
    // a relation's first row event, NEVER invalidated mid-session — pg_class
    // OIDs survive renames, so a cached resolution stays correct by
    // construction (asserted by the live gate's rename drill). A fresh session
    // re-resolves from the map.
    let mut entities: HashMap<u32, Option<String>> = HashMap::new();
    loop {
        let ev = match stream.next_event().await {
            Ok(ev) => ev,
            Err(ReplicationError::Cancelled(_)) => {
                tracing::info!(published, deduped, "drain summary");
                return Ok(());
            }
            Err(e) => {
                tracing::info!(published, deduped, "drain summary (severed)");
                return Err(e);
            }
        };
        let lsn = ev.lsn.value();
        let (op, old, new, schema, table, relation_oid) = match ev.event_type {
            EventType::Begin {
                transaction_id,
                commit_timestamp,
                ..
            } => {
                txn = Some(Txn {
                    txid: transaction_id,
                    commit_ts: commit_timestamp,
                });
                continue;
            }
            EventType::Commit { end_lsn, .. } => {
                txn = None;
                // Every row of this txn is acked (sequential awaits above) —
                // NOW the confirmed LSN may advance past the commit.
                let l = end_lsn.value();
                stream.update_flushed_lsn(l);
                stream.update_applied_lsn(l);
                continue;
            }
            EventType::Insert {
                schema,
                table,
                relation_oid,
                data,
            } => (Op::Insert, None, Some(data), schema, table, relation_oid),
            EventType::Update {
                schema,
                table,
                relation_oid,
                old_data,
                new_data,
                ..
            } => (
                Op::Update,
                old_data,
                Some(new_data),
                schema,
                table,
                relation_oid,
            ),
            EventType::Delete {
                schema,
                table,
                relation_oid,
                old_data,
                ..
            } => (
                Op::Delete,
                Some(old_data),
                None,
                schema,
                table,
                relation_oid,
            ),
            EventType::Truncate(tables) => {
                // Not part of the event plane (v3 ops are insert/update/delete).
                tracing::warn!(?tables, "TRUNCATE observed — not published");
                continue;
            }
            // Metadata frames; nothing to publish, nothing to advance.
            EventType::Relation { .. }
            | EventType::Type { .. }
            | EventType::Origin { .. }
            | EventType::Message { .. } => continue,
            other => {
                // Streaming/two-phase frames can't occur (Off/off) — a
                // protocol surprise is worth a loud log, not a crash.
                tracing::warn!(?other, "unexpected replication frame — skipped");
                continue;
            }
        };
        let Some(frame) = txn.as_ref() else {
            return Err(ReplicationError::Protocol(
                "row event outside a Begin/Commit frame".into(),
            ));
        };
        // First row event of a relation this session: resolve its entity id
        // from the map (by OID — rename-proof). `None` is cached too, so an
        // unmapped table costs one lookup per session, not one per event.
        if !entities.contains_key(&relation_oid) {
            let resolved = resolve_entity(args, &schema, relation_oid).await?;
            tracing::info!(
                %table,
                relation_oid,
                entity = resolved.as_deref().unwrap_or("(unmapped)"),
                "entity resolved"
            );
            entities.insert(relation_oid, resolved);
        }
        let entity = entities.get(&relation_oid).cloned().flatten();
        let envelope = Envelope {
            op,
            old: old.as_ref().map(row_to_map),
            new: new.as_ref().map(row_to_map),
            entity,
            table: table.to_string(),
            lsn,
            txid: frame.txid,
            commit_ts: frame.commit_ts,
            causation: None, // stitching = wamn-l5i9.12
        };
        let subj = subject(
            &args.org,
            &args.project,
            &args.env,
            envelope.entity_segment(),
            op,
        );
        let id = msg_id(&args.project, &args.env, lsn);
        let payload = serde_json::to_vec(&envelope)
            .map_err(|e| ReplicationError::Generic(format!("serialize envelope: {e}")))?;
        match publish_acked(js, token, &subj, &id, payload).await {
            PublishOutcome::Acked { duplicate } => {
                published += 1;
                if duplicate {
                    deduped += 1;
                    tracing::debug!(id, "redelivery deduped by the stream");
                }
            }
            PublishOutcome::CancelledMidRetry => {
                tracing::info!(published, deduped, "drain summary (cancelled mid-publish)");
                return Ok(());
            }
        }
    }
}

enum PublishOutcome {
    Acked { duplicate: bool },
    CancelledMidRetry,
}

/// Publish one event and wait for the JetStream ack — retrying FOREVER
/// (bounded only by shutdown). JetStream down ⇒ we hold here ⇒ the LSN holds
/// ⇒ WAL is retained: delayed, never lost.
async fn publish_acked(
    js: &jetstream::Context,
    token: &CancellationToken,
    subject: &str,
    id: &str,
    payload: Vec<u8>,
) -> PublishOutcome {
    let payload = bytes::Bytes::from(payload);
    let mut delay = Duration::from_millis(500);
    loop {
        if token.is_cancelled() {
            return PublishOutcome::CancelledMidRetry;
        }
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, id);
        let attempt = async {
            js.publish_with_headers(subject.to_string(), headers, payload.clone())
                .await
                .map_err(|e| anyhow::anyhow!("publish: {e}"))?
                .await
                .map_err(|e| anyhow::anyhow!("ack: {e}"))
        };
        match attempt.await {
            Ok(ack) => {
                return PublishOutcome::Acked {
                    duplicate: ack.duplicate,
                };
            }
            Err(e) => {
                tracing::warn!(subject, id, error = %e, "publish unacked — holding the LSN; retrying");
                tokio::select! {
                    _ = token.cancelled() => return PublishOutcome::CancelledMidRetry,
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = (delay * 2).min(Duration::from_secs(10));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walsender_url_appends_replication_database() {
        assert_eq!(
            walsender_url("postgres://u:p@h:5432/db", "disable").unwrap(),
            "postgres://u:p@h:5432/db?sslmode=disable&replication=database"
        );
        // The R8b Secret contract: a plain URL only.
        assert!(walsender_url("postgres://h/db?sslmode=require", "disable").is_err());
        assert_eq!(
            preflight_url("postgres://u:p@h:5432/db", "require").unwrap(),
            "postgres://u:p@h:5432/db?sslmode=require"
        );
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
            SlotIncident
        );
        assert_eq!(
            classify(&ReplicationError::Generic(
                "slot can no longer be used".into()
            )),
            SlotIncident
        );
    }

    #[test]
    fn entity_lookup_is_by_relation_oid_in_the_event_schema() {
        // The pinned lookup: OID-keyed (rename-proof, timeless under
        // catch-up), qualified by the EVENT's schema — the map lives beside
        // the tables it describes, so no registry column is needed.
        assert_eq!(
            entity_lookup_sql("app"),
            "SELECT entity_id FROM \"app\".wamn_entities WHERE relation_oid = $1"
        );
        // pgoutput schema names are server-provided — quote-safe embedding.
        assert_eq!(
            entity_lookup_sql("we\"ird"),
            "SELECT entity_id FROM \"we\"\"ird\".wamn_entities WHERE relation_oid = $1"
        );
    }

    #[test]
    fn row_map_keeps_null_and_absence_distinct() {
        let row = RowData::from_pairs(vec![
            ("id", ColumnValue::text("7")),
            ("note", ColumnValue::Null),
            // "big" (unchanged TOAST) is ABSENT from the pgoutput row.
        ]);
        let map = row_to_map(&row);
        assert_eq!(map.get("id").unwrap(), "7");
        assert!(map.get("note").unwrap().is_null());
        assert!(map.get("big").is_none());
    }
}
