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
//! - **Stall interlock** (E2): "delayed, never lost" is only SAFE if someone is
//!   told early, because a held LSN silently freezes WAL retention on the source
//!   DB until `max_slot_wal_keep_size` invalidates the slot — a capture gap.
//!   `publish_acked` escalates to a distinct `CDC_PUBLISH_STALLED` event past
//!   `--stall-threshold-secs`; independently a slot-headroom monitor polls
//!   `pg_replication_slots.safe_wal_size` over a SEPARATE plain connection and
//!   alerts BEFORE `wal_status` leaves `reserved`. Runbook: on a sustained
//!   stall, fix JetStream — NEVER drop the slot (that "fixes" the disk by
//!   creating the gap). All signals are structured `wamn::event_reader` events.
//! - **Commit order**: `StreamingMode::Off` — the server delivers whole
//!   transactions in commit order; sequential publish preserves it, so stream
//!   order == commit order per DB.
//! - **The reader NEVER creates the slot.** `enable-cdc-project-env` created
//!   it (WAL pinned from enable); a MISSING or INVALIDATED slot is a capture
//!   GAP — a first-class incident (v3 §11): the reader refuses to start (or
//!   dies) loudly instead of silently re-creating and resuming from "now".
//!   Recovery is operator-driven: re-enable CDC + replay/backfill assessment.
//! - **Session re-open** (S-CDC-1 finding F2, R11): the crate's inner retry can
//!   be shorter than a real primary-less window, so a session-level re-open
//!   loop wraps the drain. ONE `ReopenLadder` backs BOTH arms (open failure and
//!   drain sever), so a session that opens cleanly then severs immediately can
//!   no longer hot-loop preflight→connect→sever: every re-open backs off, and
//!   the cap trips two ways. The consecutive-failure streak resets ONLY on a
//!   drain that committed a transaction (`DrainSummary { commits > 0 }` —
//!   productivity, never open success), catching a fast flap; a trailing-window
//!   re-open RATE cap catches a slow flap that commits a little each session
//!   and would otherwise reset the streak forever. The slot admits one
//!   consumer, so exclusivity is structural (the lease that elects WHICH
//!   replica holds the session is deferred — run replicas=1).
//! - **Entity keying** (wamn-l5i9.11): each relation resolves to its stable
//!   catalog entity id via the OID-keyed `wamn_entities` map (maintained by
//!   publish/migrate-catalog in the DDL's transaction) — resolved lazily per
//!   session, never invalidated (OIDs survive renames), so envelopes and
//!   subjects stay keyed on the entity id across `ALTER TABLE RENAME` (R9b).
//!   Unmapped tables publish with the table-name fallback, `entity` absent.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use async_nats::header::{HeaderMap, NATS_MESSAGE_ID};
use async_nats::jetstream;
use clap::Args;
use pg_walstream::{
    CancellationToken, ColumnValue, EventStream, EventType, LogicalReplicationStream,
    ReplicationError, ReplicationStreamConfig, RetryConfig, RowData, StreamingMode,
};
use tokio_postgres::NoTls;

use wamn_event_wire::{Causation, Envelope, Op, msg_id, stream_subjects, subject};
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

    /// How long a single JetStream publish may retry unacked before the reader
    /// emits the distinct `CDC_PUBLISH_STALLED` alert event (E2). Below this the
    /// retries are ordinary warns; at/past it every retry is an error alerts can
    /// bind to. The LSN is held throughout — a stall silently freezes WAL
    /// retention on the source DB, so this is a SAFETY INTERLOCK, not a metric.
    #[arg(long, default_value_t = 30)]
    pub stall_threshold_secs: u64,

    /// How often the slot-headroom monitor polls `pg_replication_slots` over a
    /// SEPARATE plain connection (E2 backstop). Zero disables the monitor.
    #[arg(long, default_value_t = 30)]
    pub slot_poll_secs: u64,

    /// Warn while the slot is still `reserved` but `safe_wal_size` has fallen
    /// below this many bytes — the early alert that fires BEFORE `wal_status`
    /// leaves `reserved` (E2). Default 256 MiB (≈16 WAL segments of headroom).
    #[arg(long, default_value_t = 268_435_456)]
    pub slot_safe_wal_warn_bytes: i64,
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

/// Shared reopen backoff + cap ladder (R11). BOTH the open-failure arm and the
/// drain-sever arm feed this ONE ladder, so a session that opens cleanly then
/// severs immediately can no longer hot-loop preflight→connect→sever as fast as
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

/// Decode a logical-decoding `Message` frame into a causation stamp, or `None`
/// when it isn't our contract (wamn-l5i9.12). Two gates: the frame must be
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
    )
    // Deliver logical-decoding Message frames (the `wamn.causation` stamp the
    // wamn:postgres plugin emits per run-owned txn — wamn-l5i9.12): off by
    // default, so the reader must opt in for `drain` to see them.
    .with_messages(true);
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

    // `confirmed_lsn_age_seconds` gauge (E2): millis of the last confirmed-LSN
    // advance, seeded at start so a reader that never commits still ages. Shared
    // with the detached slot-headroom monitor.
    let last_lsn_advance_ms = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_millis()));
    spawn_slot_monitor(
        &args,
        reg.slot.clone(),
        token.clone(),
        last_lsn_advance_ms.clone(),
    );

    let mut ladder = ReopenLadder::new();
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

        match drain(&mut stream, &args, &token, &js, &last_lsn_advance_ms).await {
            DrainOutcome::Shutdown(summary) => {
                let _ = stream.shutdown().await;
                tracing::info!(
                    reopens = ladder.reopens(),
                    commits = summary.commits,
                    "shutdown requested; exiting cleanly"
                );
                return Ok(());
            }
            DrainOutcome::Severed(e, summary) => {
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
                        // Productivity — not open success — resets the streak:
                        // only a drain that committed clears the fast-flap cap.
                        ladder_step_or_bail(&mut ladder, summary.commits, &e, &token).await?;
                    }
                }
            }
        }
    }
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
                backoff_ms = delay.as_millis() as u64,
                "session severed/failed; backing off before re-open"
            );
            tokio::select! {
                _ = token.cancelled() => {}
                _ = tokio::time::sleep(delay) => {}
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
}

/// A decoded row event awaiting its transaction's `Commit`. Entity resolution
/// happens at arrival (keeping the per-session OID cache warm); only the
/// causation stamp and the publish are deferred to the commit.
struct PendingRow {
    op: Op,
    old: Option<serde_json::Map<String, serde_json::Value>>,
    new: Option<serde_json::Map<String, serde_json::Value>>,
    entity: Option<String>,
    table: String,
    lsn: u64,
}

/// What a drained session produced. `commits` is the R11 productivity signal —
/// the shared ladder resets the fast-flap streak only when a session committed
/// at least one transaction, so an open-then-sever session that never commits
/// keeps counting toward the cap.
struct DrainSummary {
    commits: u64,
    published: u64,
    deduped: u64,
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
/// publishes every buffered row (ack-awaited, in arrival order) with the
/// causation stamp attached, then advances the feedback LSN — only once every
/// row is acked. Returns the `DrainSummary` in both outcomes.
async fn drain(
    stream: &mut EventStream,
    args: &EventReaderArgs,
    token: &CancellationToken,
    js: &jetstream::Context,
    last_lsn_advance_ms: &AtomicI64,
) -> DrainOutcome {
    let mut txn: Option<Txn> = None;
    let mut summary = DrainSummary {
        commits: 0,
        published: 0,
        deduped: 0,
    };
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
                tracing::info!(
                    target: "wamn::event_reader",
                    commits = summary.commits,
                    events_published = summary.published,
                    deduped = summary.deduped,
                    "drain summary"
                );
                return DrainOutcome::Shutdown(summary);
            }
            Err(e) => {
                tracing::info!(
                    target: "wamn::event_reader",
                    commits = summary.commits,
                    events_published = summary.published,
                    deduped = summary.deduped,
                    "drain summary (severed)"
                );
                return DrainOutcome::Severed(e, summary);
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
                    causation: None,
                    rows: Vec::new(),
                });
                continue;
            }
            EventType::Commit { end_lsn, .. } => {
                let Some(frame) = txn.take() else {
                    return DrainOutcome::Severed(
                        ReplicationError::Protocol("Commit outside a Begin frame".into()),
                        summary,
                    );
                };
                // Buffer-per-txn: publish the whole transaction NOW, the
                // causation stamp (if the plugin emitted one) attached to every
                // row — robust to whether the message arrived before or after
                // these rows. Each publish awaits its ack.
                for row in &frame.rows {
                    let envelope = Envelope {
                        op: row.op,
                        old: row.old.clone(),
                        new: row.new.clone(),
                        entity: row.entity.clone(),
                        table: row.table.clone(),
                        lsn: row.lsn,
                        txid: frame.txid,
                        commit_ts: frame.commit_ts,
                        causation: frame.causation.clone(),
                    };
                    let subj = subject(
                        &args.org,
                        &args.project,
                        &args.env,
                        envelope.entity_segment(),
                        row.op,
                    );
                    let id = msg_id(&args.project, &args.env, row.lsn);
                    let payload = match serde_json::to_vec(&envelope) {
                        Ok(p) => p,
                        Err(e) => {
                            return DrainOutcome::Severed(
                                ReplicationError::Generic(format!("serialize envelope: {e}")),
                                summary,
                            );
                        }
                    };
                    let stall_threshold = Duration::from_secs(args.stall_threshold_secs);
                    match publish_acked(js, token, &subj, &id, payload, stall_threshold).await {
                        PublishOutcome::Acked { duplicate } => {
                            summary.published += 1;
                            if duplicate {
                                summary.deduped += 1;
                                tracing::debug!(id, "redelivery deduped by the stream");
                            }
                        }
                        PublishOutcome::CancelledMidRetry => {
                            tracing::info!(
                                target: "wamn::event_reader",
                                commits = summary.commits,
                                events_published = summary.published,
                                deduped = summary.deduped,
                                "drain summary (cancelled mid-publish)"
                            );
                            return DrainOutcome::Shutdown(summary);
                        }
                    }
                }
                // Every row of this txn is acked — NOW the confirmed LSN may
                // advance past the commit.
                let l = end_lsn.value();
                stream.update_flushed_lsn(l);
                stream.update_applied_lsn(l);
                summary.commits += 1;
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
                    match txn.as_mut() {
                        Some(frame) => frame.causation = Some(c),
                        None => tracing::warn!(
                            "transactional wamn.causation message outside a txn frame — ignored"
                        ),
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
            EventType::Truncate(tables) => {
                // Not part of the event plane (v3 ops are insert/update/delete).
                tracing::warn!(?tables, "TRUNCATE observed — not published");
                continue;
            }
            // Metadata frames; nothing to buffer, nothing to advance.
            EventType::Relation { .. } | EventType::Type { .. } | EventType::Origin { .. } => {
                continue;
            }
            other => {
                // Streaming/two-phase frames can't occur (Off/off) — a
                // protocol surprise is worth a loud log, not a crash.
                tracing::warn!(?other, "unexpected replication frame — skipped");
                continue;
            }
        };
        // First row event of a relation this session: resolve its entity id
        // from the map (by OID — rename-proof). `None` is cached too, so an
        // unmapped table costs one lookup per session, not one per event.
        if !entities.contains_key(&relation_oid) {
            let resolved = match resolve_entity(args, &schema, relation_oid).await {
                Ok(r) => r,
                Err(e) => return DrainOutcome::Severed(e, summary),
            };
            tracing::info!(
                %table,
                relation_oid,
                entity = resolved.as_deref().unwrap_or("(unmapped)"),
                "entity resolved"
            );
            entities.insert(relation_oid, resolved);
        }
        let entity = entities.get(&relation_oid).cloned().flatten();
        // Buffer the row; it publishes at Commit, once the txn's causation
        // stamp (if any) is known.
        let Some(frame) = txn.as_mut() else {
            return DrainOutcome::Severed(
                ReplicationError::Protocol("row event outside a Begin/Commit frame".into()),
                summary,
            );
        };
        frame.rows.push(PendingRow {
            op,
            old: old.as_ref().map(row_to_map),
            new: new.as_ref().map(row_to_map),
            entity,
            table: table.to_string(),
            lsn,
        });
    }
}

enum PublishOutcome {
    Acked { duplicate: bool },
    CancelledMidRetry,
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
/// `safe_wal_size` early-warning floor so the alert fires BEFORE the status
/// leaves `reserved`.
#[derive(Debug, PartialEq, Eq)]
enum SlotHealth {
    /// `reserved` with headroom above the warn floor.
    Healthy,
    /// Still `reserved` but `safe_wal_size` fell below the warn floor — the
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
/// floor check only applies on the `reserved` branch.
fn classify_slot_health(
    wal_status: &str,
    safe_wal_size: Option<i64>,
    warn_floor_bytes: i64,
) -> SlotHealth {
    match wal_status {
        "reserved" => match safe_wal_size {
            Some(n) if n < warn_floor_bytes => SlotHealth::HeadroomLow,
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
    sslmode: &str,
    slot: &str,
    warn_floor: i64,
    last_lsn_advance_ms: &AtomicI64,
) -> anyhow::Result<()> {
    let url = preflight_url(cdc_url, sslmode)?;
    let (client, conn) = tokio_postgres::connect(&url, NoTls)
        .await
        .context("slot-monitor connect")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let row = client
        .query_opt(
            "SELECT wal_status::text, safe_wal_size, \
             (pg_current_wal_lsn() - restart_lsn)::bigint \
             FROM pg_replication_slots WHERE slot_name = $1",
            &[&slot],
        )
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
    match classify_slot_health(status, safe_wal_size, warn_floor) {
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
/// loop's preflight owns the die-loudly decision.
fn spawn_slot_monitor(
    args: &EventReaderArgs,
    slot: String,
    token: CancellationToken,
    last_lsn_advance_ms: Arc<AtomicI64>,
) {
    if args.slot_poll_secs == 0 {
        return;
    }
    let cdc_url = args.cdc_url.clone();
    let sslmode = args.sslmode.clone();
    let warn_floor = args.slot_safe_wal_warn_bytes;
    let poll = Duration::from_secs(args.slot_poll_secs);
    tokio::spawn(async move {
        loop {
            if let Err(e) =
                poll_slot_once(&cdc_url, &sslmode, &slot, warn_floor, &last_lsn_advance_ms).await
            {
                tracing::warn!(
                    target: "wamn::event_reader",
                    slot = %slot,
                    error = %e,
                    "slot-headroom poll failed (transient) — will retry"
                );
            }
            tokio::select! {
                _ = token.cancelled() => return,
                _ = tokio::time::sleep(poll) => {}
            }
        }
    });
}

/// Publish one event and wait for the JetStream ack — retrying FOREVER
/// (bounded only by shutdown). JetStream down ⇒ we hold here ⇒ the LSN holds
/// ⇒ WAL is retained: delayed, never lost. But a held LSN silently freezes WAL
/// retention on the source DB, so once the retries pass `stall_threshold` this
/// escalates from ordinary warns to a distinct `CDC_PUBLISH_STALLED` alert
/// event (E2 — the interlock that makes "delayed, never lost" observable).
async fn publish_acked(
    js: &jetstream::Context,
    token: &CancellationToken,
    subject: &str,
    id: &str,
    payload: Vec<u8>,
    stall_threshold: Duration,
) -> PublishOutcome {
    let payload = bytes::Bytes::from(payload);
    let mut delay = Duration::from_millis(500);
    let first_attempt = Instant::now();
    let mut publish_retries: u64 = 0;
    let mut stalled = false;
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
                if stalled {
                    tracing::info!(
                        target: "wamn::event_reader",
                        event = "CDC_PUBLISH_RECOVERED",
                        subject,
                        id,
                        publish_retries,
                        publish_stall_seconds = first_attempt.elapsed().as_secs(),
                        "JetStream publish recovered — the LSN can advance again"
                    );
                }
                return PublishOutcome::Acked {
                    duplicate: ack.duplicate,
                };
            }
            Err(e) => {
                publish_retries += 1;
                let stall = first_attempt.elapsed();
                match stall_level(stall, stall_threshold) {
                    StallLevel::Retrying => tracing::warn!(
                        target: "wamn::event_reader",
                        subject,
                        id,
                        publish_retries,
                        publish_stall_seconds = stall.as_secs(),
                        error = %e,
                        "publish unacked — holding the LSN; retrying"
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

    #[test]
    fn ladder_open_then_err_flap_terminates_within_the_cap() {
        // Every session opens then severs without committing (commits == 0) —
        // the hot-loop R11 describes. The streak guard MUST trip, and within
        // `max_consecutive` re-opens, not never.
        let mut ladder = ReopenLadder::new();
        let cap = ladder.max_consecutive;
        let t0 = Instant::now();
        let mut tripped_at = None;
        for i in 1..=cap {
            match ladder.note_reopen(t0, 0) {
                LadderStep::Trip(_) => {
                    tripped_at = Some(i);
                    break;
                }
                LadderStep::Backoff(d) => {
                    // Backoff must be bounded and grow with the streak.
                    assert!(d <= ladder.max_backoff);
                }
            }
        }
        assert_eq!(
            tripped_at,
            Some(cap),
            "an open-then-err flap must terminate exactly at the consecutive cap"
        );
    }

    #[test]
    fn ladder_productivity_resets_the_streak() {
        // A session that commits (commits > 0) clears the fast-flap streak, so
        // the consecutive guard NEVER trips no matter how many productive
        // re-opens happen. Spaced beyond the rate window so ONLY the streak
        // guard is exercised here.
        let mut ladder = ReopenLadder::new();
        let t0 = Instant::now();
        for i in 0..100 {
            let now = t0 + Duration::from_secs(i * 10);
            assert!(
                matches!(ladder.note_reopen(now, 1), LadderStep::Backoff(_)),
                "a productive re-open must never trip the streak guard"
            );
        }
        assert_eq!(ladder.consecutive_failures(), 0);
    }

    #[test]
    fn ladder_rate_cap_catches_a_slow_productive_flap() {
        // The slow flap the streak guard alone misses: every session commits
        // once (streak stays 0) but re-opens ~1/s, far above the rate cap. The
        // trailing-window guard must NOT trip while under the cap and MUST trip
        // exactly once it is exceeded — pinning both directions.
        let mut ladder = ReopenLadder::new();
        let cap = ladder.rate_cap as u64;
        let t0 = Instant::now();
        let mut trip_at = None;
        for i in 0..100u64 {
            let now = t0 + Duration::from_secs(i);
            match ladder.note_reopen(now, 1) {
                LadderStep::Backoff(_) => assert!(
                    i <= cap,
                    "re-open #{i} is still under the rate cap ({cap}); must not trip yet"
                ),
                LadderStep::Trip(_) => {
                    trip_at = Some(i);
                    break;
                }
            }
        }
        // `cap + 1` re-opens (indices 0..=cap) put `cap + 1` instants in the
        // window; the trip fires on that one, i.e. at index `cap`.
        assert_eq!(
            trip_at,
            Some(cap),
            "a sustained productive flap must trip the rate cap the moment it is exceeded"
        );
        assert_eq!(
            ladder.consecutive_failures(),
            0,
            "the trip came from the rate cap, not the streak"
        );
    }

    // E2 — the stall interlock's decision cores.

    #[test]
    fn stall_level_escalates_at_the_threshold() {
        let t = Duration::from_secs(30);
        // Below the threshold: an ordinary retry.
        assert_eq!(
            stall_level(Duration::from_secs(29), t),
            StallLevel::Retrying
        );
        // AT the threshold: the distinct alert fires (boundary is load-bearing —
        // an off-by-one here delays the CDC_PUBLISH_STALLED alert by a whole
        // retry interval).
        assert_eq!(stall_level(Duration::from_secs(30), t), StallLevel::Stalled);
        // Past it: still stalled.
        assert_eq!(
            stall_level(Duration::from_secs(600), t),
            StallLevel::Stalled
        );
    }

    #[test]
    fn slot_health_maps_wal_status_and_the_safe_wal_floor() {
        let floor = 268_435_456; // 256 MiB
        // reserved with ample headroom → healthy; below the floor → the early
        // warning that fires BEFORE the status leaves 'reserved'.
        assert_eq!(
            classify_slot_health("reserved", Some(floor * 4), floor),
            SlotHealth::Healthy
        );
        assert_eq!(
            classify_slot_health("reserved", Some(floor - 1), floor),
            SlotHealth::HeadroomLow
        );
        // At exactly the floor it is still healthy (strictly-below warns).
        assert_eq!(
            classify_slot_health("reserved", Some(floor), floor),
            SlotHealth::Healthy
        );
        // The Postgres wal_status ladder past 'reserved'.
        assert_eq!(
            classify_slot_health("extended", None, floor),
            SlotHealth::Extended
        );
        assert_eq!(
            classify_slot_health("unreserved", None, floor),
            SlotHealth::Unreserved
        );
        assert_eq!(classify_slot_health("lost", None, floor), SlotHealth::Lost);
        // NULL/unknown status is surfaced, never silently healthy.
        assert_eq!(
            classify_slot_health("unknown", None, floor),
            SlotHealth::HeadroomLow
        );
    }
}
