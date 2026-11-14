//! Stream committed PostgreSQL row changes to the environment event stream.
//! A transaction advances only after every broker acknowledgement settles.
//! Lost slots end the process. Recovery repairs the slot and restarts the reader.

use sqlx::Row as _;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use async_nats::header::{HeaderMap, NATS_EXPECTED_STREAM, NATS_MESSAGE_ID};
use async_nats::jetstream;
use pg_walstream::{
    CancellationToken, ColumnValue, EventStream, EventType, LogicalReplicationStream,
    ReplicationError, ReplicationStreamConfig, RetryConfig, RowData, StreamingMode,
};

use postgres_protocol::escape::escape_identifier;
use wamn_catalog::manifest::CONTROL_OWNED_RELATION_TABLES;
use wamn_event_wire::{Causation, CdcIdentity, Envelope, Op, msg_id, subject};

use wamn_events::reader::ReaderConfig;

/// What a session error means for the service.
#[derive(Debug, PartialEq, Eq)]
enum SessionFate {
    /// Shutdown was requested — exit cleanly.
    Cancelled,
    /// The slot is missing or invalidated. Capture requires operator recovery.
    SlotIncident,
    /// Misconfiguration that a retry cannot fix.
    Fatal,
    /// Transient (connection loss, switchover) — re-open the session (F2).
    Reopen,
}

/// Classify a pg_walstream error. Invalidation keywords are validated across
/// ALL variants (the server message may surface as a connection error).
fn classify(e: &ReplicationError) -> SessionFate {
    let msg = e.to_string().to_ascii_lowercase();
    if msg.contains("invalidat")
        || msg.contains("can no longer be used")
        || (msg.contains("replication slot") && msg.contains("does not exist"))
    {
        return SessionFate::SlotIncident;
    }
    match e {
        ReplicationError::Cancelled(_) => SessionFate::Cancelled,
        ReplicationError::ReplicationSlot(_) => SessionFate::SlotIncident,
        ReplicationError::Authentication(_) | ReplicationError::Config(_) => SessionFate::Fatal,
        _ => SessionFate::Reopen,
    }
}

/// Shared reopen backoff + cap ladder (R11). BOTH the open-failure arm and the
/// drain-sever arm feed this ONE ladder, so a session that opens cleanly then
/// severs immediately can no longer hot-loop connect→sever as fast as
/// Postgres answers: every re-open is a bounded backoff, and the cap trips two
/// independent ways.
///
/// - `consecutive_failures` is the FAST-flap guard. It resets ONLY when a drain
///   committed a transaction (`note_reopen(_, commits > 0)` — productivity, not
///   open success), so an open-then-sever session that never commits keeps
///   incrementing it until `max_consecutive` trips.
/// - `window_reopens` is the SLOW-flap guard. EVERY re-open (productive or not)
///   is timestamped; more than `rate_cap` inside `rate_window` trips even when
///   each session commits once and thus keeps clearing the streak.
struct ReopenLadder {
    /// Consecutive re-opens with no committed transaction between them.
    consecutive_failures: u32,
    /// Every re-open ever taken (the `reopens` gauge — E2).
    total_reopens: u64,
    /// Re-open instants inside the trailing window, oldest first.
    window_reopens: VecDeque<Instant>,
    max_consecutive: u32,
    rate_window: Duration,
    rate_cap: usize,
    base_backoff: Duration,
    max_backoff: Duration,
}

/// The ladder's verdict for one re-open.
enum LadderStep {
    /// Sleep this long, then re-open.
    Backoff(Duration),
    /// The cap tripped — terminate the reader with this reason.
    Trip(String),
}

impl ReopenLadder {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            total_reopens: 0,
            window_reopens: VecDeque::new(),
            // Bails at 10 in a row (the pre-R11 open-path cap), now measuring
            // productivity rather than open success.
            max_consecutive: 10,
            // >20 re-opens inside a rolling minute is a sustained flap even if
            // each one committed and cleared the streak.
            rate_window: Duration::from_secs(60),
            rate_cap: 20,
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }

    fn reopens(&self) -> u64 {
        self.total_reopens
    }

    fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Record a re-open of a session that produced `commits` committed
    /// transactions, and return the next step: a bounded backoff, or a trip
    /// that must terminate the reader.
    fn note_reopen(&mut self, now: Instant, commits: u64) -> LadderStep {
        self.total_reopens += 1;
        if commits > 0 {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures += 1;
        }
        self.window_reopens.push_back(now);
        while let Some(&front) = self.window_reopens.front() {
            if now.saturating_duration_since(front) > self.rate_window {
                self.window_reopens.pop_front();
            } else {
                break;
            }
        }
        if self.consecutive_failures >= self.max_consecutive {
            return LadderStep::Trip(format!(
                "{} consecutive re-opens with no committed transaction between them",
                self.consecutive_failures
            ));
        }
        if self.window_reopens.len() > self.rate_cap {
            return LadderStep::Trip(format!(
                "{} re-opens within {:?} — a sustained flap",
                self.window_reopens.len(),
                self.rate_window
            ));
        }
        // Backoff grows with the unproductive streak (a productive re-open,
        // streak 0, backs off the base minimum). Capped at `max_backoff`.
        let shift = self.consecutive_failures.saturating_sub(1).min(20);
        let backoff = self
            .base_backoff
            .saturating_mul(1u32 << shift)
            .min(self.max_backoff);
        LadderStep::Backoff(backoff)
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct EntityMapping {
    package_id: String,
    entity_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExcludedRelation {
    package_id: String,
    relation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RelationClassification {
    Entity(EntityMapping),
    Excluded(ExcludedRelation),
}

/// Typed refusal emitted before an unmapped managed relation can reach NATS.
pub const UNMAPPED_MANAGED_RELATION_REFUSAL: &str = "cdc-unmapped-managed-relation";

/// Operator remedy paired with [`UNMAPPED_MANAGED_RELATION_REFUSAL`].
pub const UNMAPPED_MANAGED_RELATION_REMEDY: &str =
    "reconcile the installed package relation classifications before resuming CDC publication";

fn remember_relation_classification(
    cache: &mut HashMap<u32, RelationClassification>,
    relation_oid: u32,
    resolved: RelationClassification,
) -> RelationClassification {
    cache.insert(relation_oid, resolved.clone());
    resolved
}

fn publishable_entity(
    classification: RelationClassification,
    excluded_counters: &mut BTreeMap<String, u64>,
) -> Option<EntityMapping> {
    match classification {
        RelationClassification::Entity(mapping) => Some(mapping),
        RelationClassification::Excluded(excluded) => {
            let identity = format!("{}::{}", excluded.package_id, excluded.relation_id);
            *excluded_counters.entry(identity).or_default() += 1;
            None
        }
    }
}

fn merge_relation_counts(committed: &mut BTreeMap<String, u64>, pending: BTreeMap<String, u64>) {
    for (identity, count) in pending {
        *committed.entry(identity).or_default() += count;
    }
}

/// Resolve one relation OID to its package-owned CDC classification over a
/// short-lived SQL connection (the CDC role's
/// grants cover both maps). An absent row or map is a non-retryable typed refusal;
/// connection/query failure remains transient and reopens the session.
async fn resolve_relation_classification(
    args: &ReaderConfig,
    schema: &str,
    relation_oid: u32,
) -> Result<RelationClassification, ReplicationError> {
    let url = Ok::<_, anyhow::Error>(args.cdc_url.clone())
        .map_err(|e| ReplicationError::Config(e.to_string()))?;
    let mut client = wamn_postgres::connect(&url)
        .await
        .map_err(|e| ReplicationError::TransientConnection(format!("relation-map connect: {e}")))?;

    match sqlx::query(sqlx::AssertSqlSafe(
        (relation_classification_lookup_sql(schema).as_str()).to_string(),
    ))
    .bind(sqlx::postgres::types::Oid(relation_oid))
    .fetch_one(&mut client)
    .await
    {
        Ok(row) => {
            let entity_package = row.get::<Option<String>, _>("entity_package_id");
            let entity_id = row.get::<Option<String>, _>("entity_id");
            let excluded_package = row.get::<Option<String>, _>("excluded_package_id");
            let excluded_relation = row.get::<Option<String>, _>("relation_id");
            match (
                entity_package,
                entity_id,
                excluded_package,
                excluded_relation,
            ) {
                (Some(package_id), Some(entity_id), None, None) => {
                    Ok(RelationClassification::Entity(EntityMapping {
                        package_id,
                        entity_id,
                    }))
                }
                (None, None, Some(package_id), Some(excluded_id)) => {
                    Ok(RelationClassification::Excluded(ExcludedRelation {
                        package_id,
                        relation_id: excluded_id,
                    }))
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
/// representation (string or null); an unchanged TOAST column is ABSENT from
/// the source `RowData`, so it stays absent here (distinguishable from NULL).
///
/// The v3 wire carries each value as a JSON string, so it must be valid UTF-8.
/// A non-UTF-8 value means the source database is not UTF-8-encoded — refuse it
/// LOUDLY (a `Config` error, classified `Fatal`: non-retryable, the reader
/// exits and the slot holds WAL) rather than corrupt it with a lossy `U+FFFD`
/// substitution (R19). Postgres validates text on input, so a UTF-8 source
/// never reaches this path; hitting it is a misconfiguration a human must fix.
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

/// Decode a logical-decoding `Message` frame into a causation stamp, or `None`
/// when it isn't our contract (wamn-l5i9.12). Two conditions: the frame must be
/// **transactional** (`flags & 1`) — the unforgeable property rides on the
/// commit, so a rolled-back txn's message never reaches us and a
/// non-transactional emit is deliberately ignored — and the prefix must be
/// exactly `wamn.causation` (the plugin's own emit is the only legitimate
/// writer). A payload that doesn't parse as a `Causation` (malformed, or
/// smuggling extra fields past `deny_unknown_fields`) is logged and dropped,
/// never a crash — a bad message must not sever the session.
fn parse_causation(flags: u8, prefix: &str, content: &[u8]) -> Option<Causation> {
    if flags & 1 == 0 || prefix != "wamn.causation" {
        return None;
    }
    match serde_json::from_slice::<Causation>(content) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %e, "wamn.causation message did not parse as Causation — ignoring");
            None
        }
    }
}

/// Structured event emitted before a reader exits on a lost slot.
pub const CAPTURE_GAP_EVENT: &str = "CDC_CAPTURE_GAP";
/// A received TRUNCATE is reported and skipped.
pub const TRUNCATE_INCIDENT_EVENT: &str = "CDC_TRUNCATE_RECEIVED";

struct Session {
    stream: EventStream,
    identity: pg_walstream::ServerIdentity,
    history: Vec<pg_walstream::TimelineSwitch>,
}

async fn open_session(
    args: &ReaderConfig,
    token: CancellationToken,
) -> Result<Session, ReplicationError> {
    let url =
        replication_url(&args.cdc_url).map_err(|e| ReplicationError::Config(e.to_string()))?;
    let cfg = ReplicationStreamConfig::new(
        args.slot.clone(),
        args.publication.clone(),
        2,
        // Off: whole transactions, post-commit, in commit order — nothing
        // uncommitted is ever published (giant txns spill server-side).
        StreamingMode::Off,
        Duration::from_secs(args.feedback_secs),
        Duration::from_secs(30),
        Duration::from_secs(30),
        RetryConfig::default(),
    )
    // Deliver logical-decoding Message frames (the `wamn.causation` stamp the
    // wamn:postgres plugin emits per run-owned txn — wamn-l5i9.12): off by
    // default, so the reader must opt in for `drain` to see them.
    .with_messages(true);
    let mut stream = LogicalReplicationStream::new(&url, cfg).await?;
    let identity = *stream.server_identity();
    let history = stream.timeline_history().await?;
    // Provisioning alone creates slots. A missing slot must stop capture.
    stream.start(None).await?;
    Ok(Session {
        stream: stream.into_stream(token),
        identity,
        history,
    })
}

/// The reader's own shutdown handle. It holds the replication adapter's
/// cancellation type inside this crate, so the public entry point names no type
/// from `pg_walstream` (wamn-0h0g.19.19). Clone it to hold the handle in more
/// than one place. Every clone controls the same reader.
#[derive(Clone, Debug, Default)]
pub struct ReaderShutdown {
    token: CancellationToken,
}

impl ReaderShutdown {
    /// Create a handle that has not asked for shutdown yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the reader to stop. The reader finishes nothing new, drops the
    /// walsender session, and returns `Ok(())`.
    pub fn shutdown(&self) {
        self.token.cancel();
    }

    /// Consume the handle into a guard that asks for shutdown when it drops.
    /// A caller holds one so an early return or an unwind still stops the
    /// reader.
    #[must_use]
    pub fn into_guard(self) -> ReaderShutdownGuard {
        ReaderShutdownGuard { shutdown: self }
    }
}

/// Asks the reader to stop when it drops. [`ReaderShutdown::into_guard`] builds
/// one.
#[derive(Debug)]
pub struct ReaderShutdownGuard {
    shutdown: ReaderShutdown,
}

impl Drop for ReaderShutdownGuard {
    fn drop(&mut self) {
        self.shutdown.shutdown();
    }
}

pub async fn run(args: ReaderConfig) -> anyhow::Result<()> {
    let shutdown = ReaderShutdown::new();
    let signalled = shutdown.clone();
    // PID 1 gets no default signal disposition (the dispatcher precedent).
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        signalled.shutdown();
    });
    run_with_shutdown(args, shutdown).await
}

/// The service body, shutdown injected — the live acceptance test drives this directly
/// (abort = the crash drill, shutdown = clean stop).
pub async fn run_with_shutdown(
    mut args: ReaderConfig,
    shutdown: ReaderShutdown,
) -> anyhow::Result<()> {
    use sqlx::ConnectOptions as _;
    use std::io::Write as _;
    let options = wamn_postgres::connect_options(&args.cdc_url).await?;
    let mut input = options.to_url_lossy();
    let ca = input
        .query_pairs()
        .find(|(name, _)| name == "sslrootcert")
        .map(|(_, value)| value.into_owned());
    let mut certificate = tempfile::NamedTempFile::new()?;
    if let Some(ca) = ca.filter(|ca| ca.starts_with("-----BEGIN CERTIFICATE-----")) {
        certificate.write_all(ca.as_bytes())?;
        let pairs: Vec<_> = input
            .query_pairs()
            .filter(|(name, _)| name != "sslrootcert")
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        input
            .query_pairs_mut()
            .clear()
            .extend_pairs(pairs)
            .append_pair("sslrootcert", &certificate.path().to_string_lossy());
    }
    args.cdc_url = input.into();
    let token = shutdown.token;
    let nats_options = wamn_broker::nats::tls_options(
        async_nats::ConnectOptions::new(),
        args.nats_tls_ca.as_deref(),
        args.nats_tls_cert.as_deref(),
        args.nats_tls_key.as_deref(),
        None,
    )?;
    let client = wamn_broker::nats::connect(
        nats_options,
        &args.nats_url,
        wamn_broker::nats::InitialConnect::Once,
    )
    .await?;
    let js = jetstream::new(client);

    // `confirmed_lsn_age_seconds` gauge (E2): millis of the last confirmed-LSN
    // advance, seeded at start so a reader that never commits still ages. Shared
    // with the detached slot-headroom monitor.
    let last_lsn_advance_ms = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_millis()));
    let monitor = token.child_token();
    spawn_slot_monitor(
        &args,
        args.slot.clone(),
        monitor.clone(),
        Arc::clone(&last_lsn_advance_ms),
    );
    // Every exit also stops the detached monitor.
    let _monitor_shutdown = ReaderShutdown { token: monitor }.into_guard();

    let mut ladder = ReopenLadder::new();
    loop {
        if token.is_cancelled() {
            return Ok(());
        }
        let mut session = match open_session(&args, token.child_token()).await {
            Ok(s) => s,
            Err(e) => match classify(&e) {
                SessionFate::Cancelled => return Ok(()),
                SessionFate::SlotIncident => return Err(capture_failure(&args.slot, e)),
                SessionFate::Fatal => return Err(anyhow::anyhow!(e).context("open session")),
                SessionFate::Reopen => {
                    // An open that never produced a commit is unproductive by
                    // definition — the SAME ladder the drain-sever arm feeds.
                    ladder_step_or_bail(&mut ladder, 0, &e, &token).await?;
                    continue;
                }
            },
        };
        tracing::info!(
            reopens = ladder.reopens(),
            "walsender session open; draining"
        );

        match drain(&mut session, &args, &token, &js, &last_lsn_advance_ms).await {
            DrainOutcome::Shutdown(summary) => {
                log_drain_summary(&summary, "shutdown");
                let _ = session.stream.shutdown().await;
                tracing::info!(
                    reopens = ladder.reopens(),
                    commits = summary.commits,
                    "shutdown requested; exiting cleanly"
                );
                return Ok(());
            }
            DrainOutcome::Severed(e, summary) => {
                log_drain_summary(&summary, "severed");
                // `EventStream::shutdown` cancels the token it was constructed
                // with. Snapshot caller intent first so cleanup cannot turn a
                // fatal decode refusal into a successful shutdown.
                let cancellation_requested = token.is_cancelled();
                let _ = session.stream.shutdown().await;
                if cancellation_requested {
                    return Ok(());
                }
                match classify(&e) {
                    SessionFate::Cancelled => return Ok(()),
                    SessionFate::SlotIncident => return Err(capture_failure(&args.slot, e)),
                    SessionFate::Fatal => return Err(anyhow::anyhow!(e).context("drain")),
                    SessionFate::Reopen => {
                        // Productivity — not open success — resets the streak:
                        // only a drain that committed clears the fast-flap cap.
                        ladder_step_or_bail(&mut ladder, summary.commits, &e, &token).await?;
                    }
                }
            }
        }
    }
}

fn capture_failure(slot: &str, error: ReplicationError) -> anyhow::Error {
    tracing::error!(target: "wamn::event_reader", event = CAPTURE_GAP_EVENT, slot,
        %error, "capture stopped; recover the slot and restart the reader");
    anyhow::anyhow!(error).context("CDC capture gap")
}

/// Feed one re-open into the shared ladder and act on its verdict: bail if the
/// cap tripped (R11 — nonzero exit), else back off the returned interval
/// (interruptible by shutdown) before the loop re-opens.
async fn ladder_step_or_bail(
    ladder: &mut ReopenLadder,
    commits: u64,
    e: &ReplicationError,
    token: &CancellationToken,
) -> anyhow::Result<()> {
    match ladder.note_reopen(Instant::now(), commits) {
        LadderStep::Trip(reason) => {
            bail!("reader reopen cap tripped ({reason}); last error: {e}")
        }
        LadderStep::Backoff(delay) => {
            tracing::warn!(
                error = %e,
                reopens = ladder.reopens(),
                consecutive_failures = ladder.consecutive_failures(),
                backoff_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                "session severed/failed; backing off before re-open"
            );
            tokio::select! {
                () = token.cancelled() => {}
                () = tokio::time::sleep(delay) => {}
            }
            Ok(())
        }
    }
}

/// One transaction, held until its `Commit`: the metadata every row shares,
/// the causation stamp (if the `wamn:postgres` plugin emitted one —
/// wamn-l5i9.12), and the row events BUFFERED until the commit.
///
/// Buffer-per-txn is what makes causation robust to frame order: a
/// transactional `wamn.causation` message rides the stream at its own LSN
/// within `Begin`..`Commit` and may arrive BEFORE or AFTER a row event, so
/// nothing can be published as it arrives — the whole txn is collected and
/// every row publishes at `Commit` with the stamp (if any) attached. The
/// confirmed LSN still advances only after every row of the txn is acked.
struct Txn {
    txid: u32,
    commit_ts: chrono::DateTime<chrono::Utc>,
    causation: Option<Causation>,
    rows: Vec<PendingRow>,
    next_ordinal: u64,
    excluded_by_relation: BTreeMap<String, u64>,
}

/// A decoded row event awaiting its transaction's `Commit`. Entity resolution
/// happens at arrival (keeping the per-session OID cache warm); only the
/// causation stamp and the publish are deferred to the commit.
struct PendingRow {
    op: Op,
    old: Option<serde_json::Map<String, serde_json::Value>>,
    new: Option<serde_json::Map<String, serde_json::Value>>,
    entity_mapping: EntityMapping,
    table: String,
    ordinal: u64,
}

/// What a drained session produced. `commits` is the R11 productivity signal —
/// the shared ladder resets the fast-flap streak only when a session committed
/// at least one transaction, so an open-then-sever session that never commits
/// keeps counting toward the cap.
struct DrainSummary {
    commits: u64,
    published: u64,
    deduped: u64,
    /// TRUNCATE frames received this session. The declared publication carries
    /// none, so any count above zero is an operational incident.
    truncates: u64,
    excluded_by_relation: BTreeMap<String, u64>,
}

fn log_drain_summary(summary: &DrainSummary, ending: &str) {
    tracing::info!(
        target: "wamn::event_reader",
        commits = summary.commits,
        events_published = summary.published,
        deduped = summary.deduped,
        truncates = summary.truncates,
        excluded_by_relation = ?summary.excluded_by_relation,
        ending,
        "drain summary"
    );
}

/// How a drain ended: either shutdown was requested (exit cleanly) or the
/// stream severed (re-open per the ladder). BOTH carry the `DrainSummary`, so
/// the caller always knows whether the ended session was productive.
enum DrainOutcome {
    Shutdown(DrainSummary),
    Severed(ReplicationError, DrainSummary),
}

/// Drain the session until cancelled (`Shutdown`) or severed (`Severed`).
/// Buffers each transaction's row events (resolving entities as they arrive)
/// and captures its `wamn.causation` message whenever it lands; at `Commit`,
/// PIPELINES the whole transaction (E1): each buffered row is published without
/// awaiting its server ack, the ack futures are held in publish order, and ALL
/// of them are settled before the feedback LSN advances — the v3 §4 invariant
/// (LSN advances only on ack, per transaction) preserved exactly, with one
/// round trip amortized over the txn instead of one per row. Returns the
/// `DrainSummary` in both outcomes.
async fn drain(
    session: &mut Session,
    args: &ReaderConfig,
    token: &CancellationToken,
    js: &jetstream::Context,
    last_lsn_advance_ms: &AtomicI64,
) -> DrainOutcome {
    let stream = &mut session.stream;
    let mut txn: Option<Txn> = None;
    let mut summary = DrainSummary {
        commits: 0,
        published: 0,
        deduped: 0,
        truncates: 0,
        excluded_by_relation: BTreeMap::new(),
    };
    // The per-session OID cache stores the relation's closed classification.
    // OIDs survive renames, so a resolution stays correct for the session.
    let mut relation_classifications: HashMap<u32, RelationClassification> = HashMap::new();
    loop {
        let ev = match stream.next_event().await {
            Ok(ev) => ev,
            Err(ReplicationError::Cancelled(_)) => return DrainOutcome::Shutdown(summary),
            Err(e) => return DrainOutcome::Severed(e, summary),
        };
        let (op, old, new, schema, table, relation_oid) = match ev.event_type {
            EventType::Begin {
                transaction_id,
                commit_timestamp,
                ..
            } => {
                txn = Some(Txn {
                    txid: transaction_id,
                    commit_ts: commit_timestamp,
                    causation: None,
                    rows: Vec::new(),
                    next_ordinal: 0,
                    excluded_by_relation: BTreeMap::new(),
                });
                continue;
            }
            EventType::Commit {
                commit_lsn,
                end_lsn,
                ..
            } => {
                let Some(frame) = txn.take() else {
                    return DrainOutcome::Severed(
                        ReplicationError::Protocol("Commit outside a Begin frame".into()),
                        summary,
                    );
                };
                // Buffer-per-txn: build the whole transaction's wire messages
                // NOW, the causation stamp (if the plugin emitted one) attached
                // to every row — robust to whether the message arrived before or
                // after these rows.
                let mut msgs = Vec::with_capacity(frame.rows.len());
                let timeline = session
                    .history
                    .iter()
                    .find(|ancestor| commit_lsn < ancestor.switchpoint())
                    .map_or(
                        session.identity.timeline(),
                        pg_walstream::TimelineSwitch::timeline,
                    );
                for row in frame.rows {
                    let envelope = Envelope {
                        op: row.op,
                        old: row.old,
                        new: row.new,
                        package_id: row.entity_mapping.package_id,
                        entity: row.entity_mapping.entity_id,
                        table: row.table,
                        identity: CdcIdentity {
                            system_id: session.identity.system_id(),
                            timeline,
                            commit_lsn: commit_lsn.value(),
                            row_ordinal: row.ordinal,
                        },
                        txid: frame.txid,
                        commit_ts: frame.commit_ts,
                        causation: frame.causation.clone(),
                    };
                    let subject = subject(
                        &args.org,
                        &args.project,
                        &args.env,
                        envelope.entity_segment(),
                        row.op,
                    );
                    let id = msg_id(&args.project, &args.env, &envelope.identity);
                    let payload =
                        match serde_json::to_vec(&wamn_event_wire::SourceEvent::Cdc(envelope)) {
                            Ok(p) => bytes::Bytes::from(p),
                            Err(e) => {
                                return DrainOutcome::Severed(
                                    ReplicationError::Io(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        e,
                                    )),
                                    summary,
                                );
                            }
                        };
                    msgs.push(PreparedMsg {
                        subject,
                        id,
                        payload,
                    });
                }
                // E1: pipeline the txn's publishes — sent without awaiting, the
                // ack futures held in publish order, ALL settled here before the
                // LSN advances (the v3 §4 per-txn invariant, unchanged).
                let has_exclusions = !frame.excluded_by_relation.is_empty();
                let stall_threshold = Duration::from_secs(args.stall_threshold_secs);
                let mut tally = PublishTally::default();
                match publish_txn(
                    &JsPublisher {
                        js,
                        stream: &args.stream,
                    },
                    token,
                    &msgs,
                    MAX_IN_FLIGHT,
                    stall_threshold,
                    &mut tally,
                )
                .await
                {
                    PublishOutcome::Acked => {
                        summary.published += tally.published;
                        summary.deduped += tally.deduped;
                        merge_relation_counts(
                            &mut summary.excluded_by_relation,
                            frame.excluded_by_relation,
                        );
                    }
                    PublishOutcome::CancelledMidRetry => return DrainOutcome::Shutdown(summary),
                }
                // Every row of this txn is acked — NOW the confirmed LSN may
                // advance past the commit.
                let l = end_lsn.value();
                stream.update_flushed_lsn(l);
                stream.update_applied_lsn(l);
                summary.commits += 1;
                if has_exclusions {
                    tracing::info!(
                        target: "wamn::event_reader",
                        excluded_by_relation = ?summary.excluded_by_relation,
                        "CDC exclusions committed"
                    );
                }
                // Feed the `confirmed_lsn_age_seconds` gauge (E2): the LSN just
                // advanced, so its age resets to ~0.
                last_lsn_advance_ms.store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
                continue;
            }
            EventType::Message {
                flags,
                prefix,
                content,
                ..
            } => {
                // The wamn:postgres causation stamp for the open txn
                // (wamn-l5i9.12). A transactional message always rides inside a
                // Begin/Commit; one outside a frame is a protocol surprise —
                // log and ignore, never crash the session.
                if let Some(c) = parse_causation(flags, &prefix, &content) {
                    if let Some(frame) = txn.as_mut() {
                        frame.causation = Some(c);
                    } else {
                        tracing::warn!(
                            "transactional wamn.causation message outside a txn frame — ignored"
                        );
                    }
                }
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
            EventType::Truncate { tables, .. } => {
                // The declared publication omits `truncate`, so a TRUNCATE
                // frame proves the publication drifted after this session
                // opened. Count it and alert; the event plane still carries
                // insert/update/delete only (wamn-0h0g.19.19).
                summary.truncates += 1;
                tracing::error!(
                    target: "wamn::event_reader",
                    event = TRUNCATE_INCIDENT_EVENT,
                    ?tables,
                    truncates = summary.truncates,
                    "TRUNCATE received from a publication that declares none — \
                     operational incident, not published"
                );
                continue;
            }
            // Metadata frames; nothing to buffer, nothing to advance.
            EventType::Relation { .. } | EventType::Type { .. } | EventType::Origin { .. } => {
                continue;
            }
            // A keepalive carries the server's sent position (wamn-59z6). With no
            // transaction open, the last returned commit is acknowledged, because
            // the Commit arm settles every ack before the loop reads on. No change
            // of this database before `wal_end` is then unpublished, so the reader
            // confirms it, and the slot of an idle database follows the WAL.
            EventType::Keepalive { wal_end } => {
                let (flushed, _) = stream.get_feedback_lsn();
                if txn.is_none() && wal_end.value() > flushed {
                    stream.update_flushed_lsn(wal_end.value());
                    stream.update_applied_lsn(wal_end.value());
                    last_lsn_advance_ms
                        .store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
                }
                continue;
            }
            other => {
                // Streaming/two-phase frames can't occur (Off/off) — a
                // protocol surprise is worth a loud log, not a crash.
                tracing::warn!(?other, "unexpected replication frame — skipped");
                continue;
            }
        };
        let Some(frame) = txn.as_mut() else {
            return DrainOutcome::Severed(
                ReplicationError::Protocol("row event outside a Begin/Commit frame".into()),
                summary,
            );
        };
        let ordinal = frame.next_ordinal;
        frame.next_ordinal += 1;
        if CONTROL_OWNED_RELATION_TABLES.contains(&table.as_ref()) {
            continue;
        }
        // Resolve an uncached relation OID before buffering anything publishable.
        let classification = if let Some(classification) =
            relation_classifications.get(&relation_oid)
        {
            classification.clone()
        } else {
            let resolved = match resolve_relation_classification(args, &schema, relation_oid).await
            {
                Ok(r) => r,
                Err(e) => return DrainOutcome::Severed(e, summary),
            };
            match &resolved {
                RelationClassification::Entity(mapping) => tracing::info!(
                    %table,
                    relation_oid,
                    package_id = mapping.package_id.as_str(),
                    entity = mapping.entity_id.as_str(),
                    "entity resolved"
                ),
                RelationClassification::Excluded(excluded) => tracing::info!(
                    %table,
                    relation_oid,
                    package_id = excluded.package_id.as_str(),
                    relation = excluded.relation_id.as_str(),
                    "CDC exclusion resolved"
                ),
            }
            remember_relation_classification(&mut relation_classifications, relation_oid, resolved)
        };
        let Some(entity_mapping) =
            publishable_entity(classification, &mut frame.excluded_by_relation)
        else {
            continue;
        };
        // Buffer the row; it publishes at Commit, once the txn's causation
        // stamp (if any) is known.
        let old = match old.as_ref().map(row_to_map).transpose() {
            Ok(m) => m,
            Err(e) => return DrainOutcome::Severed(e, summary),
        };
        let new = match new.as_ref().map(row_to_map).transpose() {
            Ok(m) => m,
            Err(e) => return DrainOutcome::Severed(e, summary),
        };
        frame.rows.push(PendingRow {
            op,
            old,
            new,
            entity_mapping,
            table: table.to_string(),
            ordinal,
        });
    }
}

/// The outcome of publishing one whole transaction (E1). `Acked` means every
/// row's server ack has settled — the LSN may advance.
enum PublishOutcome {
    Acked,
    CancelledMidRetry,
}

/// The E1 in-flight publish bound: at most this many un-acked publishes are held
/// before the pipeline settles them mid-transaction. Well under async-nats' 5000
/// default ack-inflight semaphore (so `send` never blocks on backpressure), and
/// large enough that the ack round trip amortizes across a whole transaction.
const MAX_IN_FLIGHT: usize = 256;

/// One row event's wire form, prepared once and re-published as-is on a retry
/// (the `Nats-Msg-Id` in `id` is what the JetStream duplicate window keys on).
struct PreparedMsg {
    subject: String,
    id: String,
    payload: bytes::Bytes,
}

/// The server acknowledgment for a completed publish.
/// A buffered message can remain unsent while the client is disconnected.
/// `duplicate` states whether JetStream already accepted this `Nats-Msg-Id`.
struct PublishAcknowledgment {
    duplicate: bool,
}

/// Running publish/dedupe counts for one transaction, folded into the
/// `DrainSummary` once the whole txn has settled.
#[derive(Default)]
struct PublishTally {
    published: u64,
    deduped: u64,
}

/// Publish messages and await the broker acknowledgements.
struct JsPublisher<'a> {
    js: &'a jetstream::Context,
    stream: &'a str,
}

impl JsPublisher<'_> {
    async fn send(
        &self,
        msg: &PreparedMsg,
    ) -> anyhow::Result<jetstream::context::PublishAckFuture> {
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, msg.id.as_str());
        headers.insert(NATS_EXPECTED_STREAM, self.stream);
        self.js
            .publish_with_headers(msg.subject.clone(), headers, msg.payload.clone())
            .await
            .map_err(|e| anyhow::anyhow!("publish: {e}"))
    }

    async fn settle(
        &self,
        ack: jetstream::context::PublishAckFuture,
    ) -> anyhow::Result<PublishAcknowledgment> {
        let ack = ack.await.map_err(|e| anyhow::anyhow!("ack: {e}"))?;
        Ok(PublishAcknowledgment {
            duplicate: ack.duplicate,
        })
    }
}

/// Where one publish's retry sits relative to the stall threshold (E2).
#[derive(Debug, PartialEq, Eq)]
enum StallLevel {
    /// Under the threshold — an ordinary retry warn.
    Retrying,
    /// At or past the threshold — emit the distinct `CDC_PUBLISH_STALLED` alert.
    Stalled,
}

/// Classify how long a single publish has been holding the LSN. At/past the
/// threshold the stall is a first-class alert; below it, an ordinary retry.
fn stall_level(stall: Duration, threshold: Duration) -> StallLevel {
    if stall >= threshold {
        StallLevel::Stalled
    } else {
        StallLevel::Retrying
    }
}

/// Slot WAL-retention headroom, derived from `pg_replication_slots` (E2
/// backstop). The ladder maps onto Postgres' own `wal_status` semantics plus a
/// `safe_wal_size` early-warning minimum so the alert fires BEFORE the status
/// leaves `reserved`.
#[derive(Debug, PartialEq, Eq)]
enum SlotHealth {
    /// `reserved` with headroom above the warn minimum.
    Healthy,
    /// Still `reserved` but `safe_wal_size` fell below the warn minimum — the
    /// early warning before the status degrades.
    HeadroomLow,
    /// `extended`: past `max_wal_size`, retained only by `max_slot_wal_keep_size`.
    Extended,
    /// `unreserved`: WAL may be removed at the next checkpoint — gap imminent.
    Unreserved,
    /// `lost`: the slot is invalidated — the v3 §11 capture-gap incident.
    Lost,
}

/// Map a slot's `wal_status` + `safe_wal_size` to a health level. A NULL/unknown
/// status is treated conservatively as `HeadroomLow` (surfaced, never silently
/// healthy). `safe_wal_size` is NULL once the status is not `reserved`, so the
/// minimum test only applies on the `reserved` branch.
fn classify_slot_health(
    wal_status: &str,
    safe_wal_size: Option<i64>,
    warn_minimum_bytes: i64,
) -> SlotHealth {
    match wal_status {
        "reserved" => match safe_wal_size {
            Some(n) if n < warn_minimum_bytes => SlotHealth::HeadroomLow,
            _ => SlotHealth::Healthy,
        },
        "extended" => SlotHealth::Extended,
        "unreserved" => SlotHealth::Unreserved,
        "lost" => SlotHealth::Lost,
        _ => SlotHealth::HeadroomLow,
    }
}

/// Seconds since the reader last advanced its confirmed LSN — the
/// `confirmed_lsn_age_seconds` gauge (E2). Clamped at 0.
fn confirmed_lsn_age_seconds(last_lsn_advance_ms: &AtomicI64) -> i64 {
    let last = last_lsn_advance_ms.load(Ordering::Relaxed);
    (chrono::Utc::now().timestamp_millis() - last).max(0) / 1000
}

/// Poll the slot's WAL-retention headroom once over a SEPARATE plain (non-
/// replication) connection — the replication connection speaks the replication
/// protocol and cannot run this query — and emit the gauge + the escalating
/// alert for its health level. Publishes `slot_safe_wal_bytes`,
/// `slot_wal_lag_bytes`, and `confirmed_lsn_age_seconds` as stable fields.
async fn poll_slot_once(
    cdc_url: &str,
    slot: &str,
    warn_minimum: i64,
    last_lsn_advance_ms: &AtomicI64,
) -> anyhow::Result<()> {
    let url = Ok::<_, anyhow::Error>(cdc_url.to_owned())?;
    let mut client = wamn_postgres::connect(&url)
        .await
        .context("slot-monitor connect")?;

    let row = sqlx::query(
        "SELECT wal_status::text, safe_wal_size, \
             (pg_current_wal_lsn() - restart_lsn)::bigint \
             FROM pg_replication_slots WHERE slot_name = $1",
    )
    .bind(slot)
    .fetch_optional(&mut client)
    .await
    .context("slot-monitor query")?;
    let confirmed_lsn_age_seconds = confirmed_lsn_age_seconds(last_lsn_advance_ms);
    let Some(row) = row else {
        tracing::error!(
            target: "wamn::event_reader",
            event = "CDC_SLOT_MISSING",
            slot,
            confirmed_lsn_age_seconds,
            "slot vanished under the reader — capture gap imminent (v3 §11)"
        );
        return Ok(());
    };
    let wal_status: Option<String> = row.get(0);
    let safe_wal_size: Option<i64> = row.get(1);
    let lag_bytes: Option<i64> = row.get(2);
    let status = wal_status.as_deref().unwrap_or("unknown");
    match classify_slot_health(status, safe_wal_size, warn_minimum) {
        SlotHealth::Healthy => tracing::info!(
            target: "wamn::event_reader", event = "cdc_slot_health", slot, wal_status = status,
            slot_safe_wal_bytes = ?safe_wal_size, slot_wal_lag_bytes = ?lag_bytes,
            confirmed_lsn_age_seconds, "slot headroom healthy"
        ),
        SlotHealth::HeadroomLow => tracing::warn!(
            target: "wamn::event_reader", event = "CDC_SLOT_WAL_LOW", slot, wal_status = status,
            slot_safe_wal_bytes = ?safe_wal_size, slot_wal_lag_bytes = ?lag_bytes,
            confirmed_lsn_age_seconds,
            "slot WAL headroom low — still reserved; act BEFORE it leaves reserved"
        ),
        SlotHealth::Extended => tracing::warn!(
            target: "wamn::event_reader", event = "CDC_SLOT_WAL_EXTENDED", slot, wal_status = status,
            slot_safe_wal_bytes = ?safe_wal_size, slot_wal_lag_bytes = ?lag_bytes,
            confirmed_lsn_age_seconds,
            "slot past max_wal_size — retained only by max_slot_wal_keep_size"
        ),
        SlotHealth::Unreserved => tracing::error!(
            target: "wamn::event_reader", event = "CDC_SLOT_WAL_UNRESERVED", slot, wal_status = status,
            slot_safe_wal_bytes = ?safe_wal_size, slot_wal_lag_bytes = ?lag_bytes,
            confirmed_lsn_age_seconds,
            "slot WAL no longer safe — invalidation imminent; fix JetStream, do NOT drop the slot (v3 §11)"
        ),
        SlotHealth::Lost => tracing::error!(
            target: "wamn::event_reader", event = "CDC_SLOT_INVALIDATED", slot, wal_status = status,
            confirmed_lsn_age_seconds,
            "slot invalidated — capture GAP (v3 §11)"
        ),
    }
    Ok(())
}

/// Spawn the slot-headroom monitor (E2 backstop): a detached loop — like the
/// SIGTERM task, it rides the process lifetime and exits on shutdown — polling
/// on its own cadence. A poll failure is transient (logged, retried); the main
/// replication session ends capture on a lost slot.
fn spawn_slot_monitor(
    args: &ReaderConfig,
    slot: String,
    token: CancellationToken,
    last_lsn_advance_ms: Arc<AtomicI64>,
) {
    if args.slot_poll_secs == 0 {
        return;
    }
    let cdc_url = args.cdc_url.clone();
    let warn_minimum = args.slot_safe_wal_warn_bytes;
    let poll = Duration::from_secs(args.slot_poll_secs);
    tokio::spawn(async move {
        loop {
            if let Err(e) =
                poll_slot_once(&cdc_url, &slot, warn_minimum, &last_lsn_advance_ms).await
            {
                tracing::warn!(
                    target: "wamn::event_reader",
                    slot = %slot,
                    error = %e,
                    "slot-headroom poll failed (transient) — will retry"
                );
            }
            tokio::select! {
                () = token.cancelled() => return,
                () = tokio::time::sleep(poll) => {}
            }
        }
    });
}

/// Publish one whole transaction's messages and settle every server ack —
/// retrying FOREVER (bounded only by shutdown). JetStream down ⇒ we hold here ⇒
/// the LSN holds ⇒ WAL is retained: delayed, never lost. But a held LSN silently
/// freezes WAL retention on the source DB, so once the retries pass
/// `stall_threshold` this escalates from ordinary warns to a distinct
/// `CDC_PUBLISH_STALLED` alert event (E2 — the interlock that makes "delayed,
/// never lost" observable). The publishes are PIPELINED (E1): sent without
/// awaiting, held in order, settled in bounded batches; the whole set is acked
/// before the caller advances the LSN.
///
/// A failed ack retries the transaction from the FIRST UNACKED row — the
/// JetStream duplicate window absorbs the already-landed prefix, and the
/// workflow's run key + `ON CONFLICT` absorbs anything past the window.
/// `first_unacked` advances only as acks settle durably, so a retry never
/// re-settles (or double-counts) a row the server already acked.
async fn publish_txn(
    publisher: &JsPublisher<'_>,
    token: &CancellationToken,
    msgs: &[PreparedMsg],
    max_in_flight: usize,
    stall_threshold: Duration,
    tally: &mut PublishTally,
) -> PublishOutcome {
    let mut first_unacked = 0usize;
    let mut delay = Duration::from_millis(500);
    let first_attempt = Instant::now();
    let mut publish_retries: u64 = 0;
    let mut stalled = false;
    loop {
        if token.is_cancelled() {
            return PublishOutcome::CancelledMidRetry;
        }
        match publish_pass(
            publisher,
            token,
            msgs,
            &mut first_unacked,
            max_in_flight,
            tally,
        )
        .await
        {
            PassOutcome::Complete => {
                if stalled {
                    tracing::info!(
                        target: "wamn::event_reader",
                        event = "CDC_PUBLISH_RECOVERED",
                        publish_retries,
                        publish_stall_seconds = first_attempt.elapsed().as_secs(),
                        "JetStream publish recovered — the LSN can advance again"
                    );
                }
                return PublishOutcome::Acked;
            }
            PassOutcome::Cancelled => return PublishOutcome::CancelledMidRetry,
            PassOutcome::Failed(e) => {
                publish_retries += 1;
                let stall = first_attempt.elapsed();
                // The retry restart point — the row alerts should bind to.
                let subject = msgs[first_unacked].subject.as_str();
                let id = msgs[first_unacked].id.as_str();
                match stall_level(stall, stall_threshold) {
                    StallLevel::Retrying => tracing::warn!(
                        target: "wamn::event_reader",
                        subject,
                        id,
                        publish_retries,
                        publish_stall_seconds = stall.as_secs(),
                        error = %e,
                        "publish unacked — holding the LSN; retrying from the first unacked row"
                    ),
                    StallLevel::Stalled => {
                        stalled = true;
                        tracing::error!(
                            target: "wamn::event_reader",
                            event = "CDC_PUBLISH_STALLED",
                            subject,
                            id,
                            publish_retries,
                            publish_stall_seconds = stall.as_secs(),
                            error = %e,
                            "JetStream publish STALLED past threshold — LSN held, WAL retention frozen on the source DB; fix JetStream, do NOT drop the slot (v3 §11)"
                        );
                    }
                }
                tokio::select! {
                    () = token.cancelled() => return PublishOutcome::CancelledMidRetry,
                    () = tokio::time::sleep(delay) => {}
                }
                delay = (delay * 2).min(Duration::from_secs(10));
            }
        }
    }
}

/// One pass of the pipeline: publish `msgs[*first_unacked..]` without awaiting,
/// holding the ack futures in publish order, draining (settling) them whenever
/// the in-flight set reaches `max_in_flight`, and settling the remainder at the
/// end. `first_unacked` advances as each ack settles durably. A `send` or
/// `settle` failure ends the pass at `Failed` (its held futures dropped — the
/// async-nats client returns their permits to the acker), leaving `first_unacked`
/// at the first row whose ack has NOT settled.
enum PassOutcome {
    Complete,
    Cancelled,
    Failed(anyhow::Error),
}

async fn publish_pass(
    publisher: &JsPublisher<'_>,
    token: &CancellationToken,
    msgs: &[PreparedMsg],
    first_unacked: &mut usize,
    max_in_flight: usize,
    tally: &mut PublishTally,
) -> PassOutcome {
    // (row index, held ack future) in publish order — index strictly ascending.
    let mut held: Vec<(usize, jetstream::context::PublishAckFuture)> = Vec::new();
    for idx in *first_unacked..msgs.len() {
        if token.is_cancelled() {
            return PassOutcome::Cancelled;
        }
        match publisher.send(&msgs[idx]).await {
            Ok(ack) => held.push((idx, ack)),
            Err(e) => return PassOutcome::Failed(e),
        }
        if held.len() >= max_in_flight {
            // In-flight bound hit mid-transaction: settle the held batch, then
            // keep publishing. The LSN hold is unaffected.
            if let Err(e) = settle_all(publisher, msgs, first_unacked, &mut held, tally).await {
                return PassOutcome::Failed(e);
            }
        }
    }
    // Settle any remaining held acks BEFORE the caller advances the LSN.
    match settle_all(publisher, msgs, first_unacked, &mut held, tally).await {
        Ok(()) => PassOutcome::Complete,
        Err(e) => PassOutcome::Failed(e),
    }
}

/// Settle held acks in publish order. Each durable ack advances `first_unacked`
/// (and the tally); the FIRST failure stops the drain and returns, dropping the
/// still-held futures — `first_unacked` is left AT the failed row so the retry
/// restarts there and never re-settles a durably-acked prefix.
async fn settle_all(
    publisher: &JsPublisher<'_>,
    msgs: &[PreparedMsg],
    first_unacked: &mut usize,
    held: &mut Vec<(usize, jetstream::context::PublishAckFuture)>,
    tally: &mut PublishTally,
) -> anyhow::Result<()> {
    for (idx, ack) in held.drain(..) {
        let acknowledgment = publisher.settle(ack).await?;
        debug_assert_eq!(idx, *first_unacked, "acks settle strictly in publish order");
        *first_unacked = idx + 1;
        tally.published += 1;
        if acknowledgment.duplicate {
            tally.deduped += 1;
            tracing::debug!(id = msgs[idx].id, "redelivery deduped by the stream");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            classify(&ReplicationError::Protocol(
                "slot can no longer be used".into()
            )),
            SlotIncident
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

    #[test]
    fn parse_causation_accepts_only_the_transactional_wamn_contract() {
        let good = br#"{"run":"f1:evt:9","root":"f1:evt:1","depth":3}"#;
        assert_eq!(
            parse_causation(1, "wamn.causation", good),
            Some(Causation {
                run: "f1:evt:9".into(),
                root: "f1:evt:1".into(),
                depth: 3,
            }),
            "a transactional wamn.causation frame with valid JSON is the stamp"
        );
        // Non-transactional: ignored — the unforgeable property rides on the
        // commit, so only a transactional message counts.
        assert_eq!(parse_causation(0, "wamn.causation", good), None);
        // A foreign prefix is not ours.
        assert_eq!(parse_causation(1, "some.other.prefix", good), None);
        // Malformed / incomplete / smuggling extra fields (deny_unknown_fields):
        // dropped, never a crash.
        assert_eq!(parse_causation(1, "wamn.causation", b"not json"), None);
        assert_eq!(
            parse_causation(1, "wamn.causation", br#"{"run":"a"}"#),
            None
        );
        assert_eq!(
            parse_causation(
                1,
                "wamn.causation",
                br#"{"run":"a","root":"b","depth":1,"x":2}"#
            ),
            None
        );
    }

    // R11 — the shared reopen backoff/cap ladder. These drive the ladder
    // deterministically with synthetic `Instant`s: the "stubbed stream that
    // opens-then-errs" is modelled by feeding `commits == 0` re-opens.
}

fn replication_url(input: &str) -> anyhow::Result<String> {
    let mut input = url::Url::parse(input)?;
    input
        .query_pairs_mut()
        .append_pair("replication", "database");
    Ok(input.into())
}
