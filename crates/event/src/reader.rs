//! Read committed row changes into the environment event stream.
//! A transaction advances only after every broker acknowledgement settles.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, hash_map::Entry};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use wamn_package::{PackageName, manifest::CONTROL_OWNED_RELATION_TABLES};
use wamn_registry::Triple;

use crate::{
    Causation, CdcIdentity, Envelope, Image, Op, SourceEvent, UnavailableColumn, msg_id, subject,
};

/// The changes captured by every CDC publication.
pub const CDC_PUBLISH: &str = "insert, update, delete";

/// The event target and publish stall threshold for one reader.
#[derive(Debug)]
pub struct ReaderConfig {
    pub target: Triple,
    pub stall_threshold: Duration,
}

/// What a replication failure means for the reader.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionFate {
    Cancelled,
    CaptureGap,
    Fatal,
    Reopen,
}

/// A failure with its retry disposition and original cause.
#[derive(Debug)]
pub struct SessionFailure {
    pub fate: SessionFate,
    pub source: anyhow::Error,
}

impl SessionFailure {
    fn reopen(source: anyhow::Error) -> Self {
        Self {
            fate: SessionFate::Reopen,
            source,
        }
    }
}

/// The closed classification of a managed relation.
#[derive(Debug)]
pub enum RelationClassification {
    Entity {
        package_id: PackageName,
        entity_id: String,
    },
    Excluded {
        package_id: PackageName,
        relation_id: String,
    },
}

type RowImage = serde_json::Map<String, serde_json::Value>;

/// A change or progress frame from the replication source.
#[derive(Debug)]
pub enum ReplicationFrame<R> {
    Begin {
        txid: u32,
        commit_ts: chrono::DateTime<chrono::Utc>,
    },
    Commit {
        commit_lsn: u64,
        end_lsn: u64,
    },
    Row {
        op: Op,
        schema: Arc<str>,
        table: Arc<str>,
        relation_oid: u32,
        old: Option<R>,
        new: Option<R>,
        /// Columns marked as replica identity in the WAL relation frame.
        replica_key: Arc<[Arc<str>]>,
        unavailable: Vec<UnavailableColumn>,
    },
    Message {
        flags: u8,
        prefix: Arc<str>,
        content: Bytes,
    },
    Truncate {
        tables: Vec<Arc<str>>,
    },
    Keepalive {
        wal_end: u64,
    },
}

/// Read one replication session and submit its confirmed progress.
pub trait ReplicationInput {
    /// The input's raw row, decoded only after entity selection.
    type Row: Send;

    /// Return the next data or progress frame, or None on cancellation.
    fn next_frame(
        &mut self,
    ) -> impl Future<Output = Result<Option<ReplicationFrame<Self::Row>>, SessionFailure>> + Send;

    /// Look up one managed relation by its source schema and OID.
    fn relation_classification(
        &mut self,
        schema: &str,
        relation_oid: u32,
    ) -> impl Future<Output = Result<RelationClassification, SessionFailure>> + Send;

    /// Find primary key and generated identity columns before trimming a row.
    fn key_columns(
        &mut self,
        relation_oid: u32,
    ) -> impl Future<Output = Result<Vec<String>, SessionFailure>> + Send;

    /// Decode the selected row into the envelope's wire representation.
    fn decode_row(row: &Self::Row) -> Result<RowImage, SessionFailure>;

    /// Return the source system ID and the timeline that contains this commit.
    fn origin(&self, commit_lsn: u64) -> (u64, u32);

    fn confirmed_position(&self) -> u64;
    fn confirm(&mut self, position: u64);
}

/// Send events and settle their broker acknowledgements separately.
pub trait EventPublisher {
    type Ack: Send;

    /// Maximum encoded body size after allowing for this message's headers.
    fn payload_limit(&self, id: &str) -> usize;

    /// Whether the broker refused the size of this message.
    fn is_payload_error(&self, error: &anyhow::Error) -> bool;

    /// Submit one message without waiting for its server acknowledgement.
    fn send(
        &self,
        subject: &str,
        id: &str,
        payload: &Bytes,
    ) -> impl Future<Output = anyhow::Result<Self::Ack>> + Send;

    /// Settle one acknowledgement. True means the message was a duplicate.
    fn settle(&self, ack: Self::Ack) -> impl Future<Output = anyhow::Result<bool>> + Send;
}

fn merge_relation_counts(committed: &mut BTreeMap<String, u64>, pending: BTreeMap<String, u64>) {
    for (identity, count) in pending {
        *committed.entry(identity).or_default() += count;
    }
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
            tracing::warn!(target: "wamn_cdc_reader", error = %e, "wamn.causation message did not parse as Causation — ignoring");
            None
        }
    }
}

/// A received TRUNCATE is reported and skipped.
const TRUNCATE_INCIDENT_EVENT: &str = "CDC_TRUNCATE_RECEIVED";

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
    old: Option<RowImage>,
    new: Option<RowImage>,
    unavailable: Vec<UnavailableColumn>,
    replica_key: Arc<[Arc<str>]>,
    relation_oid: u32,
    package_id: PackageName,
    entity_id: String,
    table: String,
    ordinal: u64,
}

/// What a drained session produced. `commits` is the R11 productivity signal —
/// the shared ladder resets the fast-flap streak only when a session committed
/// at least one transaction, so an open-then-sever session that never commits
/// keeps counting toward the cap.
#[derive(Default)]
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

/// The result and productivity of one replication session.
#[derive(Debug)]
pub struct DrainOutcome {
    pub commits: u64,
    pub result: Result<(), SessionFailure>,
}

/// Drain a session, settling every acknowledgement before confirming a commit.
/// The caller supplies the stop check and the interruptible retry timer.
pub async fn drain(
    input: &mut impl ReplicationInput,
    args: &ReaderConfig,
    publisher: &impl EventPublisher,
    is_stopping: &impl Fn() -> bool,
    wait_to_retry: &impl AsyncFn(Duration) -> bool,
) -> DrainOutcome {
    let mut summary = DrainSummary::default();
    let result = drain_frames(
        input,
        args,
        publisher,
        is_stopping,
        wait_to_retry,
        &mut summary,
    )
    .await;
    log_drain_summary(
        &summary,
        if result.is_ok() {
            "shutdown"
        } else {
            "severed"
        },
    );
    DrainOutcome {
        commits: summary.commits,
        result,
    }
}

async fn drain_frames<I: ReplicationInput>(
    input: &mut I,
    args: &ReaderConfig,
    publisher: &impl EventPublisher,
    is_stopping: &impl Fn() -> bool,
    wait_to_retry: &impl AsyncFn(Duration) -> bool,
    summary: &mut DrainSummary,
) -> Result<(), SessionFailure> {
    let mut txn: Option<Txn> = None;
    // OIDs survive renames, so a resolution stays correct for this session.
    let mut relation_classifications: HashMap<u32, RelationClassification> = HashMap::new();
    while let Some(frame) = input.next_frame().await? {
        let (op, old, new, schema, table, relation_oid, replica_key, unavailable) = match frame {
            ReplicationFrame::Begin { txid, commit_ts } => {
                txn = Some(Txn {
                    txid,
                    commit_ts,
                    causation: None,
                    rows: Vec::new(),
                    next_ordinal: 0,
                    excluded_by_relation: BTreeMap::new(),
                });
                continue;
            }
            ReplicationFrame::Commit {
                commit_lsn,
                end_lsn,
            } => {
                let Some(frame) = txn.take() else {
                    return Err(SessionFailure::reopen(anyhow::anyhow!(
                        "Protocol parsing error: Commit outside a Begin frame"
                    )));
                };
                // Buffer-per-txn: build the whole transaction's wire messages
                // NOW, the causation stamp (if the plugin emitted one) attached
                // to every row — robust to whether the message arrived before or
                // after these rows.
                let mut msgs = Vec::with_capacity(frame.rows.len());
                let (system_id, timeline) = input.origin(commit_lsn);
                for row in frame.rows {
                    let envelope = Envelope {
                        op: row.op,
                        old: row.old,
                        new: row.new,
                        unavailable: row.unavailable,
                        package_id: row.package_id,
                        entity: row.entity_id,
                        table: row.table,
                        identity: CdcIdentity {
                            system_id,
                            timeline,
                            commit_lsn,
                            row_ordinal: row.ordinal,
                        },
                        txid: frame.txid,
                        commit_ts: frame.commit_ts,
                        causation: frame.causation.clone(),
                    };
                    let subject = subject(
                        &args.target.org,
                        &args.target.project,
                        &args.target.env,
                        envelope.entity_segment(),
                        row.op,
                    );
                    let id = msg_id(&args.target.project, &args.target.env, &envelope.identity);
                    let limit = publisher.payload_limit(&id);
                    msgs.push(
                        prepare_message(
                            input,
                            row.relation_oid,
                            envelope,
                            subject,
                            id,
                            limit,
                            &row.replica_key,
                        )
                        .await?,
                    );
                }
                // E1: pipeline the txn's publishes — sent without awaiting, the
                // ack futures held in publish order, ALL settled here before the
                // LSN advances (the v3 §4 per-txn invariant, unchanged).
                let has_exclusions = !frame.excluded_by_relation.is_empty();
                let Some(tally) = publish_txn(
                    publisher,
                    is_stopping,
                    wait_to_retry,
                    &msgs,
                    args.stall_threshold,
                )
                .await?
                else {
                    return Ok(());
                };
                summary.published += tally.published;
                summary.deduped += tally.deduped;
                merge_relation_counts(
                    &mut summary.excluded_by_relation,
                    frame.excluded_by_relation,
                );
                input.confirm(end_lsn);
                summary.commits += 1;
                if has_exclusions {
                    tracing::info!(
                        target: "wamn::event_reader",
                        excluded_by_relation = ?summary.excluded_by_relation,
                        "CDC exclusions committed"
                    );
                }
                continue;
            }
            ReplicationFrame::Message {
                flags,
                prefix,
                content,
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
                            target: "wamn_cdc_reader",
                            "transactional wamn.causation message outside a txn frame — ignored"
                        );
                    }
                }
                continue;
            }
            ReplicationFrame::Row {
                op,
                old,
                new,
                schema,
                table,
                relation_oid,
                replica_key,
                unavailable,
            } => (
                op,
                old,
                new,
                schema,
                table,
                relation_oid,
                replica_key,
                unavailable,
            ),
            ReplicationFrame::Truncate { tables } => {
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
            // With no transaction open, all earlier changes are acknowledged.
            ReplicationFrame::Keepalive { wal_end } => {
                if txn.is_none() && wal_end > input.confirmed_position() {
                    input.confirm(wal_end);
                }
                continue;
            }
        };
        let Some(frame) = txn.as_mut() else {
            return Err(SessionFailure::reopen(anyhow::anyhow!(
                "Protocol parsing error: row event outside a Begin/Commit frame"
            )));
        };
        let ordinal = frame.next_ordinal;
        frame.next_ordinal += 1;
        if CONTROL_OWNED_RELATION_TABLES.contains(&table.as_ref()) {
            continue;
        }
        let classification = match relation_classifications.entry(relation_oid) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let resolved = input.relation_classification(&schema, relation_oid).await?;
                match &resolved {
                    RelationClassification::Entity {
                        package_id,
                        entity_id,
                    } => tracing::info!(
                        target: "wamn_cdc_reader",
                        %table,
                        relation_oid,
                        package_id = package_id.as_str(),
                        entity = entity_id.as_str(),
                        "entity resolved"
                    ),
                    RelationClassification::Excluded {
                        package_id,
                        relation_id,
                    } => tracing::info!(
                        target: "wamn_cdc_reader",
                        %table,
                        relation_oid,
                        package_id = package_id.as_str(),
                        relation = relation_id.as_str(),
                        "CDC exclusion resolved"
                    ),
                }
                entry.insert(resolved)
            }
        };
        match classification {
            RelationClassification::Entity {
                package_id,
                entity_id,
            } => {
                frame.rows.push(PendingRow {
                    op,
                    old: old.as_ref().map(I::decode_row).transpose()?,
                    new: new.as_ref().map(I::decode_row).transpose()?,
                    unavailable,
                    replica_key,
                    relation_oid,
                    package_id: package_id.clone(),
                    entity_id: entity_id.clone(),
                    table: table.to_string(),
                    ordinal,
                });
            }
            RelationClassification::Excluded {
                package_id,
                relation_id,
            } => {
                let identity = format!("{package_id}::{relation_id}");
                *frame.excluded_by_relation.entry(identity).or_default() += 1;
            }
        }
    }
    Ok(())
}

/// The E1 in-flight publish bound: at most this many un-acked publishes are held
/// before the pipeline settles them mid-transaction.
const MAX_IN_FLIGHT: usize = 256;

/// One row event's wire form, prepared once and re-published as-is on a retry
/// (the `Nats-Msg-Id` in `id` is what the JetStream duplicate window keys on).
struct PreparedMsg {
    subject: String,
    id: String,
    payload: Bytes,
}

async fn prepare_message(
    input: &mut impl ReplicationInput,
    relation_oid: u32,
    envelope: Envelope,
    subject: String,
    id: String,
    limit: usize,
    replica_key: &[Arc<str>],
) -> Result<PreparedMsg, SessionFailure> {
    let mut event = SourceEvent::Cdc(envelope);
    let mut payload = Vec::new();
    encode(&mut payload, &event)?;
    if payload.len() <= limit {
        return Ok(PreparedMsg {
            subject,
            id,
            payload: payload.into(),
        });
    }

    let original_size = payload.len();
    let key_columns = input.key_columns(relation_oid).await?;
    let SourceEvent::Cdc(envelope) = &mut event;
    let unavailable_before = envelope.unavailable.len();
    let mut candidates = Vec::new();
    for (image, values) in [(Image::Old, &envelope.old), (Image::New, &envelope.new)] {
        let Some(values) = values else { continue };
        let start = candidates.len();
        for (column, value) in values {
            if column == "project_id"
                || replica_key.iter().any(|name| &**name == column)
                || key_columns.contains(column)
            {
                continue;
            }
            encode(&mut payload, value)?;
            candidates.push((Reverse(payload.len()), column.clone(), image));
        }
        candidates[start..].sort_unstable_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    }

    for (_, column, image) in candidates {
        let SourceEvent::Cdc(envelope) = &mut event;
        let values = match image {
            Image::Old => &mut envelope.old,
            Image::New => &mut envelope.new,
        };
        values
            .as_mut()
            .expect("trim candidates have an image")
            .remove(&column);
        envelope
            .unavailable
            .push(UnavailableColumn { image, column });
        encode(&mut payload, &event)?;
        if payload.len() <= limit {
            let SourceEvent::Cdc(envelope) = &event;
            tracing::warn!(
                target: "wamn::event_reader",
                %subject,
                %id,
                table = %envelope.table,
                omitted_columns = ?&envelope.unavailable[unavailable_before..],
                original_size,
                "CDC event trimmed to fit the broker payload limit"
            );
            payload.shrink_to_fit();
            return Ok(PreparedMsg {
                subject,
                id,
                payload: payload.into(),
            });
        }
    }

    let SourceEvent::Cdc(envelope) = &event;
    Err(SessionFailure {
        fate: SessionFate::Fatal,
        source: anyhow::anyhow!(
            "CDC event {id} on {subject} for table {} needs {} bytes with only protected columns and envelope metadata. The body limit is {limit} bytes.",
            envelope.table,
            payload.len()
        ),
    })
}

fn encode(payload: &mut Vec<u8>, value: &impl serde::Serialize) -> Result<(), SessionFailure> {
    payload.clear();
    serde_json::to_writer(payload, value).map_err(|error| {
        let message = format!("IO error: {error}");
        let source = std::io::Error::new(std::io::ErrorKind::InvalidData, error);
        SessionFailure::reopen(anyhow::Error::new(source).context(message))
    })
}

/// Running publish/dedupe counts for one transaction, folded into the
/// `DrainSummary` once the whole txn has settled.
#[derive(Default)]
struct PublishTally {
    published: u64,
    deduped: u64,
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

/// Publish one whole transaction's messages and settle every server ack —
/// retrying transient failures until shutdown. JetStream down ⇒ we hold here ⇒
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
    publisher: &impl EventPublisher,
    is_stopping: &impl Fn() -> bool,
    wait_to_retry: &impl AsyncFn(Duration) -> bool,
    msgs: &[PreparedMsg],
    stall_threshold: Duration,
) -> Result<Option<PublishTally>, SessionFailure> {
    let mut tally = PublishTally::default();
    let mut first_unacked = 0usize;
    let mut delay = Duration::from_millis(500);
    let first_attempt = Instant::now();
    let mut publish_retries: u64 = 0;
    let mut stalled = false;
    loop {
        if is_stopping() {
            return Ok(None);
        }
        match publish_pass(publisher, is_stopping, msgs, &mut first_unacked, &mut tally).await {
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
                return Ok(Some(tally));
            }
            PassOutcome::Cancelled => return Ok(None),
            PassOutcome::Failed { index, source: e } => {
                if publisher.is_payload_error(&e) {
                    let msg = &msgs[index];
                    return Err(SessionFailure {
                        fate: SessionFate::Fatal,
                        source: e.context(format!(
                            "broker refused CDC event {} on {} due to its size",
                            msg.id, msg.subject
                        )),
                    });
                }
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
                        error = %format_args!("{e:#}"),
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
                            error = %format_args!("{e:#}"),
                            "JetStream publish STALLED past threshold — LSN held, WAL retention frozen on the source DB; fix JetStream, do NOT drop the slot (v3 §11)"
                        );
                    }
                }
                if wait_to_retry(delay).await {
                    return Ok(None);
                }
                delay = (delay * 2).min(Duration::from_secs(10));
            }
        }
    }
}

/// One pass of the pipeline: publish `msgs[*first_unacked..]` without awaiting,
/// holding the ack futures in publish order, draining (settling) them whenever
/// the in-flight set reaches `MAX_IN_FLIGHT`, and settling the remainder at the
/// end. `first_unacked` advances as each ack settles durably. A `send` or
/// `settle` failure ends the pass at `Failed` (its held futures dropped — the
/// broker client releases their permits), leaving `first_unacked`
/// at the first row whose ack has NOT settled.
enum PassOutcome {
    Complete,
    Cancelled,
    Failed { index: usize, source: anyhow::Error },
}

async fn publish_pass(
    publisher: &impl EventPublisher,
    is_stopping: &impl Fn() -> bool,
    msgs: &[PreparedMsg],
    first_unacked: &mut usize,
    tally: &mut PublishTally,
) -> PassOutcome {
    // (row index, held ack future) in publish order — index strictly ascending.
    let mut held = Vec::new();
    for (idx, msg) in msgs.iter().enumerate().skip(*first_unacked) {
        if is_stopping() {
            return PassOutcome::Cancelled;
        }
        match publisher.send(&msg.subject, &msg.id, &msg.payload).await {
            Ok(ack) => held.push((idx, ack)),
            Err(source) => return PassOutcome::Failed { index: idx, source },
        }
        if held.len() >= MAX_IN_FLIGHT {
            // In-flight bound hit mid-transaction: settle the held batch, then
            // keep publishing. The LSN hold is unaffected.
            if let Err(source) = settle_all(publisher, msgs, first_unacked, &mut held, tally).await
            {
                return PassOutcome::Failed {
                    index: *first_unacked,
                    source,
                };
            }
        }
    }
    // Settle any remaining held acks BEFORE the caller advances the LSN.
    match settle_all(publisher, msgs, first_unacked, &mut held, tally).await {
        Ok(()) => PassOutcome::Complete,
        Err(source) => PassOutcome::Failed {
            index: *first_unacked,
            source,
        },
    }
}

/// Settle held acks in publish order. Each durable ack advances `first_unacked`
/// (and the tally); the FIRST failure stops the drain and returns, dropping the
/// still-held futures — `first_unacked` is left AT the failed row so the retry
/// restarts there and never re-settles a durably-acked prefix.
async fn settle_all<P: EventPublisher>(
    publisher: &P,
    msgs: &[PreparedMsg],
    first_unacked: &mut usize,
    held: &mut Vec<(usize, P::Ack)>,
    tally: &mut PublishTally,
) -> anyhow::Result<()> {
    for (idx, ack) in held.drain(..) {
        let duplicate = publisher.settle(ack).await?;
        debug_assert_eq!(idx, *first_unacked, "acks settle strictly in publish order");
        *first_unacked = idx + 1;
        tally.published += 1;
        if duplicate {
            tally.deduped += 1;
            tracing::debug!(target: "wamn_cdc_reader", id = msgs[idx].id, "redelivery deduped by the stream");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::future::{poll_fn, ready};
    use std::pin::pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
    use std::task::{Context, Poll, Waker};

    struct Input<'a> {
        frames: VecDeque<ReplicationFrame<RowImage>>,
        confirmed: &'a AtomicU64,
    }

    impl ReplicationInput for Input<'_> {
        type Row = RowImage;

        fn next_frame(
            &mut self,
        ) -> impl Future<Output = Result<Option<ReplicationFrame<Self::Row>>, SessionFailure>> + Send
        {
            ready(Ok(self.frames.pop_front()))
        }

        fn relation_classification(
            &mut self,
            _: &str,
            _: u32,
        ) -> impl Future<Output = Result<RelationClassification, SessionFailure>> + Send {
            ready(Ok(RelationClassification::Entity {
                package_id: "orders".parse().unwrap(),
                entity_id: "order".into(),
            }))
        }

        fn key_columns(
            &mut self,
            _: u32,
        ) -> impl Future<Output = Result<Vec<String>, SessionFailure>> + Send {
            ready(Ok(vec!["id".into(), "serial".into()]))
        }

        fn decode_row(row: &Self::Row) -> Result<RowImage, SessionFailure> {
            Ok(row.clone())
        }

        fn origin(&self, _: u64) -> (u64, u32) {
            (1, 1)
        }

        fn confirmed_position(&self) -> u64 {
            self.confirmed.load(Relaxed)
        }

        fn confirm(&mut self, position: u64) {
            self.confirmed.store(position, Relaxed);
        }
    }

    #[derive(Default)]
    struct Publisher {
        sent: Mutex<Vec<u64>>,
        last_failed: AtomicBool,
        last_ready: AtomicBool,
        refuse_payload: bool,
    }

    const PAYLOAD_LIMIT: usize = 1024 * 1024;

    impl EventPublisher for Publisher {
        type Ack = u64;

        fn payload_limit(&self, _: &str) -> usize {
            PAYLOAD_LIMIT
        }

        fn is_payload_error(&self, _: &anyhow::Error) -> bool {
            self.refuse_payload
        }

        fn send(
            &self,
            _: &str,
            id: &str,
            _: &Bytes,
        ) -> impl Future<Output = anyhow::Result<Self::Ack>> + Send {
            let (_, ordinal) = id.rsplit_once(':').unwrap();
            let ordinal = ordinal.parse().unwrap();
            self.sent.lock().unwrap().push(ordinal);
            ready(if self.refuse_payload {
                Err(anyhow::anyhow!("payload refused"))
            } else {
                Ok(ordinal)
            })
        }

        async fn settle(&self, ack: Self::Ack) -> anyhow::Result<bool> {
            if ack == 1 {
                anyhow::ensure!(self.last_failed.swap(true, Relaxed), "ack unavailable");
                poll_fn(|_| {
                    if self.last_ready.load(Relaxed) {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;
            }
            Ok(false)
        }
    }

    #[test]
    fn confirmation_waits_for_every_ack_and_retry_resumes_at_first_unacked_row() {
        let confirmed = AtomicU64::new(1);
        let commit_end = 42;
        let row = || ReplicationFrame::Row {
            op: Op::Insert,
            schema: "orders".into(),
            table: "orders".into(),
            relation_oid: 1,
            old: None,
            new: Some(RowImage::new()),
            replica_key: Arc::default(),
            unavailable: Vec::new(),
        };
        let mut input = Input {
            frames: [
                ReplicationFrame::Begin {
                    txid: 1,
                    commit_ts: chrono::DateTime::UNIX_EPOCH,
                },
                row(),
                ReplicationFrame::Keepalive { wal_end: 35 },
                row(),
                ReplicationFrame::Commit {
                    commit_lsn: 41,
                    end_lsn: commit_end,
                },
            ]
            .into(),
            confirmed: &confirmed,
        };
        let config = ReaderConfig {
            target: Triple::new(
                "acme".parse().unwrap(),
                "dev".parse().unwrap(),
                "orders".parse().unwrap(),
            ),
            stall_threshold: Duration::from_secs(30),
        };
        let publisher = Publisher::default();
        let retry = async |_| {
            assert_eq!(confirmed.load(Relaxed), 1);
            false
        };
        let mut reader = pin!(drain(&mut input, &config, &publisher, &|| false, &retry));
        let mut context = Context::from_waker(Waker::noop());
        assert!(reader.as_mut().poll(&mut context).is_pending());
        assert_eq!(*publisher.sent.lock().unwrap(), [0, 1, 1]);
        assert_eq!(confirmed.load(Relaxed), 1);

        publisher.last_ready.store(true, Relaxed);
        let Poll::Ready(outcome) = reader.poll(&mut context) else {
            panic!("reader still pending after all acknowledgements settled");
        };
        outcome.result.unwrap();
        assert_eq!(confirmed.load(Relaxed), commit_end);
    }

    fn prepare_test_message(envelope: Envelope, replica_key: &[Arc<str>]) -> PreparedMsg {
        let confirmed = AtomicU64::new(1);
        let mut input = Input {
            frames: VecDeque::new(),
            confirmed: &confirmed,
        };
        let mut prepared = pin!(prepare_message(
            &mut input,
            1,
            envelope,
            "subject".into(),
            "id".into(),
            PAYLOAD_LIMIT,
            replica_key,
        ));
        let mut context = Context::from_waker(Waker::noop());
        let Poll::Ready(result) = prepared.as_mut().poll(&mut context) else {
            panic!("preparing an event with a ready source must settle");
        };
        result.unwrap()
    }

    #[test]
    fn oversized_event_trims_old_then_new_by_encoded_value_size() {
        let envelope = serde_json::from_value(serde_json::json!({
            "op": "update",
            "old": {
                "id": "k".repeat(560_000), "replica_key": "r".repeat(50_000),
                "serial": "12", "project_id": "acme--dev--orders",
                "escaped": "\"".repeat(270_000), "plain": "p".repeat(330_000)
            },
            "new": {"id": "7", "big": "b".repeat(800_000), "keep": "k".repeat(100_000), "note": null},
            "unavailable": [{"image": "new", "column": "toasted"}],
            "package_id": "orders", "entity": "order", "table": "orders",
            "identity": {"system_id": 1, "timeline": 1, "commit_lsn": 41, "row_ordinal": 0},
            "txid": 1, "commit_ts": "2026-07-18T12:00:00Z"
        }))
        .unwrap();
        let msg = prepare_test_message(envelope, &["replica_key".into()]);
        assert!(msg.payload.len() <= PAYLOAD_LIMIT);
        let SourceEvent::Cdc(event) = SourceEvent::decode(&msg.payload).unwrap();
        assert_eq!(
            event
                .unavailable
                .iter()
                .map(|column| (column.image, column.column.as_str()))
                .collect::<Vec<_>>(),
            [
                (Image::New, "toasted"),
                (Image::Old, "escaped"),
                (Image::Old, "plain"),
                (Image::New, "big")
            ],
        );
        let old = event.old.as_ref().unwrap();
        assert_eq!(old["id"].as_str().unwrap().len(), 560_000);
        assert_eq!(old["replica_key"].as_str().unwrap().len(), 50_000);
        assert_eq!(old["serial"], "12");
        assert_eq!(old["project_id"], "acme--dev--orders");
        assert!(!old.contains_key("escaped") && !old.contains_key("plain"));
        let new = event.new.as_ref().unwrap();
        assert!(!new.contains_key("big") && !new.contains_key("toasted"));
        assert!(new["note"].is_null());
        let input = crate::matching::event_input(&event);
        assert_eq!(
            input["unavailable"],
            serde_json::json!([
                {"image": "new", "column": "toasted"}, {"image": "old", "column": "escaped"},
                {"image": "old", "column": "plain"}, {"image": "new", "column": "big"}
            ])
        );
    }

    #[test]
    fn size_check_includes_unavailable_entries_and_preserves_an_absent_image() {
        let envelope: Envelope = serde_json::from_value(serde_json::json!({
            "op": "update", "new": {"id": "7", "body": ""},
            "unavailable": [{"image": "new", "column": "toasted"}],
            "package_id": "orders", "entity": "order", "table": "orders",
            "identity": {"system_id": 1, "timeline": 1, "commit_lsn": 41, "row_ordinal": 0},
            "txid": 1, "commit_ts": "2026-07-18T12:00:00Z"
        }))
        .unwrap();
        let event = SourceEvent::Cdc(envelope);
        let fixed_size = serde_json::to_vec(&event).unwrap().len();
        let SourceEvent::Cdc(mut envelope) = event;
        envelope.new.as_mut().unwrap()["body"] = "b".repeat(PAYLOAD_LIMIT - fixed_size + 1).into();
        let msg = prepare_test_message(envelope, &[]);
        let SourceEvent::Cdc(event) = SourceEvent::decode(&msg.payload).unwrap();
        assert!(event.old.is_none());
        let new = event.new.unwrap();
        assert_eq!(new["id"], "7");
        assert!(!new.contains_key("body"));
        assert!(event.unavailable.contains(&UnavailableColumn {
            image: Image::New,
            column: "body".into()
        }));
    }

    #[test]
    fn a_large_key_or_a_broker_size_refusal_stops_without_retry_or_confirmation() {
        for (size, refuse_payload) in [(PAYLOAD_LIMIT, false), (1, true)] {
            let confirmed = AtomicU64::new(1);
            let mut input = Input {
                frames: [
                    ReplicationFrame::Begin {
                        txid: 1,
                        commit_ts: chrono::DateTime::UNIX_EPOCH,
                    },
                    ReplicationFrame::Row {
                        op: Op::Insert,
                        schema: "orders".into(),
                        table: "orders".into(),
                        relation_oid: 1,
                        old: None,
                        new: Some(
                            [("id".into(), "k".repeat(size).into())]
                                .into_iter()
                                .collect(),
                        ),
                        replica_key: Arc::default(),
                        unavailable: Vec::new(),
                    },
                    ReplicationFrame::Commit {
                        commit_lsn: 41,
                        end_lsn: 42,
                    },
                ]
                .into(),
                confirmed: &confirmed,
            };
            let config = ReaderConfig {
                target: Triple::new(
                    "acme".parse().unwrap(),
                    "dev".parse().unwrap(),
                    "orders".parse().unwrap(),
                ),
                stall_threshold: Duration::from_secs(30),
            };
            let publisher = Publisher {
                refuse_payload,
                ..Publisher::default()
            };
            let retry = async |_| panic!("a payload refusal cannot be retried");
            let mut reader = pin!(drain(&mut input, &config, &publisher, &|| false, &retry));
            let mut context = Context::from_waker(Waker::noop());
            let Poll::Ready(outcome) = reader.as_mut().poll(&mut context) else {
                panic!("a payload refusal must stop the reader");
            };
            assert_eq!(outcome.result.unwrap_err().fate, SessionFate::Fatal);
            assert_eq!(confirmed.load(Relaxed), 1);
            let sent = publisher.sent.lock().unwrap();
            if refuse_payload {
                assert_eq!(*sent, [0]);
            } else {
                assert!(sent.is_empty());
            }
        }
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
}
