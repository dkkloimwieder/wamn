//! The `e2ebench` subcommand: the Phase-2 event-plane before/after campaign
//! (wamn-l5i9.22 [EVT-C-E2E], D19 v3 §7/§8 — MEASURED, not gated).
//!
//! This is the C-E2E ceiling bench: the one before/after chart that justifies
//! or indicts the CDC event plane vs the OLD outbox path. It composes BOTH real
//! paths in one process (the cutbench substrate) at identical write load from
//! the same writer program:
//!
//!   OLD path — a `wamn_app` write to an `old_*` table carrying the REAL
//!   `Migration::outbox_triggers` fires the real `wamn_dispatcher` engine
//!   (poll/match/fire/enqueue) into `wamn_run.run_queue` (run id
//!   `{flow}:outbox:{seq}`).
//!
//!   NEW path — a `wamn_app` write to a `new_*` table with NO trigger is
//!   captured by the embedded `wamn-cdc-reader` (one `pg_walstream` session →
//!   JetStream), materialized by the real `materializer.wasm` guest into the
//!   same run_queue (run id `{flow}:evt:{stream_seq}`).
//!
//! Old-arm tables carry the trigger; new-arm tables do not (the post-teardown
//! CDC world, where §3 removes triggers) — so the app-transaction cost is
//! measured honestly per path. Old-arm flows have NO live CDC registration (the
//! dispatcher fires them); new-arm flows have live registrations (the
//! dispatcher yields them — moot, since a trigger-free table produces no outbox
//! rows — and the materializer fires them). The two arms live in disjoint
//! entity/table/flow sets so both run in one provision. Runs are attributed by
//! run-id namespace (`:outbox:` vs `:evt:`, disjoint by design).
//!
//! Measurements (each vs BOTH paths):
//!   a. commit→run-start distribution at two steady rates (p50/p90/p99 + a
//!      latency histogram). Commit instant = `clock_timestamp()` returned by the
//!      writing INSERT; enqueue instant = `run_queue.enqueued_at` — BOTH the same
//!      Postgres wall clock (the throwaway PG shares the host kernel clock, so
//!      there is no client/server skew to correct). Latency = enqueued − commit.
//!   b. fan-out 1→N (N = 1, 5, 20): one logical event fires N flows. Headline =
//!      app-transaction commit latency at each N per path (old pays the outbox
//!      trigger tax; new pays nothing) + commit→last-run-enqueued for the N runs.
//!   c. burst: 10× the steady rate for a bounded window → queue/lag depth over
//!      time (outbox unfired backlog vs JetStream consumer pending), drain time,
//!      and app-write-path latency DURING the drain, both paths.
//!
//! Records: CSV under `docs/ceilings-data/ce2e-*.csv` + `docs/ceilings.md`
//! § C-E2E. CEILING bench — curves/knees, provenance-labelled; only structural
//! sanity asserts gate (so the numbers are trustworthy). Recipe:
//! docs/build-and-test.md [EVT-C-E2E]. Local provenance is METHODOLOGY
//! VALIDATION (debug host binaries, fsync=off throwaway PG); the rows of record
//! come from the in-cluster release-image job.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;

use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wash_runtime::wasmtime::{Engine as RawEngine, Store};
use wasmtime_wasi::p2::bindings::CommandPre;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

use pg_walstream::CancellationToken;
use wamn_cdc_reader::{EventReaderArgs, run_with_token};
use wamn_ddl::{Confirmation, Migration, OutboxOptions};
use wamn_dispatcher::{Dispatcher, DispatcherConfig, ProjectSpec, epoch_ms};
use wamn_gate_harness::emit_csv;
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_jetstream::{
    self, WAMN_JETSTREAM_ID, WamnJetstream, WamnJetstreamConfig,
};
use wamn_host::plugins::wamn_postgres::{self, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig};
use wamn_provision::{cdc_object_name, event_stream_name, sql as provision_sql};
use wamn_registry::sql::{
    upsert_event_reader_sql, upsert_org_sql, upsert_project_env_sql, upsert_project_sql,
};

#[derive(Debug, Args)]
pub struct E2eBenchArgs {
    /// The compiled materializer component.
    #[arg(long, default_value = "/bench/materializer.wasm")]
    pub component: PathBuf,

    /// SUPERUSER URL (path `/postgres`) on a `wal_level=logical` Postgres — the
    /// bench owns the throwaway `wamn_e2ebench` database, slot, and role.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// JetStream-enabled NATS (the reader's EVT stream).
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    pub nats_url: String,

    /// Which phases to run: all | dist | fanout | burst (comma-separated ok).
    #[arg(long, default_value = "all")]
    pub phase: String,

    /// distribution steady rates (events/sec), comma-separated.
    #[arg(long, default_value = "10,50")]
    pub dist_rates: String,

    /// distribution: seconds of steady load per rate.
    #[arg(long, default_value_t = 12)]
    pub dist_secs: u64,

    /// fan-out: logical events per N level.
    #[arg(long, default_value_t = 24)]
    pub fanout_events: usize,

    /// fan-out: offered event rate (events/sec).
    #[arg(long, default_value_t = 10.0)]
    pub fanout_rate: f64,

    /// burst: steady baseline rate (events/sec).
    #[arg(long, default_value_t = 20.0)]
    pub burst_steady: f64,

    /// burst: spike multiplier over the steady rate.
    #[arg(long, default_value_t = 10.0)]
    pub burst_mult: f64,

    /// burst: warmup seconds at the steady rate before the spike.
    #[arg(long, default_value_t = 4)]
    pub burst_warmup_secs: u64,

    /// burst: spike window seconds.
    #[arg(long, default_value_t = 3)]
    pub burst_spike_secs: u64,

    /// burst: seconds to observe drain after the spike (still writing steady).
    #[arg(long, default_value_t = 14)]
    pub burst_drain_secs: u64,

    /// dispatcher poll cadence, ms (the OLD path's dominant latency term —
    /// production min interval is 250 ms; a tighter value here surfaces the
    /// structural floor). Recorded in provenance.
    #[arg(long, default_value_t = 50)]
    pub disp_poll_ms: u64,

    /// materializer fetch long-poll window per registration, ms (the NEW path's
    /// pacing term). Recorded in provenance.
    #[arg(long, default_value_t = 100)]
    pub fetch_ms: u64,

    /// Also write each CSV to this directory (stdout always carries them).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

const BENCH_ID: &str = "e2ebench";
const DB: &str = "wamn_e2ebench";
const ORG: &str = "e2e";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const TENANT: &str = "t1";
const CDC_PW: &str = "wamn_cdc_pw";
const CATALOG_ID: &str = "e2ecat";

/// Fan-out levels (N flows per one logical event). N=1 is reused by the
/// distribution + burst phases.
const FANOUT: [usize; 3] = [1, 5, 20];

// The REAL shipped DDL, compiled in — the gate cannot drift from deploy/sql.
const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

// ---------------------------------------------------------------------------
// Path attribution (load-bearing: the run-id namespaces are disjoint by design
// — {flow}:outbox:{seq} vs {flow}:evt:{stream_seq}). Kept pure + unit-tested so
// a broken attribution predicate dies at `cargo test`, not in a silent miscount.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    Old,
    New,
}

impl Path {
    fn like(self) -> &'static str {
        match self {
            Path::Old => "%:outbox:%",
            Path::New => "%:evt:%",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Path::Old => "old-outbox",
            Path::New => "new-cdc",
        }
    }
}

/// Classify a run id by its namespace token. `None` = neither (a
/// misattribution — the bench treats it as a fatal sanity break).
fn classify_run_id(run_id: &str) -> Option<Path> {
    if run_id.contains(":outbox:") {
        Some(Path::Old)
    } else if run_id.contains(":evt:") {
        Some(Path::New)
    } else {
        None
    }
}

/// Fan-out sanity: every observed event fired exactly `want` runs (no event
/// under- or over-fired). Empty = nothing observed = not ok.
fn all_fanned_out(counts: &[usize], want: usize) -> bool {
    !counts.is_empty() && counts.iter().all(|&c| c == want)
}

// ---------------------------------------------------------------------------
// Stats over f64 latency samples (ms)
// ---------------------------------------------------------------------------

fn pctl(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx]
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// Fixed-edge latency histogram (ms). Edges are upper-exclusive; the last
/// bucket is the overflow (edge..∞).
const HIST_EDGES: [f64; 7] = [10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0];

fn histogram(samples: &[f64]) -> [u64; 8] {
    let mut buckets = [0u64; 8];
    for &s in samples {
        let mut b = HIST_EDGES.len();
        for (i, &edge) in HIST_EDGES.iter().enumerate() {
            if s < edge {
                b = i;
                break;
            }
        }
        buckets[b] += 1;
    }
    buckets
}

// ---------------------------------------------------------------------------
// Small helpers COPIED from cutbench.rs (l5i9.18) — the reader-live-test
// idioms. Copied per the l5i9.22 owner directive (do not refactor cutbench.rs;
// a sibling lane edits it): swap_db / role_url / connect / scalar / the guest
// Harness. Kept verbatim in shape so both benches drift together only by intent.
// ---------------------------------------------------------------------------

fn swap_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has a path");
    format!("{base}/{db}")
}

fn role_url(super_url: &str, role: &str, pw: &str) -> String {
    let after_scheme = super_url.strip_prefix("postgres://").expect("postgres://");
    let (_, host_and_path) = after_scheme.rsplit_once('@').expect("url has userinfo");
    let (host_port, _) = host_and_path.split_once('/').expect("url has a path");
    format!("postgres://{role}:{pw}@{host_port}/{DB}")
}

async fn connect(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("connect {url}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(client)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn entity_id(arm: &str, n: usize) -> String {
    format!("{arm}_e{n}")
}
fn table_name(arm: &str, n: usize) -> String {
    format!("{arm}_events_{n}")
}
fn flow_id(arm: &str, n: usize, i: usize) -> String {
    // Flow ids are lowercase slugs [a-z0-9-] (the run-id charset rule) — hyphen,
    // never underscore.
    format!("{arm}f{n}-{i}")
}
fn reg_id(n: usize, i: usize) -> String {
    format!("nr{n}-{i}")
}

/// The 6-entity catalog: {old,new} × {1,5,20}. Each entity is a small row
/// (site + tag text) over the REAL 3.2 floor.
fn build_catalog(arms: &[&str]) -> anyhow::Result<wamn_catalog::Catalog> {
    let mut entities = Vec::new();
    for &arm in arms {
        for &n in &FANOUT {
            entities.push(serde_json::json!({
                "id": entity_id(arm, n),
                "name": table_name(arm, n),
                "fields": [
                    { "id": "site", "name": "site", "type": { "kind": "text" } },
                    { "id": "tag",  "name": "tag",  "type": { "kind": "text" } }
                ]
            }));
        }
    }
    let doc = serde_json::json!({
        "schema-version": "0.1",
        "catalog-id": CATALOG_ID,
        "version": 1,
        "entities": entities,
    });
    wamn_catalog::Catalog::from_json(&doc.to_string())
        .map_err(|e| anyhow::anyhow!("e2ebench catalog parse: {e}"))
}

fn row_event_flow_json(flow: &str, table: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1", "flow-id": flow, "version": 1,
        "trigger": {"type": "row-event", "table": table, "event": "insert"},
        "entry": "n1", "nodes": [{"id": "n1", "type": "noop"}],
    })
    .to_string()
}

/// A live registration (state omitted = live, per the 0.1.x additive rule).
fn registration_json(reg: &str, flow: &str, entity: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "registration-id": reg,
        "catalog-id": CATALOG_ID,
        "flow-id": flow,
        "entity": entity,
        "ops": ["insert"],
        "condition": serde_json::Value::Null,
        "partition-key": serde_json::Value::Null,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// The guest harness (the matbench/cutbench shape — COPIED, per the l5i9.22
// directive; parametrised on the sweep windows the bench sweeps over).
// ---------------------------------------------------------------------------

struct Harness {
    engine: wash_runtime::engine::Engine,
    pre: CommandPre<SharedCtx>,
    pg: Arc<WamnPostgres>,
    js: Arc<WamnJetstream>,
    report_dir: PathBuf,
    stream_name: String,
}

impl Harness {
    fn plugin_map(
        &self,
    ) -> std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> {
        let mut m: std::collections::HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> =
            std::collections::HashMap::new();
        m.insert(WAMN_POSTGRES_ID, self.pg.clone());
        m.insert(WAMN_JETSTREAM_ID, self.js.clone());
        m
    }

    async fn run_guest(
        &self,
        max_sweeps: u64,
        batch: u32,
        fetch_ms: u64,
    ) -> anyhow::Result<serde_json::Value> {
        let report_path = self.report_dir.join("counters.json");
        let _ = std::fs::remove_file(&report_path);

        let mut wasi = WasiCtxBuilder::new();
        wasi.args(&["materializer.wasm"])
            .inherit_stdout()
            .inherit_stderr()
            .envs(&[
                ("WAMN_MAT_STREAM", self.stream_name.as_str()),
                ("WAMN_MAT_ORG", ORG),
                ("WAMN_MAT_PROJECT", PROJECT),
                ("WAMN_MAT_ENV", ENV),
                ("WAMN_MAT_TENANT", TENANT),
                ("WAMN_MAT_BATCH", &batch.to_string()),
                ("WAMN_MAT_FETCH_MS", &fetch_ms.to_string()),
                ("WAMN_MAT_SWEEP_MS", "0"),
                ("WAMN_MAT_MAX_SWEEPS", &max_sweeps.to_string()),
                ("WAMN_MAT_ACK_WAIT_MS", "30000"),
                ("WAMN_MAT_NACK_DELAY_MS", "500"),
                ("WAMN_MAT_REPORT_PATH", "/report/counters.json"),
            ])
            .preopened_dir(
                &self.report_dir,
                "/report",
                DirPerms::all(),
                FilePerms::all(),
            )
            .map_err(|e| anyhow::anyhow!("preopen report dir: {e}"))?;

        let ctx = Ctx::builder(BENCH_ID.to_string(), BENCH_ID.to_string())
            .with_plugins(self.plugin_map())
            .with_wasi_ctx(wasi.build())
            .build();
        let mut store = Store::new(self.engine.inner(), SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);

        let cmd = self
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| anyhow::anyhow!("instantiate materializer: {e}"))?;
        let outcome = tokio::time::timeout(
            Duration::from_secs(180),
            cmd.wasi_cli_run().call_run(&mut store),
        )
        .await
        .context("materializer run deadline (180s) exceeded")?
        .map_err(|e| anyhow::anyhow!("materializer run trapped: {e}"))?;
        if outcome.is_err() {
            bail!("materializer exited with error status");
        }
        let raw = std::fs::read_to_string(&report_path)
            .with_context(|| format!("read guest report {}", report_path.display()))?;
        serde_json::from_str(&raw).context("parse guest report")
    }
}

// ---------------------------------------------------------------------------
// The single traffic source: a wamn_app writer under the tenant claim.
// ---------------------------------------------------------------------------

struct Writer {
    app: tokio_postgres::Client,
}

impl Writer {
    /// Insert one row into `table`; returns (row id, commit-instant ms). The
    /// commit instant is `clock_timestamp()` evaluated during the INSERT — a
    /// hair before the autocommit finalises, the SAME Postgres wall clock the
    /// run_queue's `enqueued_at` default reads.
    async fn insert(&self, table: &str, site: &str, tag: &str) -> anyhow::Result<(String, f64)> {
        let sql = format!(
            "INSERT INTO \"{table}\" (tenant_id, site, tag) \
             VALUES (current_setting('app.tenant', true), $1, $2) \
             RETURNING id::text, (extract(epoch from clock_timestamp())*1000)::float8"
        );
        let row = self
            .app
            .query_one(&sql, &[&site, &tag])
            .await
            .with_context(|| format!("insert into {table}"))?;
        Ok((row.get(0), row.get(1)))
    }
}

/// One enqueued run row as observed after a phase: (payload id, enqueued ms).
async fn enqueued_rows(
    db: &tokio_postgres::Client,
    path: Path,
) -> anyhow::Result<Vec<(String, f64)>> {
    let rows = db
        .query(
            "SELECT r.run_id, r.input_json->'payload'->>'id', \
                    (extract(epoch from q.enqueued_at)*1000)::float8 \
             FROM wamn_run.runs r \
             JOIN wamn_run.run_queue q ON q.tenant_id = r.tenant_id AND q.run_id = r.run_id \
             WHERE r.tenant_id = $1 AND r.run_id LIKE $2",
            &[&TENANT, &path.like()],
        )
        .await
        .context("read enqueued rows")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let run_id: String = row.get(0);
        // Load-bearing cross-check: the LIKE filter and the namespace token must
        // agree — a mislabelled run is a fatal attribution break.
        if classify_run_id(&run_id) != Some(path) {
            bail!(
                "run {run_id} does not classify as {:?} — path attribution broke",
                path
            );
        }
        let id: Option<String> = row.get(1);
        let ms: f64 = row.get(2);
        if let Some(id) = id {
            out.push((id, ms));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Registration set management (control the materializer's live-registration
// count per phase so its per-sweep cost stays bounded).
// ---------------------------------------------------------------------------

async fn add_new_regs(db: &tokio_postgres::Client, n: usize) -> anyhow::Result<()> {
    let entity = entity_id("new", n);
    for i in 0..n {
        let reg = reg_id(n, i);
        let flow = flow_id("new", n, i);
        db.execute(
            "INSERT INTO catalog.event_registrations \
             (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb) \
             ON CONFLICT (tenant_id, catalog_id, registration_id) DO NOTHING",
            &[
                &TENANT,
                &CATALOG_ID,
                &reg,
                &flow,
                &entity,
                &registration_json(&reg, &flow, &entity),
            ],
        )
        .await
        .with_context(|| format!("add registration {reg}"))?;
    }
    Ok(())
}

async fn del_new_regs(db: &tokio_postgres::Client, n: usize) -> anyhow::Result<()> {
    for i in 0..n {
        let reg = reg_id(n, i);
        db.execute(
            "DELETE FROM catalog.event_registrations \
             WHERE tenant_id = $1 AND catalog_id = $2 AND registration_id = $3",
            &[&TENANT, &CATALOG_ID, &reg],
        )
        .await
        .with_context(|| format!("del registration {reg}"))?;
    }
    Ok(())
}

/// Durable-consumer name the materializer binds per registration
/// (`mat_<tenant>_<catalog>_<registration>`; the guest's charset sanitises to
/// [A-Za-z0-9_-]). Used to read the NEW-path consumer's `num_pending`.
fn durable_name(reg: &str) -> String {
    let sanitize = |raw: &str| -> String {
        raw.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    format!(
        "mat_{}_{}_{}",
        sanitize(TENANT),
        sanitize(CATALOG_ID),
        sanitize(reg)
    )
}

// ---------------------------------------------------------------------------
// The concurrent consumers (dispatcher + materializer), each on its OWN tokio
// task so the CPU-heavy materializer guest cannot starve the dispatcher's poll
// cadence — the old-path drain must be measured, not throttled by cooperative
// single-task scheduling (an early join!-on-one-task version inflated the old
// drain ~15× via exactly that starvation).
// ---------------------------------------------------------------------------

type DispatchHandle = tokio::task::JoinHandle<anyhow::Result<Dispatcher>>;
type MatHandle = tokio::task::JoinHandle<anyhow::Result<()>>;

/// Tick the real dispatcher on a fixed real cadence until `stop`, then hand the
/// dispatcher back (so the next phase reuses one connection). The OLD path's
/// enqueue happens here (enqueued_at = the fire txn's server clock).
fn spawn_dispatch(mut d: Dispatcher, stop: Arc<AtomicBool>, poll_ms: u64) -> DispatchHandle {
    tokio::spawn(async move {
        while !stop.load(Ordering::SeqCst) {
            d.tick_project(0, epoch_ms()).await?;
            tokio::time::sleep(Duration::from_millis(poll_ms)).await;
        }
        // One last sweep so a row committed after the final in-loop tick fires.
        d.tick_project(0, epoch_ms()).await?;
        anyhow::Ok(d)
    })
}

/// Run the materializer guest back-to-back (bounded sweeps per call so `stop` is
/// checked promptly) until `stop`. The NEW path's enqueue happens here.
fn spawn_materialize(
    h: Arc<Harness>,
    stop: Arc<AtomicBool>,
    batch: u32,
    fetch_ms: u64,
) -> MatHandle {
    tokio::spawn(async move {
        while !stop.load(Ordering::SeqCst) {
            h.run_guest(4, batch, fetch_ms).await?;
        }
        // One last drain pass for stragglers committed during the final sweep.
        h.run_guest(4, batch, fetch_ms).await?;
        anyhow::Ok(())
    })
}

/// Sample old-path outbox backlog + new-path consumer pending every 200 ms until
/// `stop`; returns the (t_ms, old_backlog, new_pending) series.
fn spawn_sampler(
    sdb: tokio_postgres::Client,
    js: async_nats::jetstream::Context,
    stream_name: String,
    durable: String,
    old_tbl: String,
    stop: Arc<AtomicBool>,
    t0: Instant,
) -> tokio::task::JoinHandle<Vec<(u128, i64, i64)>> {
    tokio::spawn(async move {
        let mut series = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            let old_backlog = sdb
                .query_one(
                    "SELECT count(*) FROM wamn_run.outbox \
                     WHERE tenant_id = $1 AND table_name = $2 AND dispatched_at IS NULL",
                    &[&TENANT, &old_tbl],
                )
                .await
                .map(|r| r.get::<_, i64>(0))
                .unwrap_or(-1);
            let new_pending = match js.get_stream(&stream_name).await {
                Ok(s) => s
                    .consumer_info(&durable)
                    .await
                    .map(|i| i.num_pending as i64)
                    .unwrap_or(-1),
                Err(_) => -1,
            };
            series.push((t0.elapsed().as_millis(), old_backlog, new_pending));
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        series
    })
}

// ---------------------------------------------------------------------------
// The bench
// ---------------------------------------------------------------------------

pub async fn run(args: E2eBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates e2ebench (l5i9.22 EVT-C-E2E — the outbox-vs-CDC before/after)");

    let want = |p: &str| args.phase == "all" || args.phase.split(',').any(|x| x.trim() == p);
    let run_dist = want("dist");
    let run_fanout = want("fanout");
    let run_burst = want("burst");

    let guest = std::fs::read(&args.component)
        .with_context(|| format!("read {}", args.component.display()))?;
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    let stream_name = event_stream_name(ORG, ENV);

    // --- hermetic preamble (leftovers mask — the reader-live-gate lesson) -----
    let admin = connect(&args.admin_database_url).await?;
    let _ = admin
        .execute(
            "SELECT pg_terminate_backend(active_pid) FROM pg_replication_slots \
             WHERE slot_name = $1 AND active",
            &[&cdc_name],
        )
        .await;
    let _ = admin
        .execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots \
             WHERE slot_name = $1",
            &[&cdc_name],
        )
        .await;
    admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {DB} WITH (FORCE)"))
        .await
        .context("drop leftover db")?;
    admin
        .batch_execute(&format!("DROP ROLE IF EXISTS {cdc_name}"))
        .await
        .context("drop leftover role")?;
    admin
        .batch_execute(&format!("CREATE DATABASE {DB}"))
        .await
        .context("create db")?;

    // --- the REAL substrate: shipped DDL + real builders ----------------------
    let db = connect(&swap_db(&args.admin_database_url, DB)).await?;
    db.batch_execute(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') \
         THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;\n\
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
         THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' \
           NOSUPERUSER NOCREATEDB NOBYPASSRLS; END IF; END $$;",
    )
    .await
    .context("roles")?;
    for (name, ddl) in [
        ("system-schema.sql", SYSTEM_SQL),
        ("run-state.sql", RUN_STATE_SQL),
        ("run-queue.sql", RUN_QUEUE_SQL),
        ("flows.sql", FLOWS_SQL),
        ("catalog-schema.sql", CATALOG_SQL),
    ] {
        db.batch_execute(ddl)
            .await
            .with_context(|| format!("apply deploy/sql/{name}"))?;
    }
    println!("provisioned {DB} from deploy/sql (include_str! — drift-proof)");

    // Registry rows (the reader reads its registration from here).
    db.execute(upsert_org_sql(), &[&ORG, &"pooled", &"wamn-pg"])
        .await
        .context("org row")?;
    db.execute(upsert_project_sql(), &[&ORG, &PROJECT])
        .await
        .context("project row")?;
    db.execute(
        wamn_registry::sql::stamp_env_policy_sql(),
        &[
            &ORG,
            &ENV,
            &r#"{"kind":"pool"}"#,
            &0i32,
            &1i32,
            &"1Gi",
            &"250m",
            &"256Mi",
            &"postgres:18",
            &"",
            &"",
            &"off",
        ],
    )
    .await
    .context("env-policy row")?;
    db.execute(
        upsert_project_env_sql(),
        &[
            &ORG,
            &PROJECT,
            &ENV,
            &"wamn-db-e2e--app--dev",
            &None::<&str>,
        ],
    )
    .await
    .context("project-env row")?;
    let secret = format!("wamn-cdc-{ORG}--{PROJECT}--{ENV}");
    db.execute(
        upsert_event_reader_sql(),
        &[
            &ORG,
            &PROJECT,
            &ENV,
            &cdc_name,
            &cdc_name,
            &stream_name,
            &secret,
            &None::<&str>,
            &true,
        ],
    )
    .await
    .context("event_readers row")?;

    // The app floor (both arms) + outbox triggers on the OLD arm only (the
    // trigger-free new arm models the post-teardown CDC world) + CDC.
    db.batch_execute(&provision_sql::ensure_schema_sql("app"))
        .await
        .context("app schema")?;
    db.batch_execute("GRANT USAGE ON SCHEMA app TO wamn_app")
        .await
        .context("app schema usage")?;
    let full = build_catalog(&["old", "new"])?;
    let old_only = build_catalog(&["old"])?;
    let floor = Migration::create(&full)
        .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
        .sql(Confirmation::None)
        .map_err(|e| anyhow::anyhow!("floor sql: {e}"))?;
    db.batch_execute(&format!("SET search_path TO app; {floor}"))
        .await
        .context("apply the 3.2 floor (all 6 tables)")?;
    let triggers = Migration::outbox_triggers(
        &old_only,
        &OutboxOptions {
            schema: "wamn_run".into(),
        },
    )
    .map_err(|e| anyhow::anyhow!("outbox plan: {e}"))?
    .sql(Confirmation::None)
    .map_err(|e| anyhow::anyhow!("outbox sql: {e}"))?;
    db.batch_execute(&format!("SET search_path TO app; {triggers}"))
        .await
        .context("apply outbox triggers (old arm only)")?;
    db.batch_execute("SET search_path TO public")
        .await
        .context("reset search_path")?;
    db.batch_execute(&provision_sql::ensure_replication_role_sql(
        &cdc_name, CDC_PW,
    ))
    .await
    .context("replication role")?;
    db.batch_execute(&provision_sql::create_publication_sql(&cdc_name, "app"))
        .await
        .context("publication")?;
    db.batch_execute(&provision_sql::ensure_entity_map_sql("app"))
        .await
        .context("entity map")?;
    // Map every entity (old entities publish as bystanders — no registration, no
    // consumer; new entities carry the live registrations).
    for &arm in &["old", "new"] {
        for &n in &FANOUT {
            db.execute(
                &provision_sql::upsert_entity_map_sql("app"),
                &[&entity_id(arm, n), &table_name(arm, n)],
            )
            .await
            .with_context(|| format!("map {}", entity_id(arm, n)))?;
        }
    }
    db.batch_execute(&provision_sql::grant_replication_access_sql(
        DB, &cdc_name, "app",
    ))
    .await
    .context("grants")?;

    // Flows: N per (arm, level). Old-arm flows fire on the outbox path (no live
    // reg); new-arm flows are fired by the materializer (regs added per phase).
    for &arm in &["old", "new"] {
        for &n in &FANOUT {
            let table = table_name(arm, n);
            for i in 0..n {
                let flow = flow_id(arm, n, i);
                db.execute(
                    "INSERT INTO wamn_run.flows (tenant_id, flow_id, version, active, graph_json) \
                     VALUES ($1, $2, 1, true, $3::text::jsonb)",
                    &[&TENANT, &flow, &row_event_flow_json(&flow, &table)],
                )
                .await
                .with_context(|| format!("seed flow {flow}"))?;
            }
        }
    }
    println!(
        "seeded 6 entities (old+new × 1/5/20) + {} flows",
        2 * FANOUT.iter().sum::<usize>()
    );

    // --- NATS + the reader (slot LAST — provisioning writes stay uncaptured) --
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats.clone());
    let _ = js.delete_stream(&stream_name).await;

    db.batch_execute(&provision_sql::create_failover_slot_sql(&cdc_name))
        .await
        .context("create failover slot")?;

    let token = CancellationToken::new();
    let reader = tokio::spawn(run_with_token(
        EventReaderArgs {
            org: ORG.into(),
            project: PROJECT.into(),
            env: ENV.into(),
            system_database_url: swap_db(&args.admin_database_url, DB),
            cdc_url: role_url(&args.admin_database_url, &cdc_name, CDC_PW),
            nats_url: args.nats_url.clone(),
            sslmode: "disable".into(),
            stream_replicas: 1,
            dup_window_secs: 120,
            feedback_secs: 1,
            stall_threshold_secs: 30,
            slot_poll_secs: 0,
            slot_safe_wal_warn_bytes: 268_435_456,
        },
        token.clone(),
    ));
    println!("reader up (one pg_walstream session -> {stream_name})");

    // --- plugins + engine + harness + dispatcher ------------------------------
    let app_url = role_url(&args.admin_database_url, "wamn_app", "wamn_app");
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.clone());
    let pg = Arc::new(WamnPostgres::new(cfg)?);
    pg.set_tenant(BENCH_ID, TENANT)?;
    pg.set_schema(BENCH_ID, "wamn_run")?;
    pg.probe_checkout().await.context("postgres preflight")?;
    let jsp = Arc::new(WamnJetstream::new(WamnJetstreamConfig {
        nats_url: Some(args.nats_url.clone()),
    }));
    jsp.set_tenant(BENCH_ID, TENANT)?;
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let raw: &RawEngine = engine.inner();
    let component =
        WasmtimeComponent::new(raw, &guest).map_err(|e| anyhow::anyhow!("compile guest: {e}"))?;
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wamn_postgres::add_to_linker(&mut linker)?;
    wamn_jetstream::add_to_linker(&mut linker)?;
    let pre = CommandPre::new(linker.instantiate_pre(&component)?)?;
    let report_dir = std::env::temp_dir().join(format!("wamn-e2ebench-{}", std::process::id()));
    std::fs::create_dir_all(&report_dir).context("create report dir")?;
    let harness = Arc::new(Harness {
        engine,
        pre,
        pg,
        js: jsp,
        report_dir: report_dir.clone(),
        stream_name: stream_name.clone(),
    });

    let mut dispatcher = Dispatcher::connect(
        &[ProjectSpec {
            name: "e2e".into(),
            url: app_url.clone(),
            tenant: TENANT.into(),
            schema: Some("wamn_run".into()),
        }],
        None,
        DispatcherConfig::default(),
    )
    .await
    .context("dispatcher connect")?;

    let writer = Writer {
        app: connect(&app_url).await?,
    };
    writer
        .app
        .batch_execute(&format!(
            "SET search_path TO app; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("writer claims")?;

    // Reader preflight: a warmup write on new_e1 must reach the stream (proves
    // the slot→reader→JetStream leg is live before we time anything).
    add_new_regs(&db, 1).await?;
    let (_warm_id, _) = writer.insert(&table_name("new", 1), "warm", "warm").await?;
    {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let have = match js.get_stream(&stream_name).await {
                Ok(mut s) => s.info().await.map(|i| i.state.messages).unwrap_or(0),
                Err(_) => 0,
            };
            if have >= 1 {
                break;
            }
            if Instant::now() > deadline {
                if reader.is_finished() {
                    bail!("reader died during preflight: {:?}", reader.await);
                }
                bail!("reader produced no stream message in 30s");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    // Drain the warmup so it does not pollute phase counts.
    harness.run_guest(4, 128, args.fetch_ms).await?;
    dispatcher.tick_project(0, epoch_ms()).await?;
    del_new_regs(&db, 1).await?;
    println!("reader + materializer preflight OK (warmup drained)\n");

    let mut pass = true;
    let mut check = |name: &str, ok: bool| {
        println!("  [{}] {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            pass = false;
        }
    };

    let machine = std::env::var("WAMN_E2E_MACHINE").unwrap_or_else(|_| "local-throwaway".into());
    let env_label = std::env::var("WAMN_E2E_ENV").unwrap_or_else(|_| "local-throwaway".into());
    let host_profile = if cfg!(debug_assertions) {
        "debug-host-binaries"
    } else {
        "release-host-binaries"
    };
    let guest_profile = match std::env::var("WAMN_E2E_GUEST_PROFILE") {
        Ok(p) => p,
        Err(_) => {
            let path = args.component.to_string_lossy().into_owned();
            if path.contains("/release/") {
                "release-guest-wasm".into()
            } else if path.contains("/debug/") {
                "debug-guest-wasm".into()
            } else {
                "unspecified-guest-wasm".into()
            }
        }
    };
    let standing = if env_label == "local-throwaway" {
        "METHODOLOGY VALIDATION (shape, not ceilings)"
    } else {
        "CAMPAIGN OF RECORD"
    };
    let infra = std::env::var("WAMN_E2E_INFRA").unwrap_or_else(|_| {
        "pg=postgres:18(fsync=off,synchronous_commit=off,wal_level=logical) nats=nats:2(1-replica)"
            .into()
    });
    let provenance = format!(
        "# provenance: env={env_label} build={host_profile} {guest_profile} {infra} \
         disp_poll_ms={} fetch_ms={} machine={} — {standing}",
        args.disp_poll_ms, args.fetch_ms, machine
    );
    println!("{provenance}");

    // ========================================================================
    // Phase A — commit→run-start distribution (N=1, two steady rates)
    // ========================================================================
    if run_dist {
        println!("\n## phase A: commit->run-start distribution");
        add_new_regs(&db, 1).await?;
        let old_tbl = table_name("old", 1);
        let new_tbl = table_name("new", 1);
        let rates: Vec<f64> = args
            .dist_rates
            .split(',')
            .map(|s| s.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .with_context(|| format!("bad --dist-rates {:?}", args.dist_rates))?;

        let mut dist_csv = String::from("path,rate_target,n,p50_ms,p90_ms,p99_ms,mean_ms\n");
        let mut hist_csv = String::from("path,rate_target,bucket_lo,bucket_hi,count\n");

        for &rate in &rates {
            let stop = Arc::new(AtomicBool::new(false));
            let dh = spawn_dispatch(dispatcher, stop.clone(), args.disp_poll_ms);
            let mh = spawn_materialize(harness.clone(), stop.clone(), 128, args.fetch_ms);

            // Writer in the main task; commit instants recorded per id, per arm.
            let mut commits_old: Vec<(String, f64)> = Vec::new();
            let mut commits_new: Vec<(String, f64)> = Vec::new();
            let start = Instant::now();
            let mut sent: u64 = 0;
            while start.elapsed().as_secs_f64() < args.dist_secs as f64 {
                let due = (start.elapsed().as_secs_f64() * rate) as u64 + 1;
                while sent < due {
                    let tag = format!("d{rate}-{sent}");
                    commits_old.push(writer.insert(&old_tbl, "s", &tag).await?);
                    commits_new.push(writer.insert(&new_tbl, "s", &tag).await?);
                    sent += 1;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            tokio::time::sleep(Duration::from_secs(4)).await; // settle/drain
            stop.store(true, Ordering::SeqCst);
            dispatcher = dh.await.context("dispatch task")??;
            mh.await.context("materialize task")??;

            for (path, commits) in [(Path::Old, &commits_old), (Path::New, &commits_new)] {
                let commit_map: std::collections::HashMap<String, f64> =
                    commits.iter().cloned().collect();
                let enq = enqueued_rows(&db, path).await?;
                let mut lat: Vec<f64> = Vec::new();
                for (id, enq_ms) in &enq {
                    if let Some(&c) = commit_map.get(id) {
                        lat.push(enq_ms - c);
                    }
                }
                check(
                    &format!(
                        "{} @ {rate}/s: every write produced a run ({}/{})",
                        path.label(),
                        lat.len(),
                        commits.len()
                    ),
                    lat.len() == commits.len() && !commits.is_empty(),
                );
                lat.retain(|v| v.is_finite());
                lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let (p50, p90, p99, m) = (
                    pctl(&lat, 0.50),
                    pctl(&lat, 0.90),
                    pctl(&lat, 0.99),
                    mean(&lat),
                );
                println!(
                    "  {:<10} {rate:>4}/s  n={:<5} p50 {:>8.1}ms  p90 {:>8.1}ms  p99 {:>8.1}ms  mean {:>8.1}ms",
                    path.label(),
                    lat.len(),
                    p50,
                    p90,
                    p99,
                    m
                );
                dist_csv.push_str(&format!(
                    "{},{rate:.0},{},{p50:.1},{p90:.1},{p99:.1},{m:.1}\n",
                    path.label(),
                    lat.len()
                ));
                let h = histogram(&lat);
                for (b, &count) in h.iter().enumerate() {
                    let lo = if b == 0 { 0.0 } else { HIST_EDGES[b - 1] };
                    let hi = HIST_EDGES.get(b).copied().unwrap_or(f64::INFINITY);
                    hist_csv.push_str(&format!(
                        "{},{rate:.0},{lo:.0},{},{count}\n",
                        path.label(),
                        if hi.is_finite() {
                            format!("{hi:.0}")
                        } else {
                            "inf".into()
                        }
                    ));
                }
            }
            // Reset both queues so the next rate reads only its own runs.
            db.batch_execute(&format!(
                "DELETE FROM wamn_run.run_queue WHERE tenant_id = '{TENANT}'; \
                 DELETE FROM wamn_run.runs WHERE tenant_id = '{TENANT}'; \
                 DELETE FROM wamn_run.outbox WHERE tenant_id = '{TENANT}';"
            ))
            .await
            .context("reset queues between rates")?;
        }
        del_new_regs(&db, 1).await?;
        emit_csv("ce2e-dist", &dist_csv, &args.out);
        emit_csv("ce2e-dist-hist", &hist_csv, &args.out);
    }

    // ========================================================================
    // Phase B — fan-out 1→N (N = 1, 5, 20)
    // ========================================================================
    if run_fanout {
        println!("\n## phase B: fan-out 1->N (app-txn cost + commit->last-run)");
        let mut fan_csv = String::from(
            "path,n_flows,events,app_p50_ms,app_p99_ms,last_run_p50_ms,last_run_p99_ms,runs_total\n",
        );
        for &n in &FANOUT {
            add_new_regs(&db, n).await?;
            let old_tbl = table_name("old", n);
            let new_tbl = table_name("new", n);
            let stop = Arc::new(AtomicBool::new(false));
            let dh = spawn_dispatch(dispatcher, stop.clone(), args.disp_poll_ms);
            let mh = spawn_materialize(harness.clone(), stop.clone(), 128, args.fetch_ms);

            let mut commits_old: Vec<(String, f64)> = Vec::new();
            let mut commits_new: Vec<(String, f64)> = Vec::new();
            // App-txn commit latency (client-observed round trip) per arm.
            let mut app_old: Vec<f64> = Vec::new();
            let mut app_new: Vec<f64> = Vec::new();
            let start = Instant::now();
            let mut sent: u64 = 0;
            while (sent as usize) < args.fanout_events {
                let due = ((start.elapsed().as_secs_f64() * args.fanout_rate) as u64 + 1)
                    .min(args.fanout_events as u64);
                while sent < due {
                    let tag = format!("f{n}-{sent}");
                    let t = Instant::now();
                    commits_old.push(writer.insert(&old_tbl, "s", &tag).await?);
                    app_old.push(t.elapsed().as_secs_f64() * 1e3);
                    let t = Instant::now();
                    commits_new.push(writer.insert(&new_tbl, "s", &tag).await?);
                    app_new.push(t.elapsed().as_secs_f64() * 1e3);
                    sent += 1;
                }
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            stop.store(true, Ordering::SeqCst);
            dispatcher = dh.await.context("dispatch task")??;
            mh.await.context("materialize task")??;

            for (path, commits, app) in [
                (Path::Old, &commits_old, &app_old),
                (Path::New, &commits_new, &app_new),
            ] {
                let commit_map: std::collections::HashMap<String, f64> =
                    commits.iter().cloned().collect();
                let enq = enqueued_rows(&db, path).await?;
                // Group enqueued runs by payload id → (count, max enqueue ms).
                let mut per_event: std::collections::HashMap<String, (usize, f64)> =
                    std::collections::HashMap::new();
                for (id, ms) in &enq {
                    if commit_map.contains_key(id) {
                        let e = per_event.entry(id.clone()).or_insert((0, f64::MIN));
                        e.0 += 1;
                        e.1 = e.1.max(*ms);
                    }
                }
                let counts: Vec<usize> = per_event.values().map(|(c, _)| *c).collect();
                let runs_total: usize = counts.iter().sum();
                check(
                    &format!(
                        "{} N={n}: each of {} events fired exactly {n} runs (total {runs_total})",
                        path.label(),
                        per_event.len()
                    ),
                    per_event.len() == commits.len() && all_fanned_out(&counts, n),
                );
                let mut last_run: Vec<f64> = per_event
                    .iter()
                    .filter_map(|(id, (_, maxms))| commit_map.get(id).map(|c| maxms - c))
                    .collect();
                last_run.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let mut appv = app.clone();
                appv.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let (ap50, ap99) = (pctl(&appv, 0.50), pctl(&appv, 0.99));
                let (lp50, lp99) = (pctl(&last_run, 0.50), pctl(&last_run, 0.99));
                println!(
                    "  {:<10} N={n:<2}  app-txn p50 {:>7.3}ms p99 {:>7.3}ms | commit->last-run p50 {:>8.1}ms p99 {:>8.1}ms | runs {runs_total}",
                    path.label(),
                    ap50,
                    ap99,
                    lp50,
                    lp99
                );
                fan_csv.push_str(&format!(
                    "{},{n},{},{ap50:.3},{ap99:.3},{lp50:.1},{lp99:.1},{runs_total}\n",
                    path.label(),
                    commits.len()
                ));
            }
            db.batch_execute(&format!(
                "DELETE FROM wamn_run.run_queue WHERE tenant_id = '{TENANT}'; \
                 DELETE FROM wamn_run.runs WHERE tenant_id = '{TENANT}'; \
                 DELETE FROM wamn_run.outbox WHERE tenant_id = '{TENANT}';"
            ))
            .await
            .context("reset queues between fan-out levels")?;
            del_new_regs(&db, n).await?;
        }
        emit_csv("ce2e-fanout", &fan_csv, &args.out);
    }

    // ========================================================================
    // Phase C — burst (10× steady; lag depth, drain, app-path during drain)
    // ========================================================================
    if run_burst {
        println!("\n## phase C: burst ({}× spike)", args.burst_mult);
        add_new_regs(&db, 1).await?;
        // Same pre-phase cleanup the other phases carry: setup-warmup runs must
        // not leak into the completeness count.
        db.batch_execute(&format!(
            "DELETE FROM wamn_run.run_queue WHERE tenant_id = '{TENANT}'; \
             DELETE FROM wamn_run.runs WHERE tenant_id = '{TENANT}'; \
             DELETE FROM wamn_run.outbox WHERE tenant_id = '{TENANT}';"
        ))
        .await
        .context("burst pre-phase cleanup")?;
        let old_tbl = table_name("old", 1);
        let new_tbl = table_name("new", 1);
        let new_durable = durable_name(&reg_id(1, 0));
        let sampler_db = connect(&swap_db(&args.admin_database_url, DB)).await?;

        let t0 = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let dh = spawn_dispatch(dispatcher, stop.clone(), args.disp_poll_ms);
        let mh = spawn_materialize(harness.clone(), stop.clone(), 128, args.fetch_ms);
        let sh = spawn_sampler(
            sampler_db,
            js.clone(),
            stream_name.clone(),
            new_durable,
            old_tbl.clone(),
            stop.clone(),
            t0,
        );

        // Writer in the main task. (t_ms, path, app_lat_ms).
        let mut applat: Vec<(u128, Path, f64)> = Vec::new();
        let spike_start = args.burst_warmup_secs as f64;
        let spike_end = spike_start + args.burst_spike_secs as f64;
        let total = spike_end + args.burst_drain_secs as f64;
        let mut sent: u64 = 0;
        loop {
            let el = t0.elapsed().as_secs_f64();
            if el >= total {
                break;
            }
            // Integrated offered count: steady everywhere + the extra spike rate
            // only within the spike window.
            let base = el * args.burst_steady;
            let spike_extra = if el <= spike_start {
                0.0
            } else {
                let s = el.min(spike_end) - spike_start;
                s * (args.burst_mult - 1.0) * args.burst_steady
            };
            let due = (base + spike_extra) as u64 + 1;
            while sent < due {
                let tag = format!("b-{sent}");
                let t = Instant::now();
                writer.insert(&old_tbl, "s", &tag).await?;
                applat.push((
                    t0.elapsed().as_millis(),
                    Path::Old,
                    t.elapsed().as_secs_f64() * 1e3,
                ));
                let t = Instant::now();
                writer.insert(&new_tbl, "s", &tag).await?;
                applat.push((
                    t0.elapsed().as_millis(),
                    Path::New,
                    t.elapsed().as_secs_f64() * 1e3,
                ));
                sent += 1;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
        stop.store(true, Ordering::SeqCst);
        dispatcher = dh.await.context("dispatch task")??;
        mh.await.context("materialize task")??;
        let depth = sh.await.context("sampler task")?;
        let mut depth_csv = String::from("t_ms,old_backlog,new_pending\n");
        let mut old_peak = 0i64;
        let mut new_peak = 0i64;
        for (t, o, p) in &depth {
            depth_csv.push_str(&format!("{t},{o},{p}\n"));
            old_peak = old_peak.max(*o);
            new_peak = new_peak.max(*p);
        }
        let spike_end_ms = ((args.burst_warmup_secs + args.burst_spike_secs) as u128) * 1000;
        let drain_after =
            |samples: &[(u128, i64, i64)], pick: fn(&(u128, i64, i64)) -> i64| -> Option<u128> {
                samples
                    .iter()
                    .filter(|s| s.0 >= spike_end_ms && pick(s) <= 0)
                    .map(|s| s.0 - spike_end_ms)
                    .next()
            };
        let old_drain = drain_after(&depth, |s| s.1);
        let new_drain = drain_after(&depth, |s| s.2);
        println!(
            "  old-outbox: peak backlog {old_peak} rows, drained {} after spike end",
            old_drain.map_or("(not within window)".into(), |d| format!("{}ms", d))
        );
        println!(
            "  new-cdc:    peak pending {new_peak} msgs, drained {} after spike end",
            new_drain.map_or("(not within window)".into(), |d| format!("{}ms", d))
        );
        // Completeness is the gate: every burst event minted exactly one run on
        // each path (1 registration / 1 flow in this phase). The OBSERVED
        // peak/drain stay reported measurements only — a zero observed new-path
        // peak is a valid fast-drain result: consumer pending can rise and fall
        // entirely between sampler ticks on a release build.
        let old_runs = enqueued_rows(&db, Path::Old).await?.len() as u64;
        let new_runs = enqueued_rows(&db, Path::New).await?.len() as u64;
        check(
            &format!("burst: old path minted one run per event ({old_runs}/{sent})"),
            old_runs == sent,
        );
        check(
            &format!("burst: new path minted one run per event ({new_runs}/{sent})"),
            new_runs == sent,
        );
        if new_peak == 0 {
            println!(
                "  note: new-path pending never sampled >0 — the drain outpaced the sampler cadence"
            );
        }

        // App-path latency per 1s window, per path.
        let mut app_csv = String::from("window_s,path,app_p50_ms,app_p99_ms,writes\n");
        let max_w = applat
            .iter()
            .map(|(t, _, _)| (t / 1000) as u64)
            .max()
            .unwrap_or(0);
        for w in 0..=max_w {
            for path in [Path::Old, Path::New] {
                let mut v: Vec<f64> = applat
                    .iter()
                    .filter(|(t, p, _)| (t / 1000) as u64 == w && *p == path)
                    .map(|(_, _, l)| *l)
                    .collect();
                if v.is_empty() {
                    continue;
                }
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                app_csv.push_str(&format!(
                    "{w},{},{:.3},{:.3},{}\n",
                    path.label(),
                    pctl(&v, 0.50),
                    pctl(&v, 0.99),
                    v.len()
                ));
            }
        }
        emit_csv("ce2e-burst-depth", &depth_csv, &args.out);
        emit_csv("ce2e-burst-applat", &app_csv, &args.out);
        del_new_regs(&db, 1).await?;
    }

    // --- teardown (zero residue: slot FIRST, then stream, then the db) --------
    println!("\n{provenance}");
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(15), reader).await;
    let _ = db
        .execute(
            "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots \
             WHERE slot_name = $1",
            &[&cdc_name],
        )
        .await;
    let _ = js.delete_stream(&stream_name).await;
    drop(writer);
    drop(dispatcher);
    drop(db);
    let _ = admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {DB} WITH (FORCE)"))
        .await;
    let _ = admin
        .batch_execute(&format!("DROP ROLE IF EXISTS {cdc_name}"))
        .await;
    let _ = std::fs::remove_dir_all(&report_dir);
    ticker.abort();

    println!("\ne2ebench complete — sanity asserts PASS: {pass}");
    if !pass {
        bail!("an l5i9.22 e2ebench sanity assert failed (measurements untrustworthy)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path attribution (load-bearing): the run-id namespaces are disjoint by
    /// design. A swapped/broken predicate must die HERE, not in a silent
    /// miscount. Kills the "break a path-attribution predicate" mutant.
    #[test]
    fn run_ids_attribute_to_disjoint_paths() {
        assert_eq!(classify_run_id("oldf1-0:outbox:42"), Some(Path::Old));
        assert_eq!(
            classify_run_id("newf5-3:evt:00000000000000000007"),
            Some(Path::New)
        );
        assert_eq!(classify_run_id("cron:tick:1"), None);
        assert_eq!(classify_run_id("plain-manual-id"), None);
        // The two LIKE patterns select disjoint namespaces.
        assert_eq!(Path::Old.like(), "%:outbox:%");
        assert_eq!(Path::New.like(), "%:evt:%");
        assert_ne!(Path::Old.like(), Path::New.like());
    }

    /// Fan-out sanity (load-bearing): every event fired exactly N runs. Kills
    /// the "break the run-counting sanity assert" mutant.
    #[test]
    fn fanout_sanity_requires_every_event_at_exactly_n() {
        assert!(all_fanned_out(&[5, 5, 5], 5));
        assert!(all_fanned_out(&[1], 1));
        assert!(!all_fanned_out(&[], 5), "no events observed is not ok");
        assert!(!all_fanned_out(&[5, 4, 5], 5), "an under-fired event fails");
        assert!(!all_fanned_out(&[5, 6, 5], 5), "an over-fired event fails");
        assert!(!all_fanned_out(&[1, 1, 1], 5), "wrong fan-out count fails");
    }

    /// The histogram bins on the fixed edges and never loses a sample (the
    /// overflow bucket catches the long tail).
    #[test]
    fn histogram_bins_on_edges_and_conserves_count() {
        let s = [0.5, 9.9, 10.0, 24.0, 60.0, 300.0, 1500.0];
        let h = histogram(&s);
        assert_eq!(h.iter().sum::<u64>(), s.len() as u64, "no sample lost");
        assert_eq!(h[0], 2, "0.5 and 9.9 fall under 10ms");
        assert_eq!(h[7], 1, "1500ms lands in the overflow bucket");
    }

    /// The fixture catalog builds and the trigger plan is scoped to the OLD arm
    /// only — the new arm is trigger-free (the measured app-txn difference).
    #[test]
    fn old_arm_gets_triggers_new_arm_stays_trigger_free() {
        let old_only = build_catalog(&["old"]).expect("old-only catalog");
        let plan = Migration::outbox_triggers(
            &old_only,
            &OutboxOptions {
                schema: "wamn_run".into(),
            },
        )
        .unwrap()
        .sql(Confirmation::None)
        .unwrap();
        for &n in &FANOUT {
            assert!(
                plan.contains(&format!("ON \"{}\"", table_name("old", n))),
                "old arm table {} must carry a trigger",
                table_name("old", n)
            );
            assert!(
                !plan.contains(&table_name("new", n)),
                "new arm table {} must NOT carry a trigger",
                table_name("new", n)
            );
        }
        // The full catalog compiles all six floor tables.
        let full = build_catalog(&["old", "new"]).expect("full catalog");
        let floor = Migration::create(&full)
            .unwrap()
            .sql(Confirmation::None)
            .unwrap();
        for &arm in &["old", "new"] {
            for &n in &FANOUT {
                assert!(
                    floor.contains(&format!("CREATE TABLE \"{}\"", table_name(arm, n))),
                    "floor creates {}",
                    table_name(arm, n)
                );
            }
        }
    }

    /// Fixture flows + registrations parse as the frozen wamn types.
    #[test]
    fn fixture_flow_and_registration_parse_as_frozen_types() {
        let f = wamn_flow::Flow::from_json(&row_event_flow_json("oldf1-0", "old_events_1"))
            .expect("flow parses");
        f.validate().expect("flow validates");
        let reg = wamn_event_reg::EventRegistration::from_json(&registration_json(
            "nr1-0", "newf1-0", "new_e1",
        ))
        .expect("registration parses");
        // State omitted = live (the 0.1.x additive default) — a live reg is what
        // makes the materializer enqueue real runs, not shadow.
        assert!(!reg.state.is_shadow());
    }
}
