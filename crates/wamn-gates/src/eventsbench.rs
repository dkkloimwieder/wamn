//! The `eventsbench` subcommand: the EVT-C1 retained-events-table campaign
//! (docs/event-plane-jetstream.md §10 C1 + §13) — the minimal Postgres
//! alternative to a JetStream event plane, measured to its knee. THE decision
//! input for the D19 checkpoint: C1's measured knee vs the target scale is the
//! honest crossover criterion that replaces the retracted "1–2k events/sec/org"
//! folklore in the §11 ledger.
//!
//! A MEASUREMENT campaign, not a regression gate: curves land in
//! `docs/ceilings.md` § C1 + `docs/ceilings-data/c1-*` (§11 provenance), and
//! only the sanity asserts gate — every event is delivered to every consumer
//! exactly once, in per-consumer seq order, completely at drain. Pure
//! host-side (raw `tokio_postgres`, no wasm): the prototype is a Postgres
//! mechanism.
//!
//! The prototype (§13 shape): an append-only `events` table (identity `seq`,
//! jsonb payload, hardened tenant floor) + one cursor ROW per consumer,
//! claimed by optimistic CAS — `UPDATE cursors SET last_seq = $new WHERE
//! consumer = $c AND last_seq = $old`. The batch read is DRIVEN BY the cursor
//! row (`WHERE seq > (SELECT last_seq FROM cursors …)`), so the row is
//! load-bearing: a restarted consumer resumes from it, and the CAS shape is
//! exactly how competing replicas of ONE logical consumer would share a
//! cursor (v1 is single-writer-per-cursor; the replica race is the noted
//! seam, not built).
//!
//! Modes:
//!   matrix — the trimmed first-pass §10 matrix: a find-knee ramp per cell,
//!            consumers {1,5,20} at 1 KiB payload plus payloads {16,64 KiB}
//!            at 5 consumers (the full cross-product is phase 2). Per level:
//!            achieved append rate, per-consumer delivery rate, append→read
//!            sojourn percentiles, WAL/event (insert-LSN deltas — never the
//!            flushed position), events relation size (TOTAL, so the 16/64 KiB
//!            cells capture the TOAST path), cursors dead tuples, end-of-window
//!            lag. The knee ramp uses the harness Ramp incl. the z7b.7
//!            retry-a-saturated-level-once noise defense.
//!   crud   — the co-resident interference probe at ONE operating point:
//!            app-path single-row INSERT/UPDATE latency (the C2 items shape,
//!            RLS-floored) alone vs beside the event plane at 80% of the
//!            5-consumer 1 KiB knee — the app-path p99 delta an org pays for
//!            running the event plane on its own instance.
//!   all    — matrix then crud (the probe's operating point comes from the
//!            measured knee).
//!
//! Not part of any `--mode all` regression suite: run it explicitly via
//! deploy/eventsbench-job.yaml.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};
use wamn_ddl::{Confirmation, Migration};
use wamn_gate_harness::{ceiling, check, emit_csv, percentile};

const SCHEMA: &str = "wamn_events_bench";
const TENANT: &str = "events-tenant";
/// The payload-axis cells run at this consumer count (the trimmed matrix
/// crosses each axis through the middle consumer cell).
const PAYLOAD_AXIS_CONSUMERS: usize = 5;

/// The co-resident app-CRUD table (the C2 items shape): a modest registered
/// entity under the REAL 3.2 tenant floor, so the interference probe measures
/// the production write path, not a bare table.
const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "c1-events-bench",
  "version": 1,
  "entities": [
    { "id": "items", "name": "items", "fields": [
      { "id": "sku", "name": "sku", "type": { "kind": "text", "max-len": 64 } },
      { "id": "qty", "name": "qty", "type": { "kind": "int" } },
      { "id": "price", "name": "price", "type": { "kind": "numeric", "precision": 12, "scale": 2 } },
      { "id": "flag", "name": "flag", "type": { "kind": "bool" } }
    ] }
  ]
}"#;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Matrix,
    Crud,
    All,
}

#[derive(Debug, Args)]
pub struct EventsBenchArgs {
    /// App (writer/consumer) Postgres URL — the NOSUPERUSER wamn_app role.
    /// Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema, VACUUM /
    /// CHECKPOINT, WAL LSN reads, the bloat probe.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which measurement to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Seconds of offered load per ramp level.
    #[arg(long, default_value_t = 60)]
    pub level_secs: u64,

    /// Ramp base rate, events/sec (the coarse doubling starts here).
    #[arg(long, default_value_t = 250.0)]
    pub base_rate: f64,

    /// Open-loop appender connections (each paces offered/n with catch-up).
    #[arg(long, default_value_t = 4)]
    pub producers: usize,

    /// Consumer-axis cells at 1 KiB payload, comma-separated consumer counts.
    #[arg(long, default_value = "1,5,20")]
    pub consumer_cells: String,

    /// Payload-axis cells at 5 consumers, comma-separated KiB. TOAST kicks in
    /// past ~2 KB, so these cells measure the TOAST path — deliberate.
    #[arg(long, default_value = "16,64")]
    pub payload_cells: String,

    /// Events per consumer batch read (the cursor-driven SELECT's LIMIT).
    #[arg(long, default_value_t = 64)]
    pub batch: usize,

    /// Crud probe: app-path single-row op rate (ops/sec, open-loop).
    #[arg(long, default_value_t = 50.0)]
    pub crud_rate: f64,

    /// Crud probe: event-plane append rate to run beside. Defaults to 80% of
    /// the measured 5-consumer 1 KiB knee (mode all); standalone `--mode crud`
    /// must pass it.
    #[arg(long)]
    pub probe_event_rate: Option<f64>,

    /// Also write each CSV to this directory (stdout always carries them
    /// between `=== BEGIN/END CSV <name> ===` markers).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// One matrix cell: N consumers reading `payload_kib` KiB events.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Cell {
    consumers: usize,
    payload_kib: usize,
}

impl Cell {
    fn label(self) -> String {
        format!("c{}-p{}k", self.consumers, self.payload_kib)
    }
}

/// Parse a comma-separated positive-integer cell list.
fn parse_cells(s: &str, what: &str) -> anyhow::Result<Vec<usize>> {
    let v: Vec<usize> = s
        .split(',')
        .map(|p| p.trim().parse::<usize>())
        .collect::<Result<_, _>>()
        .with_context(|| format!("bad --{what} {s:?} (want e.g. \"1,5,20\")"))?;
    if v.is_empty() || v.contains(&0) {
        bail!("--{what} needs non-zero entries");
    }
    Ok(v)
}

/// The trimmed first-pass matrix: the consumer axis at 1 KiB, then the
/// payload axis at [`PAYLOAD_AXIS_CONSUMERS`].
fn build_cells(consumer_cells: &[usize], payload_cells: &[usize]) -> Vec<Cell> {
    let mut cells: Vec<Cell> = consumer_cells
        .iter()
        .map(|&consumers| Cell {
            consumers,
            payload_kib: 1,
        })
        .collect();
    cells.extend(payload_cells.iter().map(|&payload_kib| Cell {
        consumers: PAYLOAD_AXIS_CONSUMERS,
        payload_kib,
    }));
    cells
}

/// A pseudo-random hex filler of `kib` KiB: LZ-incompressible (no long
/// repeats), so TOAST stores the 16/64 KiB payloads uncompressed and the
/// payload-axis cells measure the real TOAST write/read path — a repeated
/// constant would pglz-compress away the very cost being measured.
fn payload_filler(kib: usize) -> String {
    let mut s = String::with_capacity(kib * 1024);
    let mut x: u64 = 0x9e3779b97f4a7c15;
    while s.len() < kib * 1024 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.push_str(&format!("{x:016x}"));
    }
    s.truncate(kib * 1024);
    s
}

fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("bench catalog parse: {e}"))
}

/// The §13 prototype tables: append-only `events` (identity seq, jsonb
/// payload) + one cursor row per consumer, both under the hardened tenant
/// floor (the run-queue.sql shape).
fn events_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.events (\
            tenant_id text NOT NULL CHECK (tenant_id <> ''), \
            seq bigint GENERATED ALWAYS AS IDENTITY, \
            payload jsonb NOT NULL, \
            created_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, seq));\
         CREATE INDEX events_seq ON {schema}.events (seq);\
         ALTER TABLE {schema}.events ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.events FORCE ROW LEVEL SECURITY;\
         CREATE POLICY events_tenant ON {schema}.events \
            USING (tenant_id = NULLIF(current_setting('app.tenant', true), '')) \
            WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));\
         GRANT SELECT, INSERT ON {schema}.events TO wamn_app;\
         CREATE TABLE {schema}.cursors (\
            tenant_id text NOT NULL CHECK (tenant_id <> ''), \
            consumer text NOT NULL, \
            last_seq bigint NOT NULL DEFAULT 0, \
            PRIMARY KEY (tenant_id, consumer));\
         ALTER TABLE {schema}.cursors ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.cursors FORCE ROW LEVEL SECURITY;\
         CREATE POLICY cursors_tenant ON {schema}.cursors \
            USING (tenant_id = NULLIF(current_setting('app.tenant', true), '')) \
            WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));\
         GRANT SELECT, INSERT, UPDATE ON {schema}.cursors TO wamn_app;"
    )
}

/// Drop-and-recreate the ephemeral schema: the 3.2 floor for the CRUD table
/// plus the events/cursors prototype tables.
async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for ephemeral schema")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; CREATE SCHEMA {SCHEMA} AUTHORIZATION postgres; GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        let floor = Migration::create(&catalog()?)
            .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
            .sql(Confirmation::None)
            .map_err(|e| anyhow::anyhow!("floor sql: {e}"))?;
        client
            .batch_execute(&format!("SET search_path TO {SCHEMA}; {floor}"))
            .await
            .context("apply the 3.2 floor")?;
        client
            .batch_execute(&events_ddl(SCHEMA))
            .await
            .context("apply the events/cursors prototype")?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn teardown(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"))
        .await
        .map_err(|e| anyhow::anyhow!("drop ephemeral schema: {e}"));
    drop(client);
    let _ = conn_task.await;
    r.map(|_| ())
}

/// A long-lived admin connection (resets, VACUUM/CHECKPOINT, WAL LSNs, the
/// bloat probe), `search_path` pinned to the bench schema.
async fn connect_admin(admin_url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!("SET search_path TO {SCHEMA};"))
        .await?;
    Ok((client, handle))
}

/// A wamn_app connection pinned to the schema + tenant claim (the RLS floor
/// every appender/consumer/CRUD statement runs under).
async fn connect_app(app_url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!(
            "SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("set search_path + tenant claim")?;
    Ok((client, handle))
}

/// The instance WAL INSERT position (the C2 instrument lesson: never
/// `pg_current_wal_lsn()` — the flushed position reads ~0 under the fixture
/// pod's `synchronous_commit=off`; the insert position measures WAL
/// *generated* regardless of flush policy).
async fn wal_lsn(admin: &Client) -> anyhow::Result<String> {
    Ok(admin
        .query_one("SELECT pg_current_wal_insert_lsn()::text", &[])
        .await?
        .get(0))
}

/// WAL bytes generated since `before`.
async fn wal_since(admin: &Client, before: &str) -> anyhow::Result<i64> {
    Ok(admin
        .query_one(
            "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(), $1::text::pg_lsn)::bigint",
            &[&before],
        )
        .await?
        .get(0))
}

/// VACUUM (ANALYZE) every bench table, then CHECKPOINT — each level starts
/// from the same dead-tuple/FPI regime (each is its own simple-query round
/// trip: VACUUM can't run inside an implicit txn block).
async fn normalize(admin: &Client) -> anyhow::Result<()> {
    for table in ["events", "cursors", "\"items\""] {
        admin
            .batch_execute(&format!("VACUUM (ANALYZE) {SCHEMA}.{table}"))
            .await?;
    }
    admin.batch_execute("CHECKPOINT").await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// matrix: a find-knee ramp per cell
// ---------------------------------------------------------------------------

/// Shared state between the appenders and consumers of one level.
#[derive(Default)]
struct C1Shared {
    /// seq → append start. Inserted right after the appender's RETURNING, so
    /// a racing consumer may miss it — those lookups park in `pending` and
    /// resolve at level end (the map only grows during the window).
    enq_at: Mutex<HashMap<i64, Instant>>,
    /// (delivery instant, append→read sojourn) across ALL consumers. Pushed
    /// live (not task-local) so an aborted drain cannot lose window samples.
    samples: Mutex<Vec<(Instant, Duration)>>,
    /// Deliveries whose enq_at entry wasn't visible yet: (seq, delivered at).
    pending: Mutex<Vec<(i64, Instant)>>,
    /// Events appended (committed) this level.
    appended: AtomicU64,
    /// Per-consumer delivery counts (live, abort-safe).
    delivered: Vec<AtomicU64>,
    /// Per-consumer order/exactly-once violations: a delivered seq ≤ the
    /// consumer's prior position (a duplicate or a re-read).
    order_violations: AtomicU64,
    /// CAS advances that lost (v1 is single-writer, so expect 0 — reported,
    /// not gated: the counter is the replica-race seam's instrument).
    cas_lost: AtomicU64,
    /// false: keep polling; true: exit once caught up to `appended`.
    drain: AtomicBool,
}

impl C1Shared {
    fn for_consumers(n: usize) -> Self {
        C1Shared {
            delivered: (0..n).map(|_| AtomicU64::new(0)).collect(),
            ..Default::default()
        }
    }
}

/// Per-level extras beside the harness LevelStats (run order matches
/// `Ramp::levels`, retries included).
struct LevelExtras {
    wal_per_event: f64,
    /// pg_total_relation_size(events) — TOAST + indexes included, so the
    /// 16/64 KiB cells show the real on-disk growth.
    events_total_bytes: i64,
    cursors_dead_tup: i64,
    /// Backlog at window close: appended − min per-consumer delivered.
    lag_events: u64,
    cas_lost: u64,
}

/// Spawn the N cursor-claim consumers. Statements are prepared once per
/// connection; the batch read is driven by the CURSOR ROW (subselect), and the
/// advance is the optimistic CAS — a wrong or lost advance re-delivers, which
/// the order/exactly-once sanity catches.
fn spawn_consumers(
    set: &mut tokio::task::JoinSet<anyhow::Result<()>>,
    app_url: &str,
    n: usize,
    batch: usize,
    shared: &Arc<C1Shared>,
) {
    for idx in 0..n {
        let app_url = app_url.to_string();
        let shared = shared.clone();
        let name = format!("c-{idx}");
        set.spawn(async move {
            let (client, _h) = connect_app(&app_url).await?;
            // payload::text so the payload bytes actually cross the wire (the
            // fan-out read amplification IS the measured cost).
            let batch_stmt = client
                .prepare(&format!(
                    "SELECT seq, payload::text FROM events \
                      WHERE seq > (SELECT last_seq FROM cursors WHERE consumer = $1) \
                      ORDER BY seq LIMIT {batch}"
                ))
                .await?;
            let cas = client
                .prepare("UPDATE cursors SET last_seq = $2 WHERE consumer = $1 AND last_seq = $3")
                .await?;
            let reread = client
                .prepare("SELECT last_seq FROM cursors WHERE consumer = $1")
                .await?;
            // The consumer's view of its cursor (single-writer v1: always in
            // step with the row unless a CAS loses).
            let mut expected: i64 = 0;
            loop {
                let rows = client.query(&batch_stmt, &[&name]).await?;
                if rows.is_empty() {
                    if shared.drain.load(Ordering::Relaxed)
                        && shared.delivered[idx].load(Ordering::Relaxed)
                            >= shared.appended.load(Ordering::Relaxed)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    continue;
                }
                let mut new_last = expected;
                for row in &rows {
                    let seq: i64 = row.get(0);
                    let payload: String = row.get(1);
                    std::hint::black_box(payload.len());
                    if seq <= new_last {
                        shared.order_violations.fetch_add(1, Ordering::Relaxed);
                    } else {
                        new_last = seq;
                    }
                    let at = Instant::now();
                    match shared.enq_at.lock().unwrap().get(&seq) {
                        Some(t0) => shared.samples.lock().unwrap().push((at, at - *t0)),
                        None => shared.pending.lock().unwrap().push((seq, at)),
                    }
                    shared.delivered[idx].fetch_add(1, Ordering::Relaxed);
                }
                let won = client.execute(&cas, &[&name, &new_last, &expected]).await?;
                if won == 1 {
                    expected = new_last;
                } else {
                    shared.cas_lost.fetch_add(1, Ordering::Relaxed);
                    expected = client.query_one(&reread, &[&name]).await?.get(0);
                }
            }
            anyhow::Ok(())
        });
    }
}

/// Open-loop appenders: `producers_n` connections each pacing
/// `offered / producers_n` events/sec for `secs` with catch-up pacing (a slow
/// insert is followed by a burst back onto schedule — `offered` is a
/// schedule; divergence of the achieved rate is reported, not hidden).
async fn append_window(
    app_url: &str,
    offered: f64,
    secs: u64,
    producers_n: usize,
    filler: Arc<String>,
    shared: Arc<C1Shared>,
) -> anyhow::Result<()> {
    let mut set = tokio::task::JoinSet::new();
    for w in 0..producers_n {
        let app_url = app_url.to_string();
        let shared = shared.clone();
        let filler = filler.clone();
        let per_sec = offered / producers_n as f64;
        set.spawn(async move {
            let (client, _h) = connect_app(&app_url).await?;
            // The payload is assembled server-side (jsonb_build_object) so the
            // client sends the filler as plain text, once per event.
            let insert = client
                .prepare(
                    "INSERT INTO events (tenant_id, payload) \
                     VALUES (current_setting('app.tenant', true), \
                             jsonb_build_object('w', $1::bigint, 'n', $2::bigint, 'fill', $3::text)) \
                     RETURNING seq",
                )
                .await?;
            let window = Duration::from_secs(secs);
            let start = Instant::now();
            let mut sent = 0u64;
            while start.elapsed() < window {
                let due = (start.elapsed().as_secs_f64() * per_sec) as u64 + 1;
                while sent < due && start.elapsed() < window {
                    let t0 = Instant::now();
                    let seq: i64 = client
                        .query_one(&insert, &[&(w as i64), &(sent as i64), &filler.as_str()])
                        .await?
                        .get(0);
                    shared.enq_at.lock().unwrap().insert(seq, t0);
                    shared.appended.fetch_add(1, Ordering::Relaxed);
                    sent += 1;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            anyhow::Ok(())
        });
    }
    while let Some(r) = set.join_next().await {
        r??;
    }
    Ok(())
}

/// Join the consumers within `budget`; a timeout aborts them (the backlog
/// never drained — itself a saturation signal).
async fn drain_consumers(
    consumers: &mut tokio::task::JoinSet<anyhow::Result<()>>,
    budget: Duration,
) -> anyhow::Result<bool> {
    match tokio::time::timeout(budget, async {
        while let Some(r) = consumers.join_next().await {
            r??;
        }
        anyhow::Ok(())
    })
    .await
    {
        Ok(r) => {
            r?;
            Ok(true)
        }
        Err(_) => {
            consumers.abort_all();
            while consumers.join_next().await.is_some() {}
            Ok(false)
        }
    }
}

/// The C1 sanity asserts: per-consumer order/exactly-once (always) +
/// completeness at drain (every consumer delivered every appended event).
fn c1_sanity(shared: &C1Shared, drained: bool, context: &str) -> bool {
    let violations = shared.order_violations.load(Ordering::Relaxed);
    let appended = shared.appended.load(Ordering::Relaxed);
    let short: Vec<u64> = shared
        .delivered
        .iter()
        .map(|d| d.load(Ordering::Relaxed))
        .filter(|&d| d != appended)
        .collect();
    let ordered = violations == 0;
    let complete = !drained || short.is_empty();
    if !ordered || !complete {
        println!(
            "  SANITY FAIL ({context}): order/exactly-once violations={violations} \
             appended={appended} off-count consumers={short:?} drained={drained}"
        );
    }
    if !drained {
        println!(
            "  ({context}: drain timed out — completeness unchecked, level counted saturated)"
        );
    }
    ordered && complete
}

/// Reset the level: empty tables, reseeded cursor rows, normalized regime.
async fn reset_level(admin: &Client, consumers: usize) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "TRUNCATE {SCHEMA}.events RESTART IDENTITY; \
             TRUNCATE {SCHEMA}.cursors; \
             TRUNCATE {SCHEMA}.\"items\"; \
             INSERT INTO {SCHEMA}.cursors (tenant_id, consumer, last_seq) \
             SELECT '{TENANT}', 'c-' || g, 0 FROM generate_series(0, {n}) g;",
            n = consumers as i64 - 1
        ))
        .await
        .context("reset level")?;
    normalize(admin).await
}

/// One ramp level for a cell: `level_secs` of offered append load against N
/// live consumers, then a drain. Stats cover the offered window only.
async fn c1_level(
    app_url: &str,
    admin: &Client,
    cell: Cell,
    filler: &Arc<String>,
    offered: f64,
    args: &EventsBenchArgs,
    tag: &str,
) -> anyhow::Result<(ceiling::LevelStats, LevelExtras, bool)> {
    reset_level(admin, cell.consumers).await?;
    let shared = Arc::new(C1Shared::for_consumers(cell.consumers));
    let mut consumers = tokio::task::JoinSet::new();
    spawn_consumers(&mut consumers, app_url, cell.consumers, args.batch, &shared);

    let wal0 = wal_lsn(admin).await?;
    let window_start = Instant::now();
    append_window(
        app_url,
        offered,
        args.level_secs,
        args.producers,
        filler.clone(),
        shared.clone(),
    )
    .await?;
    let window_end = Instant::now();
    let appended = shared.appended.load(Ordering::Relaxed);
    let lag_events = appended
        - shared
            .delivered
            .iter()
            .map(|d| d.load(Ordering::Relaxed))
            .min()
            .unwrap_or(0)
            .min(appended);
    shared.drain.store(true, Ordering::Relaxed);
    let drained = drain_consumers(
        &mut consumers,
        Duration::from_secs(args.level_secs * 3 + 60),
    )
    .await?;
    let wal = wal_since(admin, &wal0).await?;

    // Resolve deliveries that raced the appender's enq_at insert (the map is
    // complete now), then window-filter the sojourns.
    {
        let enq_at = shared.enq_at.lock().unwrap();
        let mut samples = shared.samples.lock().unwrap();
        for (seq, at) in shared.pending.lock().unwrap().drain(..) {
            if let Some(t0) = enq_at.get(&seq) {
                samples.push((at, at - *t0));
            }
        }
    }
    let window = (window_end - window_start).as_secs_f64();
    let mut window_sojourns: Vec<Duration> = {
        let samples = shared.samples.lock().unwrap();
        samples
            .iter()
            .filter(|(at, _)| *at <= window_end)
            .map(|(_, d)| *d)
            .collect()
    };
    window_sojourns.sort();
    let wc = window_sojourns.len() as u64;
    let stats = ceiling::LevelStats {
        offered,
        achieved_enqueue: appended as f64 / window,
        // Per-consumer delivery rate: directly comparable to the append rate,
        // so consumer lag registers as divergence (a saturation signal).
        achieved_complete: wc as f64 / cell.consumers as f64 / window,
        p50_ms: percentile(&window_sojourns, 0.50).as_secs_f64() * 1e3,
        p99_ms: percentile(&window_sojourns, 0.99).as_secs_f64() * 1e3,
        p999_ms: percentile(&window_sojourns, 0.999).as_secs_f64() * 1e3,
        window_completed: wc,
        drained,
    };
    let probe = admin
        .query_one(
            &format!(
                "SELECT pg_total_relation_size('{SCHEMA}.events'), \
                        COALESCE((SELECT n_dead_tup FROM pg_stat_all_tables \
                                   WHERE schemaname = '{SCHEMA}' AND relname = 'cursors'), 0)::bigint"
            ),
            &[],
        )
        .await?;
    let extras = LevelExtras {
        wal_per_event: if appended == 0 {
            0.0
        } else {
            wal as f64 / appended as f64
        },
        events_total_bytes: probe.get(0),
        cursors_dead_tup: probe.get(1),
        lag_events,
        cas_lost: shared.cas_lost.load(Ordering::Relaxed),
    };
    let sanity = c1_sanity(&shared, drained, tag);
    Ok((stats, extras, sanity))
}

/// The per-cell ramp CSV: the harness LevelStats columns plus the C1 extras,
/// one row per RUN in run order (a z7b.7 retry appears as a second row at the
/// same offered rate).
fn cell_csv(levels: &[ceiling::LevelStats], extras: &[LevelExtras]) -> String {
    let mut out = String::from(
        "offered_per_s,achieved_append_per_s,delivered_per_consumer_per_s,\
         p50_ms,p99_ms,p999_ms,window_delivered,drained,\
         wal_bytes_per_event,events_total_bytes,cursors_dead_tup,lag_events,cas_lost\n",
    );
    for (s, e) in levels.iter().zip(extras) {
        out.push_str(&format!(
            "{:.0},{:.1},{:.1},{:.3},{:.3},{:.3},{},{},{:.0},{},{},{},{}\n",
            s.offered,
            s.achieved_enqueue,
            s.achieved_complete,
            s.p50_ms,
            s.p99_ms,
            s.p999_ms,
            s.window_completed,
            s.drained,
            e.wal_per_event,
            e.events_total_bytes,
            e.cursors_dead_tup,
            e.lag_events,
            e.cas_lost
        ));
    }
    out
}

/// Drive the find-knee ramp for one cell (the z7b.7 retry-robust controller).
/// Returns the ramp (knee + curve) and the cell's sanity verdict.
async fn c1_ramp(
    app_url: &str,
    admin: &Client,
    cell: Cell,
    args: &EventsBenchArgs,
) -> anyhow::Result<(ceiling::Ramp, Vec<LevelExtras>, bool)> {
    println!(
        "\n### ramp — {} ({} consumers × {} KiB payload)",
        cell.label(),
        cell.consumers,
        cell.payload_kib
    );
    let filler = Arc::new(payload_filler(cell.payload_kib));
    // A discarded warmup level: the first MEASURED level is the p99-doubling
    // baseline, so it must not carry cold-cache noise.
    let warm_tag = format!("{}-warm", cell.label());
    let (_, _, warm_ok) = c1_level(
        app_url,
        admin,
        cell,
        &filler,
        args.base_rate,
        args,
        &warm_tag,
    )
    .await?;
    let mut ramp = ceiling::Ramp::new(args.base_rate, 0.15, 16);
    let mut extras = Vec::new();
    let mut sanity = warm_ok;
    while let Some(offered) = ramp.next_offered() {
        let tag = format!("{}-{offered:.0}", cell.label());
        let (stats, extra, ok) =
            c1_level(app_url, admin, cell, &filler, offered, args, &tag).await?;
        println!(
            "  offered {:>7.0}/s | append {:>7.1}/s | deliver {:>7.1}/s/consumer | \
             p50 {:>8.2}ms p99 {:>8.2}ms | lag {:>6} | wal/ev {:>6.0}B | drained={}{}",
            stats.offered,
            stats.achieved_enqueue,
            stats.achieved_complete,
            stats.p50_ms,
            stats.p99_ms,
            extra.lag_events,
            extra.wal_per_event,
            stats.drained,
            if stats.achieved_enqueue < 0.95 * stats.offered {
                " (producer-limited)"
            } else {
                ""
            }
        );
        sanity &= ok;
        extras.push(extra);
        ramp.record(stats);
    }
    match ramp.knee() {
        Some(k) => println!(
            "  knee({}) = {:.0}/s offered → {:.0}/s sustained appends, {:.0}/s/consumer delivered",
            cell.label(),
            k.offered,
            k.achieved_enqueue,
            k.achieved_complete
        ),
        None => println!(
            "  knee({}) = below the base rate {:.0}/s",
            cell.label(),
            args.base_rate
        ),
    }
    Ok((ramp, extras, sanity))
}

// ---------------------------------------------------------------------------
// crud: the co-resident app-path interference probe
// ---------------------------------------------------------------------------

/// The app-path single-row INSERT/UPDATE mix (the C2 items shape) at
/// `rate` ops/sec for `secs`; per-op latency samples.
async fn crud_driver(app_url: &str, rate: f64, secs: u64) -> anyhow::Result<Vec<Duration>> {
    let (client, _h) = connect_app(app_url).await?;
    let insert = client
        .prepare(
            "INSERT INTO \"items\" (tenant_id, sku, qty, price, flag) \
             VALUES (current_setting('app.tenant', true), $1, $2, '12.50', false) \
             RETURNING id::text",
        )
        .await?;
    // `$1::text::uuid`, never a bare `$1::uuid` (the bind-cast lesson).
    let update = client
        .prepare("UPDATE \"items\" SET qty = qty + 1 WHERE id = $1::text::uuid")
        .await?;
    let window = Duration::from_secs(secs);
    let start = Instant::now();
    let mut samples = Vec::new();
    let mut last_id: Option<String> = None;
    let mut sent = 0u64;
    while start.elapsed() < window {
        let due = (start.elapsed().as_secs_f64() * rate) as u64 + 1;
        while sent < due && start.elapsed() < window {
            let t0 = Instant::now();
            match &last_id {
                // Alternate: INSERT a row, then UPDATE it.
                None => {
                    let id: String = client
                        .query_one(&insert, &[&format!("sku-{sent}"), &(sent as i32)])
                        .await?
                        .get(0);
                    last_id = Some(id);
                }
                Some(id) => {
                    client.execute(&update, &[id]).await?;
                    last_id = None;
                }
            }
            samples.push(t0.elapsed());
            sent += 1;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    samples.sort();
    Ok(samples)
}

/// The interference probe: CRUD alone, then CRUD beside the event plane at
/// `event_rate` with [`PAYLOAD_AXIS_CONSUMERS`] consumers × 1 KiB.
async fn crud_phase(
    app_url: &str,
    admin: &Client,
    args: &EventsBenchArgs,
    event_rate: f64,
) -> anyhow::Result<bool> {
    println!(
        "\n## crud (C1) — app-path p99 delta beside the event plane \
         ({:.0} CRUD ops/s; event side {:.0}/s × {PAYLOAD_AXIS_CONSUMERS} consumers × 1 KiB)",
        args.crud_rate, event_rate
    );
    let cell = Cell {
        consumers: PAYLOAD_AXIS_CONSUMERS,
        payload_kib: 1,
    };
    let mut pass = true;
    let mut csv = String::from("phase,event_rate_per_s,crud_rate_per_s,ops,p50_ms,p99_ms\n");
    let mut measured: Vec<(f64, f64)> = Vec::new();
    for (phase, with_events) in [("baseline", false), ("co-resident", true)] {
        let samples = if with_events {
            let crud = {
                let app_url = app_url.to_string();
                let rate = args.crud_rate;
                let secs = args.level_secs;
                tokio::spawn(async move { crud_driver(&app_url, rate, secs).await })
            };
            let filler = Arc::new(payload_filler(cell.payload_kib));
            let tag = format!("crud-co-resident-{event_rate:.0}");
            let (_, _, ok) =
                c1_level(app_url, admin, cell, &filler, event_rate, args, &tag).await?;
            pass &= ok;
            crud.await??
        } else {
            reset_level(admin, cell.consumers).await?;
            crud_driver(app_url, args.crud_rate, args.level_secs).await?
        };
        let p50 = percentile(&samples, 0.50).as_secs_f64() * 1e3;
        let p99 = percentile(&samples, 0.99).as_secs_f64() * 1e3;
        println!(
            "  {phase:<12} {} ops  p50 {:>7.3}ms  p99 {:>7.3}ms",
            samples.len(),
            p50,
            p99
        );
        csv.push_str(&format!(
            "{phase},{:.0},{:.0},{},{:.3},{:.3}\n",
            if with_events { event_rate } else { 0.0 },
            args.crud_rate,
            samples.len(),
            p50,
            p99
        ));
        check(
            &mut pass,
            &format!(
                "crud {phase}: the probe sampled ops (got {})",
                samples.len()
            ),
            !samples.is_empty(),
        );
        measured.push((p50, p99));
    }
    if let [(b50, b99), (c50, c99)] = measured[..] {
        println!(
            "  app-path delta beside the event plane: p50 {:+.3}ms, p99 {:+.3}ms",
            c50 - b50,
            c99 - b99
        );
    }
    emit_csv("c1-crud-probe", &csv, &args.out);
    Ok(pass)
}

// ---------------------------------------------------------------------------

pub async fn run(args: EventsBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "eventsbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;
    let consumer_cells = parse_cells(&args.consumer_cells, "consumer-cells")?;
    let payload_cells = parse_cells(&args.payload_cells, "payload-cells")?;

    println!("# wamn-gates EVT-C1 eventsbench (schema {SCHEMA}, tenant {TENANT})");
    provision(&admin_url)
        .await
        .context("provision ephemeral schema")?;

    let run_all = args.mode == Mode::All;
    let mut pass = true;
    let outcome = async {
        let (admin, _ah) = connect_admin(&admin_url).await?;
        // The 5-consumer 1 KiB knee: the crud probe's operating-point source.
        let mut probe_knee: Option<f64> = None;
        if run_all || args.mode == Mode::Matrix {
            println!(
                "\n## matrix (C1) — find-knee ramp per cell (measurement; only sanity asserts gate)\n\
                 #  {}s levels, base {:.0}/s, {} appenders, batch {}",
                args.level_secs, args.base_rate, args.producers, args.batch
            );
            for cell in build_cells(&consumer_cells, &payload_cells) {
                let (ramp, extras, ok) = c1_ramp(&app_url, &admin, cell, &args).await?;
                pass &= ok;
                emit_csv(
                    &format!("c1-ramp-{}", cell.label()),
                    &cell_csv(ramp.levels(), &extras),
                    &args.out,
                );
                if cell.consumers == PAYLOAD_AXIS_CONSUMERS && cell.payload_kib == 1 {
                    probe_knee = ramp.knee().map(|k| k.achieved_enqueue);
                }
            }
        }
        if run_all || args.mode == Mode::Crud {
            let event_rate = match args.probe_event_rate.or(probe_knee.map(|k| 0.8 * k)) {
                Some(r) => r,
                None => bail!(
                    "--mode crud needs --probe-event-rate (or run --mode all so the \
                     5-consumer 1 KiB knee provides it)"
                ),
            };
            pass &= crud_phase(&app_url, &admin, &args, event_rate).await?;
        }
        anyhow::Ok(())
    }
    .await;

    let _ = teardown(&admin_url).await;
    outcome?;

    println!("\neventsbench complete — overall PASS: {pass}");
    if !pass {
        bail!("an EVT-C1 sanity assert failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_lists_parse_and_reject_junk() {
        assert_eq!(
            parse_cells("1,5,20", "consumer-cells").unwrap(),
            vec![1, 5, 20]
        );
        assert_eq!(
            parse_cells(" 16 , 64 ", "payload-cells").unwrap(),
            vec![16, 64]
        );
        assert!(parse_cells("", "consumer-cells").is_err());
        assert!(parse_cells("1,0", "consumer-cells").is_err());
        assert!(parse_cells("1,x", "consumer-cells").is_err());
    }

    #[test]
    fn the_trimmed_matrix_crosses_both_axes_through_the_middle_cell() {
        let cells = build_cells(&[1, 5, 20], &[16, 64]);
        let labels: Vec<String> = cells.iter().map(|c| c.label()).collect();
        assert_eq!(
            labels,
            vec!["c1-p1k", "c5-p1k", "c20-p1k", "c5-p16k", "c5-p64k"]
        );
    }

    #[test]
    fn payload_filler_is_sized_and_lz_incompressible() {
        let f = payload_filler(16);
        assert_eq!(f.len(), 16 * 1024);
        // No long repeats: the two halves differ (a constant filler would
        // pglz-compress the TOAST cost away).
        let (a, b) = f.split_at(f.len() / 2);
        assert_ne!(a, b);
        assert!(f.is_ascii());
    }

    #[test]
    fn prototype_ddl_and_catalog_compile() {
        let ddl = events_ddl(SCHEMA);
        for needle in [
            "GENERATED ALWAYS AS IDENTITY",
            "FORCE ROW LEVEL SECURITY",
            "PRIMARY KEY (tenant_id, seq)",
            "PRIMARY KEY (tenant_id, consumer)",
            "NULLIF(current_setting('app.tenant', true), '')",
        ] {
            assert!(ddl.contains(needle), "events DDL lost {needle:?}");
        }
        let floor = Migration::create(&catalog().expect("catalog parses"))
            .unwrap()
            .sql(Confirmation::None)
            .unwrap();
        assert!(floor.contains("CREATE TABLE \"items\""));
    }
}
