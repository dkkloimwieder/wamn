//! The `queuebench` subcommand: the 5.14 durable-run-queue gates (docs/run-queue.md).
//!
//! Unlike flowbench/testhostbench, this is **pure host-side** — the queue is a
//! Postgres mechanism (`FOR UPDATE SKIP LOCKED`) plus a NATS-core doorbell, so the
//! gate drives raw `tokio_postgres` claimers (no wasm guest) using the pure SQL
//! builders from [`wamn_run_queue`]. It provisions a fresh ephemeral schema
//! (clone of the 5.7 `runs` + the 5.14 `run_queue`) through the Postgres superuser
//! (`WAMN_PG_ADMIN_URL`; `wamn_app` is NOSUPERUSER and cannot create schemas,
//! exactly as in production), then measures the D15 dispatch SLOs and proves the
//! queue's core properties.
//!
//! Modes:
//!   dispatch   — D15 write-ahead (p99 < 15 ms) and reduced-audit fast path
//!                (p99 < 10 ms) enqueue latency.
//!   throughput — N concurrent claimers over one queue: SKIP LOCKED gives every
//!                run to exactly one claimer (exactly-once) and none is missed
//!                (completeness), sustaining ~1–5k claims/s.
//!   reclaim    — a claimant's lease expires; another replica reclaims the run
//!                (crash-safe failover), and not before the lease expires.
//!   park       — park/wake cycles are budget-free: `attempts` counts crash
//!                evidence (expired-lease reclaims) only, so a flow that parks far
//!                more times than `max_attempts` still completes — on BOTH claim
//!                paths — and the janitor retires nothing. Plus the wamn-fqg.7
//!                corollary: a budget-spent run whose lease a park released (NULL)
//!                still WAKES and completes (not wedged invisible), while a
//!                budget-spent run holding an expired lease stays terminal.
//!   janitor    — an abandoned (expired-lease, budget-spent) run is swept to
//!                `infrastructure-failure` and dequeued; a healthy run is untouched.
//!   doorbell   — enqueue publishes a NATS-core hint; a subscriber wakes and
//!                claims with no polling (async warm p50 < 25 ms / p99 < 100 ms).
//!   partition  — `partitioned(key)` runs dispatch in-order per key across
//!                concurrent replicas (per-key serialization + in-order +
//!                exactly-once), and a partition fails over in order when its owner
//!                dies (the dedicated `partition_owner` lease).
//!   ceiling    — the EVT-C7 measurement campaign (docs/event-plane-jetstream.md
//!                §10): full-lifecycle transitions/sec knee (ramp + bisect), a
//!                sustained soak at 80% of knee with a bloat probe, and a 10×
//!                burst/recovery profile — curves + CSVs, not pass/fail numbers
//!                (only the exactly-once/completeness sanity asserts gate). NOT
//!                part of `--mode all` (it is a long campaign, not a regression
//!                gate); run it explicitly via deploy/gates/queuebench-ceiling-job.yaml.
//!   all        — every regression mode in sequence (everything except ceiling).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};
use wamn_gate_harness::{ceiling, emit_csv, percentile};
use wamn_run_queue::{
    acquire_partitions_sql, claim_batch_sql, claim_dispatch_sql, claim_partition_head_sql,
    complete_dequeue_sql, dequeue_sql, enqueue_sql, janitor_sweep_sql, mark_running_sql, park_sql,
    write_ahead_run_sql,
};

const SCHEMA: &str = "wamn_queue_bench";
const TENANT: &str = "queue-tenant";
const CLAIM_BATCH: usize = 20;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Dispatch,
    Throughput,
    Reclaim,
    Park,
    Janitor,
    Doorbell,
    Partition,
    Ceiling,
    All,
}

#[derive(Debug, Args)]
pub struct QueueBenchArgs {
    /// App (runner) Postgres URL — the NOSUPERUSER wamn_app role that claims work.
    /// Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema (runs + run_queue)
    /// the gate runs against. wamn_app is NOSUPERUSER/NOCREATEDB, like production.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// NATS URL for the doorbell mode (fire-and-forget hints).
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,

    /// mTLS material for the doorbell NATS connection (the in-cluster operator NATS
    /// uses verify_and_map; mount the wasmcloud-runtime-tls secret and pass these).
    /// Omit for a plain (no-TLS) NATS, e.g. a local throwaway server.
    #[arg(long)]
    pub nats_tls_ca: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_cert: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_key: Option<PathBuf>,

    /// Which gate to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Concurrent claimers for the throughput gate.
    #[arg(long, default_value_t = 12)]
    pub concurrency: usize,

    /// Queue depth for the throughput gate.
    #[arg(long, default_value_t = 5_000)]
    pub seed_runs: usize,

    /// Enqueue-latency samples for the dispatch gate (per sub-mode).
    #[arg(long, default_value_t = 500)]
    pub dispatch_iters: usize,

    /// Hint→claim samples for the doorbell gate.
    #[arg(long, default_value_t = 300)]
    pub doorbell_iters: usize,

    /// Ceiling mode: seconds of offered load per ramp level (60 for the record
    /// run — the §10 methodology; small values for local iteration).
    #[arg(long, default_value_t = 60)]
    pub level_secs: u64,

    /// Ceiling mode: the ramp's starting offered rate, lifecycles/sec.
    #[arg(long, default_value_t = 250.0)]
    pub base_rate: f64,

    /// Ceiling mode: open-loop producer connections (write-ahead + enqueue).
    #[arg(long, default_value_t = 8)]
    pub producers: usize,

    /// Ceiling mode: sustained-soak seconds at 80% of the measured knee.
    #[arg(long, default_value_t = 300)]
    pub soak_secs: u64,

    /// Ceiling mode: burst seconds at 10× the soak baseline.
    #[arg(long, default_value_t = 60)]
    pub burst_secs: u64,

    /// Ceiling mode: also write each CSV to this directory (stdout always
    /// carries them between `=== BEGIN/END CSV <name> ===` markers).
    #[arg(long)]
    pub ceiling_out: Option<PathBuf>,
}

/// The ephemeral-schema clone: the 5.7 `runs` (the write-ahead target + the FK)
/// and the 5.14 `run_queue`, schema-qualified, with the house tenant floor. A
/// faithful, self-contained stand-in for `deploy/sql/run-state.sql` + `run-queue.sql`
/// so the gate never touches the shared production schema (the same pattern as
/// testhostbench's `template_ddl`).
fn queue_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, \
            status text NOT NULL DEFAULT 'running' \
              CHECK (status IN ('dispatched','running','completed','failed','cancelled','infrastructure-failure')), \
            input_json jsonb, result_json jsonb, state_json jsonb, \
            updated_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.run_queue (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text, \
            partition_policy text NOT NULL DEFAULT 'blocking' \
              CHECK (partition_policy IN ('blocking','leapfrog')), \
            priority int NOT NULL DEFAULT 0, available_at timestamptz NOT NULL DEFAULT now(), \
            lease_owner text, lease_expires_at timestamptz, \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            stream_seq bigint NOT NULL DEFAULT 0, \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX run_queue_claimable ON {schema}.run_queue (tenant_id, available_at, stream_seq, lease_expires_at);\
         CREATE INDEX run_queue_partition ON {schema}.run_queue (tenant_id, partition_key) WHERE partition_key IS NOT NULL;\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;\
         CREATE TABLE {schema}.partition_owner (\
            tenant_id text NOT NULL, partition_key text NOT NULL, \
            lease_owner text NOT NULL, lease_expires_at timestamptz NOT NULL, \
            acquired_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, partition_key));\
         ALTER TABLE {schema}.partition_owner ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.partition_owner FORCE ROW LEVEL SECURITY;\
         CREATE POLICY partition_owner_tenant ON {schema}.partition_owner \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.partition_owner TO wamn_app;\
         CREATE TABLE {schema}.flows (\
            tenant_id text NOT NULL, flow_id text NOT NULL, version int NOT NULL, \
            active boolean NOT NULL DEFAULT false, graph_json jsonb, \
            PRIMARY KEY (tenant_id, flow_id, version));\
         ALTER TABLE {schema}.flows ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.flows FORCE ROW LEVEL SECURITY;\
         CREATE POLICY flows_tenant ON {schema}.flows \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.flows TO wamn_app;"
    )
}

/// Drop-and-recreate the ephemeral schema from the template DDL, via superuser.
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
        client
            .batch_execute(&queue_ddl(SCHEMA))
            .await
            .context("apply queue DDL")?;
        // The one active flow every seeded run references: claim_dispatch_sql's
        // active-version probe joins it (reset() truncates only runs, so this
        // survives every phase).
        client
            .batch_execute(&format!(
                "INSERT INTO {SCHEMA}.flows (tenant_id, flow_id, version, active, graph_json) \
                 VALUES ('{TENANT}', 'f', 1, true, '{{}}'::jsonb);"
            ))
            .await
            .context("seed the active flow row")?;
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

/// Empty the queue + run tables (superuser) so each phase starts from a clean
/// slate — no leftover rows from an earlier phase leak into a claim. TRUNCATE
/// runs CASCADEs to run_queue via the FK.
async fn reset(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = client
        .batch_execute(&format!("TRUNCATE {SCHEMA}.runs CASCADE;"))
        .await
        .map_err(|e| anyhow::anyhow!("reset queue tables: {e}"));
    drop(client);
    let _ = conn_task.await;
    r.map(|_| ())
}

/// A wamn_app connection pinned to the ephemeral schema + tenant claim (the RLS
/// floor the runner runs under). Session-level SETs persist for the connection.
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

/// Enqueue a run: the D15 write-ahead run row + the queue row, co-transacted
/// (one durability domain, D3). Takes `&mut` because the transaction borrows the
/// connection exclusively for its lifetime.
async fn enqueue(client: &mut Client, run_id: &str, delay_ms: i64) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(&write_ahead_run_sql(), &[&run_id, &"f", &1i32])
        .await?;
    tx.execute(
        &enqueue_sql(),
        &[&run_id, &Option::<&str>::None, &0i32, &delay_ms],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Enqueue a run bound to a partition (the `partitioned(key)` path). Same
/// write-ahead + queue-row transaction as [`enqueue`], but with a `partition_key`.
async fn enqueue_partitioned(
    client: &mut Client,
    run_id: &str,
    partition_key: &str,
    delay_ms: i64,
) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(&write_ahead_run_sql(), &[&run_id, &"f", &1i32])
        .await?;
    tx.execute(
        &enqueue_sql(),
        &[&run_id, &Some(partition_key), &0i32, &delay_ms],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn run(args: QueueBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "queuebench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;

    println!("# wamn-host 5.14 queuebench (schema {SCHEMA}, tenant {TENANT})");
    provision(&admin_url)
        .await
        .context("provision ephemeral schema")?;

    let run_all = args.mode == Mode::All;
    let mut pass = true;
    let outcome = async {
        if run_all || args.mode == Mode::Dispatch {
            pass &= dispatch_phase(&app_url, &admin_url, &args).await?;
        }
        if run_all || args.mode == Mode::Throughput {
            pass &= throughput_phase(&app_url, &admin_url, &args).await?;
        }
        if run_all || args.mode == Mode::Reclaim {
            pass &= reclaim_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Park {
            pass &= park_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Janitor {
            pass &= janitor_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Doorbell {
            pass &=
                doorbell_phase(&app_url, &admin_url, &args, args.mode == Mode::Doorbell).await?;
        }
        if run_all || args.mode == Mode::Partition {
            pass &= partition_phase(&app_url, &admin_url, &args).await?;
        }
        // Ceiling is a measurement CAMPAIGN, not a regression gate: it runs
        // only when asked for explicitly, never under --mode all.
        if args.mode == Mode::Ceiling {
            pass &= ceiling_phase(&app_url, &admin_url, &args).await?;
        }
        anyhow::Ok(())
    }
    .await;

    // Always drop the ephemeral schema, even on a phase error.
    let _ = teardown(&admin_url).await;
    outcome?;

    println!("\nqueuebench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more 5.14 queue gates failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// dispatch: D15 write-ahead + reduced-audit fast-path enqueue latency
// ---------------------------------------------------------------------------

async fn dispatch_phase(
    app_url: &str,
    admin_url: &str,
    args: &QueueBenchArgs,
) -> anyhow::Result<bool> {
    let n = args.dispatch_iters;
    println!("\n## dispatch — {n} enqueues each (write-ahead SLO p99<15ms, fast-path p99<10ms)");
    reset(admin_url).await?;
    let (mut client, _h) = connect_app(app_url).await?;

    // Warm up prepared-statement caches so the first call doesn't skew p99.
    for i in 0..10 {
        enqueue(&mut client, &format!("warm-{i}"), 0).await?;
    }

    // Write-ahead (default): a durable dispatched run row + the queue row.
    let mut wa: Vec<Duration> = Vec::with_capacity(n);
    for i in 0..n {
        let run_id = format!("wa-{i}");
        let start = Instant::now();
        enqueue(&mut client, &run_id, 0).await?;
        wa.push(start.elapsed());
    }
    wa.sort();

    // Reduced-audit fast path (D15 opt-in): the write-ahead run row only, no
    // separate queue row (direct sync dispatch).
    let mut fp: Vec<Duration> = Vec::with_capacity(n);
    let wa_sql = write_ahead_run_sql();
    for i in 0..n {
        let run_id = format!("fp-{i}");
        let start = Instant::now();
        client.execute(&wa_sql, &[&run_id, &"f", &1i32]).await?;
        fp.push(start.elapsed());
    }
    fp.sort();

    let wa_p99 = percentile(&wa, 0.99);
    let fp_p99 = percentile(&fp, 0.99);
    println!(
        "write-ahead: p50 {:?}  p99 {:?}  max {:?}",
        percentile(&wa, 0.50),
        wa_p99,
        wa.last().copied().unwrap_or_default()
    );
    println!(
        "fast-path:   p50 {:?}  p99 {:?}  max {:?}",
        percentile(&fp, 0.50),
        fp_p99,
        fp.last().copied().unwrap_or_default()
    );
    let pass = wa_p99 < Duration::from_millis(15) && fp_p99 < Duration::from_millis(10);
    println!("PASS(dispatch SLOs): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// throughput: SKIP LOCKED exactly-once + completeness + claims/s
// ---------------------------------------------------------------------------

async fn throughput_phase(
    app_url: &str,
    admin_url: &str,
    args: &QueueBenchArgs,
) -> anyhow::Result<bool> {
    let n = args.seed_runs;
    println!(
        "\n## throughput — {} claimers over {n} queued runs (SKIP LOCKED exactly-once)",
        args.concurrency
    );
    reset(admin_url).await?;

    // Seed n runs + queue rows as superuser (bypasses RLS for a fast bulk load).
    {
        let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
        let conn_task = tokio::spawn(conn);
        let r = client
            .batch_execute(&format!(
                "INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version, status) \
                   SELECT '{TENANT}', 'tp-'||g, 'f', 1, 'dispatched' FROM generate_series(1,{n}) g; \
                 INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id, priority, available_at) \
                   SELECT '{TENANT}', 'tp-'||g, 0, now() FROM generate_series(1,{n}) g;"
            ))
            .await;
        drop(client);
        let _ = conn_task.await;
        r.context("seed throughput queue")?;
    }

    // Each claimer holds a long lease on what it claims, so the loop drains when
    // every row is leased — no dequeue round trips to muddy the claim rate. The
    // union of claimed ids proves exactly-once (no dup) + completeness (all n).
    let lease_ttl_ms: i64 = 600_000; // 10 min — no expiry during the gate
    let claim_sql = Arc::new(claim_batch_sql(CLAIM_BATCH));
    let started = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for w in 0..args.concurrency {
        let app_url = app_url.to_string();
        let claim_sql = claim_sql.clone();
        let owner = format!("claimer-{w}");
        tasks.spawn(async move {
            let (client, _h) = connect_app(&app_url).await?;
            let mut mine: Vec<String> = Vec::new();
            loop {
                let rows = client
                    .query(claim_sql.as_str(), &[&owner, &lease_ttl_ms])
                    .await?;
                if rows.is_empty() {
                    break;
                }
                for row in &rows {
                    mine.push(row.get::<_, String>("run_id"));
                }
            }
            anyhow::Ok(mine)
        });
    }

    let mut all: HashSet<String> = HashSet::new();
    let mut total = 0usize;
    while let Some(res) = tasks.join_next().await {
        let mine = res??;
        total += mine.len();
        for id in mine {
            all.insert(id);
        }
    }
    let elapsed = started.elapsed();
    let rate = total as f64 / elapsed.as_secs_f64();

    let exactly_once = all.len() == total;
    let complete = total == n;
    println!(
        "claimed {total} (unique {}), elapsed {elapsed:?}, rate {rate:.0}/s",
        all.len()
    );
    let rate_ok = rate >= 500.0;
    let pass = exactly_once && complete && rate_ok;
    println!(
        "PASS(exactly-once, complete, rate≥500/s): {pass} (exactly_once={exactly_once}, complete={complete}, rate_ok={rate_ok})"
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// reclaim: lease expiry -> another replica reclaims (crash-safe failover)
// ---------------------------------------------------------------------------

async fn reclaim_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!("\n## reclaim — a claimant's lease expires, another replica reclaims exactly once");
    reset(admin_url).await?;
    let (mut a, _ha) = connect_app(app_url).await?;
    let (b, _hb) = connect_app(app_url).await?;

    enqueue(&mut a, "rc-1", 0).await?;

    let short_ttl: i64 = 400;
    let claim = claim_batch_sql(10);
    // attempts counts crash evidence only (wamn-fqg.5): A's first claim of the
    // never-leased row is FREE (attempts stays 0)…
    let got_a = a.query(&claim, &[&"A", &short_ttl]).await?;
    let a_ok = got_a.len() == 1
        && got_a[0].get::<_, String>("run_id") == "rc-1"
        && got_a[0].get::<_, i32>("attempts") == 0;

    // B cannot steal a live lease.
    let blocked = b.query(&claim, &[&"B", &short_ttl]).await?;
    let b_blocked = blocked.is_empty();

    // …and after the lease expires, B's reclaim of the expired lease is the first
    // counted unit of crash evidence: attempts == 1 (it was 2 under the pre-fqg.5
    // count-every-claim semantics — the new value is the point, not a regression).
    tokio::time::sleep(Duration::from_millis(short_ttl as u64 + 250)).await;
    let reclaimed = b.query(&claim, &[&"B", &short_ttl]).await?;
    let b_reclaimed = reclaimed.len() == 1
        && reclaimed[0].get::<_, String>("run_id") == "rc-1"
        && reclaimed[0].get::<_, i32>("attempts") == 1;

    println!(
        "A claimed={a_ok}, B blocked while lease live={b_blocked}, B reclaimed after expiry={b_reclaimed}"
    );
    let pass = a_ok && b_blocked && b_reclaimed;
    println!("PASS(lease failover): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// park: park/wake cycles are budget-free (attempts counts crash evidence only)
// ---------------------------------------------------------------------------

/// A delay-loop flow parks and wakes far more times than its `max_attempts`, on
/// BOTH claim paths (the global claim and the partition head claim — a parked
/// partitioned head is re-claimed on every wake). Every wake re-claim must be
/// FREE: park releases the lease, so the claim's crash-evidence `CASE` sees no
/// expired lease and leaves `attempts` at 0. The runs complete with the full
/// redelivery budget intact and a janitor sweep retires nothing. Before the
/// wamn-fqg.5 fix each claim bumped `attempts`, so 10 parks with max_attempts=3
/// classified the runs Exhausted mid-loop — killed having failed zero times —
/// and this phase fails at the first post-budget wake.
/// wamn-fqg.7: the wedge. A budget-spent run (`attempts == max_attempts`) whose lease
/// a park RELEASED (NULL) must WAKE and complete — a NULL lease is proof the last owner
/// was alive (it parked), never crash evidence, so the crash budget must not gate it. A
/// budget-spent run still holding an EXPIRED lease (a crash after the budget was spent)
/// stays terminal: not claimed, reaped by the janitor. Proven on BOTH claim paths.
async fn park_wedge_check(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    reset(admin_url).await?;
    let (mut client, _h) = connect_app(app_url).await?;

    // Global path: a woken and a poison budget-spent row.
    enqueue(&mut client, "rq-wedge-woken", 0).await?;
    enqueue(&mut client, "rq-wedge-poison", 0).await?;
    // Partition path (site-w): a woken budget-spent HEAD and its ready later sibling.
    enqueue_partitioned(&mut client, "pw-0", "site-w", 0).await?;
    enqueue_partitioned(&mut client, "pw-1", "site-w", 0).await?;
    // Spend the budget on the wedged rows; poison additionally holds an expired lease.
    client
        .batch_execute(
            "UPDATE run_queue SET max_attempts = 3;\n\
             UPDATE run_queue SET attempts = 3 \
               WHERE run_id IN ('rq-wedge-woken', 'rq-wedge-poison', 'pw-0');\n\
             UPDATE run_queue SET lease_owner = 'dead', lease_expires_at = now() - interval '1 hour' \
               WHERE run_id = 'rq-wedge-poison';",
        )
        .await?;

    let claim = claim_batch_sql(10);
    let acquire = acquire_partitions_sql(4);
    let claim_head = claim_partition_head_sql(4);
    let ttl: i64 = 60_000;

    // The global claim wakes the released-lease row (attempts UNCHANGED — a NULL lease
    // is not crash evidence) and skips the expired-lease poison row.
    client.query(&claim, &[&"CW", &ttl]).await?;
    let woken = client
        .query_one(
            "SELECT lease_owner, attempts FROM run_queue WHERE run_id = 'rq-wedge-woken'",
            &[],
        )
        .await?;
    let woken_claimed =
        woken.get::<_, Option<String>>(0).as_deref() == Some("CW") && woken.get::<_, i32>(1) == 3;
    let poison_owner: Option<String> = client
        .query_one(
            "SELECT lease_owner FROM run_queue WHERE run_id = 'rq-wedge-poison'",
            &[],
        )
        .await?
        .get(0);
    let poison_unclaimed = poison_owner.as_deref() == Some("dead");
    // The woken run completes; then a zero-grace janitor retires the poison run only.
    client
        .execute(&mark_running_sql(), &[&"rq-wedge-woken"])
        .await?;
    client.execute(&dequeue_sql(), &[&"rq-wedge-woken"]).await?;
    client.execute(&janitor_sweep_sql(), &[&0i64]).await?;
    let poison_reaped: bool = client
        .query_one(
            "SELECT status = 'infrastructure-failure' AND \
                    NOT EXISTS (SELECT 1 FROM run_queue q WHERE q.run_id = r.run_id) \
               FROM runs r WHERE run_id = 'rq-wedge-poison'",
            &[],
        )
        .await?
        .get(0);

    // Partition path: the woken budget-spent head is acquirable + head-claimed
    // (attempts unchanged), and its later sibling stays blocked (in-order preserved).
    client.query(&acquire, &[&"RW", &ttl]).await?;
    let owns_w: i64 = client
        .query_one(
            "SELECT count(*) FROM partition_owner WHERE partition_key = 'site-w' AND lease_owner = 'RW'",
            &[],
        )
        .await?
        .get(0);
    client.query(&claim_head, &[&"RW", &ttl]).await?;
    let head = client
        .query_one(
            "SELECT lease_owner, attempts FROM run_queue WHERE run_id = 'pw-0'",
            &[],
        )
        .await?;
    let head_claimed =
        head.get::<_, Option<String>>(0).as_deref() == Some("RW") && head.get::<_, i32>(1) == 3;
    let sibling_owner: Option<String> = client
        .query_one(
            "SELECT lease_owner FROM run_queue WHERE run_id = 'pw-1'",
            &[],
        )
        .await?
        .get(0);
    let sibling_blocked = sibling_owner.is_none();

    let ok = woken_claimed
        && poison_unclaimed
        && poison_reaped
        && owns_w == 1
        && head_claimed
        && sibling_blocked;
    println!(
        "wedge: woken claimed={woken_claimed} | poison not claimed={poison_unclaimed} | poison reaped={poison_reaped} | partition head woken+claimed={} (owns={owns_w}) | sibling blocked={sibling_blocked}",
        head_claimed
    );
    println!("PASS(a woken budget-spent run wakes; a poison one stays terminal): {ok}");
    Ok(ok)
}

async fn park_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    const PARKS: usize = 10;
    const MAX_ATTEMPTS: i32 = 3;
    println!(
        "\n## park — {PARKS} park/wake cycles with max_attempts={MAX_ATTEMPTS} complete on both claim paths"
    );
    reset(admin_url).await?;

    let (mut client, _h) = connect_app(app_url).await?;
    enqueue(&mut client, "pk-global", 0).await?;
    enqueue_partitioned(&mut client, "pk-part", "pk", 0).await?;
    // The tight budget that made claim-counting fatal: >3 parks would exhaust it.
    client
        .execute("UPDATE run_queue SET max_attempts = $1", &[&MAX_ATTEMPTS])
        .await?;

    let claim = claim_batch_sql(10);
    let acquire = acquire_partitions_sql(4);
    let claim_head = claim_partition_head_sql(4);
    let park = park_sql();
    let ttl: i64 = 60_000;
    let park_ms: i64 = 5;

    // Global path: claim -> park -> wake -> re-claim, PARKS times over.
    let mut global_free = true;
    for cycle in 0..PARKS {
        let got = client.query(&claim, &[&"P1", &ttl]).await?;
        if got.len() != 1 || got[0].get::<_, i32>("attempts") != 0 {
            println!(
                "global cycle {cycle}: claimed {} rows, attempts {:?} (want 1 row, attempts 0)",
                got.len(),
                got.first().map(|r| r.get::<_, i32>("attempts"))
            );
            global_free = false;
            break;
        }
        client.execute(&park, &[&"pk-global", &park_ms]).await?;
        tokio::time::sleep(Duration::from_millis(park_ms as u64 + 10)).await;
    }

    // Partition path: the parked head is re-claimed head-first on every wake.
    // (P1's partition lease is taken once and stays live across the cycles.)
    client.query(&acquire, &[&"P1", &ttl]).await?;
    let mut part_free = true;
    for cycle in 0..PARKS {
        let got = client.query(&claim_head, &[&"P1", &ttl]).await?;
        if got.len() != 1 || got[0].get::<_, i32>("attempts") != 0 {
            println!(
                "partition cycle {cycle}: claimed {} rows, attempts {:?} (want 1 row, attempts 0)",
                got.len(),
                got.first().map(|r| r.get::<_, i32>("attempts"))
            );
            part_free = false;
            break;
        }
        client.execute(&park, &[&"pk-part", &park_ms]).await?;
        tokio::time::sleep(Duration::from_millis(park_ms as u64 + 10)).await;
    }

    // While both runs sit parked (leases released), a zero-grace janitor sweep must
    // retire nothing: the cycles never made anything reap-eligible.
    client.execute(&janitor_sweep_sql(), &[&0i64]).await?;
    let infra_q = "SELECT count(*) FROM runs WHERE status='infrastructure-failure'";
    let infra_mid: i64 = client.query_one(infra_q, &[]).await?.get(0);
    let queued: i64 = client
        .query_one("SELECT count(*) FROM run_queue", &[])
        .await?
        .get(0);
    let janitor_clean = infra_mid == 0 && queued == 2;

    // The final wakes complete both runs — full redelivery budget intact.
    let done = client.query(&claim, &[&"P1", &ttl]).await?;
    let global_done = done.len() == 1 && done[0].get::<_, i32>("attempts") == 0;
    client.execute(&mark_running_sql(), &[&"pk-global"]).await?;
    client.execute(&dequeue_sql(), &[&"pk-global"]).await?;
    let done = client.query(&claim_head, &[&"P1", &ttl]).await?;
    let part_done = done.len() == 1 && done[0].get::<_, i32>("attempts") == 0;
    client.execute(&mark_running_sql(), &[&"pk-part"]).await?;
    client.execute(&dequeue_sql(), &[&"pk-part"]).await?;

    let infra_end: i64 = client.query_one(infra_q, &[]).await?.get(0);
    let drained: i64 = client
        .query_one("SELECT count(*) FROM run_queue", &[])
        .await?
        .get(0);
    let completed = global_done && part_done && infra_end == 0 && drained == 0;

    println!(
        "global wakes free={global_free} | partition wakes free={part_free} | janitor retired nothing={janitor_clean} | both completed with budget intact={completed}"
    );
    let budget_free = global_free && part_free && janitor_clean && completed;
    println!("PASS(park/wake never consumes the redelivery budget): {budget_free}");

    // wamn-fqg.7: the corollary — a budget-spent run that PARKED (NULL lease) is not
    // wedged invisible; it wakes and completes, while an expired-lease poison stays
    // terminal. (Its own reset, so it runs after the park-cycle results are captured.)
    let wedge_ok = park_wedge_check(app_url, admin_url).await?;

    Ok(budget_free && wedge_ok)
}

// ---------------------------------------------------------------------------
// janitor: abandoned run -> infrastructure-failure + dequeued
// ---------------------------------------------------------------------------

async fn janitor_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!(
        "\n## janitor — an abandoned run is swept to infrastructure-failure; healthy untouched"
    );
    reset(admin_url).await?;

    // Seed (superuser): an orphan (expired lease, budget SPENT) that must be
    // retired; a reclaimable run (expired lease, retries LEFT) that must NOT be
    // swept (the budget-check the janitor turns on); and a never-leased healthy run.
    // grace=1h below, and the orphan's lease expired 2h ago, so only it is retired.
    {
        let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
        let conn_task = tokio::spawn(conn);
        let r = client
            .batch_execute(&format!(
                "INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
                   ('{TENANT}','jr-orphan','f',1,'dispatched'), \
                   ('{TENANT}','jr-reclaim','f',1,'dispatched'), \
                   ('{TENANT}','jr-healthy','f',1,'dispatched'); \
                 INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id, run_id, available_at, lease_owner, lease_expires_at, attempts, max_attempts) VALUES \
                   ('{TENANT}','jr-orphan',  now()-interval '3 hour','dead',now()-interval '2 hour',5,5), \
                   ('{TENANT}','jr-reclaim', now()-interval '1 min', 'dead',now()-interval '1 min', 1,5), \
                   ('{TENANT}','jr-healthy', now(), NULL, NULL, 0, 5);"
            ))
            .await;
        drop(client);
        let _ = conn_task.await;
        r.context("seed janitor fixtures")?;
    }

    let (client, _h) = connect_app(app_url).await?;
    // 1-hour grace: the orphan (lease expired 2h ago) is past it; the reclaimable
    // row (expired 1min ago, retries left) is excluded by the budget check anyway.
    client
        .execute(&janitor_sweep_sql(), &[&3_600_000i64])
        .await?;

    let status_q = format!("SELECT status FROM {SCHEMA}.runs WHERE run_id=$1");
    let queued_q = format!("SELECT count(*) FROM {SCHEMA}.run_queue WHERE run_id=$1");
    let status_of = async |run: &str| -> anyhow::Result<String> {
        Ok(client.query_one(&status_q, &[&run]).await?.get(0))
    };
    let queued = async |run: &str| -> anyhow::Result<i64> {
        Ok(client.query_one(&queued_q, &[&run]).await?.get(0))
    };

    let orphan_status = status_of("jr-orphan").await?;
    let orphan_queued = queued("jr-orphan").await?;
    let reclaim_status = status_of("jr-reclaim").await?;
    let reclaim_queued = queued("jr-reclaim").await?;
    let healthy_status = status_of("jr-healthy").await?;
    let healthy_queued = queued("jr-healthy").await?;

    let pass = orphan_status == "infrastructure-failure"
        && orphan_queued == 0
        && reclaim_status == "dispatched"
        && reclaim_queued == 1
        && healthy_status == "dispatched"
        && healthy_queued == 1;
    println!(
        "orphan status={orphan_status} dequeued={} | reclaimable status={reclaim_status} kept={} | healthy status={healthy_status} kept={}",
        orphan_queued == 0,
        reclaim_queued == 1,
        healthy_queued == 1
    );
    println!("PASS(janitor: retires spent orphan, keeps reclaimable + healthy): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// doorbell: NATS-core hint -> subscriber wakes and claims (no polling)
// ---------------------------------------------------------------------------

async fn doorbell_phase(
    app_url: &str,
    admin_url: &str,
    args: &QueueBenchArgs,
    required: bool,
) -> anyhow::Result<bool> {
    use futures_util::StreamExt;
    use wash_runtime::washlet::{NatsConnectionOptions, connect_nats};

    let n = args.doorbell_iters;
    println!("\n## doorbell — {n} enqueue→hint→claim, NATS-core (async warm p50<25ms/p99<100ms)");
    reset(admin_url).await?;

    let nats_opts = NatsConnectionOptions {
        request_timeout: None,
        tls_ca: args.nats_tls_ca.clone(),
        tls_first: false,
        tls_cert: args.nats_tls_cert.clone(),
        tls_key: args.nats_tls_key.clone(),
    };
    let nats = match connect_nats(args.nats_url.clone(), nats_opts).await {
        Ok(c) => c,
        Err(e) => {
            if required {
                bail!("doorbell mode needs NATS at {}: {e}", args.nats_url);
            }
            println!(
                "(skipping doorbell gate: no NATS at {} — {e})",
                args.nats_url
            );
            return Ok(true);
        }
    };

    let subject = format!("wamn.doorbell.{TENANT}");
    // Ping-pong: exactly one run in flight at a time so each sample is the warm
    // doorbell WAKE latency (enqueue-committed → hint → subscriber claims), not a
    // backlog wait. The subscriber signals claim-done over an in-process channel;
    // the publisher blocks on that before enqueuing the next.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let (sub_client, _hs) = connect_app(app_url).await?;
    let mut subscription = nats.subscribe(subject.clone()).await?;
    nats.flush().await?;
    let claim_one = claim_batch_sql(1);
    let dequeue = dequeue_sql();
    let mark_running = mark_running_sql();
    let subscriber = tokio::spawn(async move {
        for _ in 0..n {
            let Some(_msg) = subscription.next().await else {
                break;
            };
            let rows = sub_client
                .query(&claim_one, &[&"doorbell", &600_000i64])
                .await?;
            let Some(row) = rows.first() else { continue };
            let run_id: String = row.get("run_id");
            // Signal claim-done (the measured point) before the follow-up writes.
            if tx.send(()).await.is_err() {
                break;
            }
            sub_client.execute(&mark_running, &[&run_id]).await?;
            sub_client.execute(&dequeue, &[&run_id]).await?;
        }
        anyhow::Ok(())
    });

    // Publisher: enqueue one run, stamp, publish the hint, wait for the claim.
    let (mut pub_client, _hp) = connect_app(app_url).await?;
    let mut samples: Vec<Duration> = Vec::with_capacity(n);
    for i in 0..n {
        let run_id = format!("db-{i}");
        enqueue(&mut pub_client, &run_id, 0).await?;
        let stamp = Instant::now();
        nats.publish(subject.clone(), run_id.into_bytes().into())
            .await?;
        nats.flush().await?;
        // Backstop a lost fire-and-forget hint so the gate can't hang.
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(())) => samples.push(stamp.elapsed()),
            _ => {
                println!("PASS(doorbell): false (hint lost / subscriber stalled at i={i})");
                return Ok(false);
            }
        }
    }
    let _ = subscriber.await;

    let mut s = samples;
    s.sort();
    let p50 = percentile(&s, 0.50);
    let p99 = percentile(&s, 0.99);
    println!(
        "delivered {}/{n}: p50 {p50:?}  p99 {p99:?}  max {:?}",
        s.len(),
        s.last().copied().unwrap_or_default()
    );
    let pass = s.len() == n && p50 < Duration::from_millis(25) && p99 < Duration::from_millis(100);
    println!("PASS(doorbell async-warm SLO): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// partition: per-partition ownership — partitioned(key) runs dispatch in-order per
// key across concurrent replicas (per-key serialization + in-order + exactly-once),
// and a partition fails over in order when its owner dies.
// ---------------------------------------------------------------------------

async fn partition_phase(
    app_url: &str,
    admin_url: &str,
    args: &QueueBenchArgs,
) -> anyhow::Result<bool> {
    const P: i32 = 6; // partitions (ordered streams)
    const K: i32 = 20; // runs per partition
    let tasks_n = args.concurrency.min(P as usize).max(2);
    let total = (P * K) as usize;

    println!(
        "\n## partition — {tasks_n} claimers over {P} partitions × {K} ordered runs (in-order per key)"
    );
    reset(admin_url).await?;

    // Seed P ordered streams as superuser. run_id = pt-<p>-<seq3>, so lexical order
    // within a partition == the enqueue sequence (all available now, so the
    // dispatch key (available_at, run_id) orders by run_id = seq).
    {
        let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
        let conn_task = tokio::spawn(conn);
        let r = client
            .batch_execute(&format!(
                "INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version, status) \
                   SELECT '{TENANT}', 'pt-'||p||'-'||to_char(g,'FM000'), 'f', 1, 'dispatched' \
                     FROM generate_series(0,{P}-1) p, generate_series(0,{K}-1) g; \
                 INSERT INTO {SCHEMA}.run_queue (tenant_id, run_id, partition_key, priority, available_at) \
                   SELECT '{TENANT}', 'pt-'||p||'-'||to_char(g,'FM000'), 'part-'||p, 0, now() \
                     FROM generate_series(0,{P}-1) p, generate_series(0,{K}-1) g;"
            ))
            .await;
        drop(client);
        let _ = conn_task.await;
        r.context("seed partition streams")?;
    }

    let acquire_sql = Arc::new(acquire_partitions_sql(2));
    let claim_sql = Arc::new(claim_partition_head_sql(P as usize));
    let mark_running = Arc::new(mark_running_sql());
    let dequeue = Arc::new(dequeue_sql());
    let part_ttl: i64 = 600_000; // long: no expiry during the gate (failover is below)
    let run_ttl: i64 = 600_000;

    // Shared dispatch log: (partition_key, seq, monotonic stamp), recorded at claim.
    let log: Arc<Mutex<Vec<(String, u32, u64)>>> = Arc::new(Mutex::new(Vec::with_capacity(total)));
    let stamp = Arc::new(AtomicU64::new(0));

    let mut set = tokio::task::JoinSet::new();
    for w in 0..tasks_n {
        let app_url = app_url.to_string();
        let (acquire_sql, claim_sql) = (acquire_sql.clone(), claim_sql.clone());
        let (mark_running, dequeue) = (mark_running.clone(), dequeue.clone());
        let (log, stamp) = (log.clone(), stamp.clone());
        let owner = format!("pw-{w}");
        set.spawn(async move {
            let (client, _h) = connect_app(&app_url).await?;
            let count_sql = "SELECT count(*) FROM run_queue";
            let mut idle = 0u32;
            loop {
                // Lease acquirable partitions, then claim the head of each I own.
                let acq = client
                    .query(acquire_sql.as_str(), &[&owner, &part_ttl])
                    .await?;
                let claimed = client
                    .query(claim_sql.as_str(), &[&owner, &run_ttl])
                    .await?;
                if claimed.is_empty() {
                    let remaining: i64 = client.query_one(count_sql, &[]).await?.get(0);
                    if remaining == 0 {
                        break;
                    }
                    // Nothing to do this round (others own the remaining partitions);
                    // back off briefly and retry until the queue drains.
                    if acq.is_empty() {
                        idle += 1;
                        if idle > 50_000 {
                            anyhow::bail!("partition gate stalled with {remaining} runs left");
                        }
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    continue;
                }
                idle = 0;
                for row in &claimed {
                    let run_id: String = row.get("run_id");
                    let part: String = row.get("partition_key");
                    let seq: u32 = run_id
                        .rsplit('-')
                        .next()
                        .unwrap_or("")
                        .parse()
                        .unwrap_or(u32::MAX);
                    let s = stamp.fetch_add(1, Ordering::SeqCst);
                    log.lock().unwrap().push((part, seq, s));
                    // "Process" the run: mark running, then dequeue so the partition's
                    // next head unblocks (one in flight per key).
                    client.execute(mark_running.as_str(), &[&run_id]).await?;
                    client.execute(dequeue.as_str(), &[&run_id]).await?;
                }
            }
            anyhow::Ok(())
        });
    }
    while let Some(res) = set.join_next().await {
        res??;
    }

    // Completeness (every run dispatched, no gap) and per-key IN-ORDER dispatch (each
    // partition's stamps are the strict sequence 0..K) across the racing replicas.
    // (No-concurrent-dispatch / exactly-once-in-flight — two owners never running the
    // same key's runs at once — is the failover check below + the live-apply in-flight
    // gate; here a single owner drains each key and dequeues before its next claim, so
    // the unique check is a completeness cross-check, not a duplicate detector.)
    let recs = log.lock().unwrap().clone();
    let unique: HashSet<(&str, u32)> = recs.iter().map(|(p, s, _)| (p.as_str(), *s)).collect();
    let complete = recs.len() == total && unique.len() == total;

    let mut in_order = true;
    for p in 0..P {
        let key = format!("part-{p}");
        let mut seqs: Vec<(u64, u32)> = recs
            .iter()
            .filter(|(pk, _, _)| *pk == key)
            .map(|(_, s, st)| (*st, *s))
            .collect();
        seqs.sort_by_key(|&(st, _)| st);
        let ordered: Vec<u32> = seqs.iter().map(|&(_, s)| s).collect();
        let expected: Vec<u32> = (0..K as u32).collect();
        if ordered != expected {
            in_order = false;
            println!("partition {key} dispatched out of order: {ordered:?}");
        }
    }
    println!(
        "dispatched {} (unique {}), complete={complete}, in_order={in_order}",
        recs.len(),
        unique.len()
    );

    // In-order failover: a partition's owner dies mid-stream; another replica takes
    // the whole key and finishes it in order.
    let failover = partition_failover(app_url, admin_url).await?;

    // D20 (R6): the head-unavailability POLICY — blocking holds/wedges a key, leapfrog
    // overtakes/releases.
    let policy = partition_policy_cases(app_url, admin_url).await?;

    let pass = complete && in_order && failover && policy;
    println!("PASS(partition in-order + exactly-once + failover + policy): {pass}");
    Ok(pass)
}

/// D20 (R6): the `partitioned(key)` head-unavailability policy, through the live
/// `claim_partition_head_sql` (policy branch) + `janitor_sweep_sql` (wedge
/// exemption). Four keys, seeded as superuser so availability, stream order
/// (`enqueued_at`), lease, and policy are all explicit:
/// - `blk` (DEFAULT = blocking): a backed-off head, FIRST in stream order, and a
///   later ready run — the later run must NOT overtake (the key holds).
/// - `lf` (leapfrog): the same shape — the later run DOES overtake.
/// - `wg` (blocking): an EXHAUSTED head — the janitor must NOT reap it (it wedges
///   the key), and the later run stays blocked behind it.
/// - `lx` (leapfrog): an exhausted head — the janitor reaps it and the key releases.
async fn partition_policy_cases(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!(
        "## partition policy — blocking holds/wedges a key; leapfrog overtakes/releases (D20)"
    );
    reset(admin_url).await?;

    {
        let (client, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
        let conn_task = tokio::spawn(conn);
        let seed = format!(
            "INSERT INTO {SCHEMA}.runs (tenant_id, run_id, flow_id, flow_version, status) VALUES \
               ('{TENANT}','blk-0','f',1,'dispatched'),('{TENANT}','blk-1','f',1,'dispatched'), \
               ('{TENANT}','lf-0','f',1,'dispatched'),('{TENANT}','lf-1','f',1,'dispatched'), \
               ('{TENANT}','wg-0','f',1,'running'),('{TENANT}','wg-1','f',1,'dispatched'), \
               ('{TENANT}','lx-0','f',1,'running'),('{TENANT}','lx-1','f',1,'dispatched'); \
             INSERT INTO {SCHEMA}.run_queue \
               (tenant_id, run_id, partition_key, available_at, enqueued_at, lease_owner, lease_expires_at, attempts, max_attempts, partition_policy) VALUES \
               ('{TENANT}','blk-0','blk', now()+interval '1 hour', now()-interval '2 min', NULL,  NULL,                    0,  20, 'blocking'), \
               ('{TENANT}','blk-1','blk', now()-interval '30 sec', now()-interval '1 min', NULL,  NULL,                    0,  20, 'blocking'), \
               ('{TENANT}','lf-0','lf',   now()+interval '1 hour', now()-interval '2 min', NULL,  NULL,                    0,  20, 'leapfrog'), \
               ('{TENANT}','lf-1','lf',   now()-interval '30 sec', now()-interval '1 min', NULL,  NULL,                    0,  20, 'leapfrog'), \
               ('{TENANT}','wg-0','wg',   now()-interval '3 hour', now()-interval '2 min','dead', now()-interval '2 hour', 20, 20, 'blocking'), \
               ('{TENANT}','wg-1','wg',   now()-interval '30 sec', now()-interval '1 min', NULL,  NULL,                    0,  20, 'blocking'), \
               ('{TENANT}','lx-0','lx',   now()-interval '3 hour', now()-interval '2 min','dead', now()-interval '2 hour', 20, 20, 'leapfrog'), \
               ('{TENANT}','lx-1','lx',   now()-interval '30 sec', now()-interval '1 min', NULL,  NULL,                    0,  20, 'leapfrog');"
        );
        let r = client.batch_execute(&seed).await;
        drop(client);
        let _ = conn_task.await;
        r.context("seed policy cases")?;
    }

    let (client, _h) = connect_app(app_url).await?;
    let acquire = acquire_partitions_sql(8);
    let claim = claim_partition_head_sql(8);
    let janitor = janitor_sweep_sql();
    let ttl: i64 = 600_000;

    // Janitor first (grace 1h): the exhausted heads are orphan-shaped. wg-0
    // (blocking) is EXEMPT — kept, its run left untouched (wedge). lx-0 (leapfrog)
    // is reaped to infrastructure-failure.
    client.execute(&janitor, &[&3_600_000i64]).await?;
    let wg0_present: i64 = client
        .query_one("SELECT count(*) FROM run_queue WHERE run_id='wg-0'", &[])
        .await?
        .get(0);
    let wg0_status: String = client
        .query_one("SELECT status FROM runs WHERE run_id='wg-0'", &[])
        .await?
        .get(0);
    let lx0_present: i64 = client
        .query_one("SELECT count(*) FROM run_queue WHERE run_id='lx-0'", &[])
        .await?
        .get(0);
    let lx0_status: String = client
        .query_one("SELECT status FROM runs WHERE run_id='lx-0'", &[])
        .await?
        .get(0);
    let wedge_kept = wg0_present == 1 && wg0_status == "running";
    let leap_reaped = lx0_present == 0 && lx0_status == "infrastructure-failure";

    // Acquire all four keys, then claim the head of each under its policy.
    client.query(&acquire, &[&"P", &ttl]).await?;
    let heads = client.query(&claim, &[&"P", &ttl]).await?;
    let claimed: HashSet<String> = heads.iter().map(|r| r.get::<_, String>("run_id")).collect();

    let blocking_holds = !claimed.contains("blk-0") && !claimed.contains("blk-1");
    let leapfrog_overtakes = claimed.contains("lf-1");
    let wedge_blocks = !claimed.contains("wg-1");
    let leap_releases = claimed.contains("lx-1");

    let pass = wedge_kept
        && leap_reaped
        && blocking_holds
        && leapfrog_overtakes
        && wedge_blocks
        && leap_releases;
    println!(
        "blocking holds={blocking_holds} | leapfrog overtakes={leapfrog_overtakes} | blocking wedge kept={wedge_kept}+blocks={wedge_blocks} | leapfrog reaped={leap_reaped}+releases={leap_releases}"
    );
    println!("PASS(partition policy blocking/leapfrog + wedge): {pass}");
    Ok(pass)
}

/// Owner A leases partition `pf` and claims its head, then dies (never renews, never
/// completes). While A's partition lease is live, replica B can neither acquire `pf`
/// nor claim its runs; once the lease expires B reacquires the whole key and drains
/// it in order, reclaiming the abandoned head.
async fn partition_failover(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!("## partition failover — owner dies mid-stream; another replica finishes in order");
    reset(admin_url).await?;
    let (mut a, _ha) = connect_app(app_url).await?;
    let (b, _hb) = connect_app(app_url).await?;

    // One partition with three ordered runs.
    for seq in 0..3 {
        enqueue_partitioned(&mut a, &format!("pf-{seq}"), "pf", 0).await?;
    }

    let acquire = acquire_partitions_sql(4);
    let claim = claim_partition_head_sql(4);
    let mark_running = mark_running_sql();
    let dequeue = dequeue_sql();
    let short_ttl: i64 = 500;

    // A leases pf and claims the head pf-0, then abandons it (no renew, no dequeue).
    let a_owned = a.query(&acquire, &[&"A", &short_ttl]).await?;
    let a_owns = a_owned
        .iter()
        .any(|r| r.get::<_, String>("partition_key") == "pf");
    let a_head = a.query(&claim, &[&"A", &short_ttl]).await?;
    let a_got_head = a_head.len() == 1 && a_head[0].get::<_, String>("run_id") == "pf-0";

    // While A's partition lease is live, B can neither acquire pf nor claim its runs.
    let b_try = b.query(&acquire, &[&"B", &short_ttl]).await?;
    let b_blocked_acq = !b_try
        .iter()
        .any(|r| r.get::<_, String>("partition_key") == "pf");
    let b_head = b.query(&claim, &[&"B", &short_ttl]).await?;
    let b_blocked_claim = b_head.is_empty();

    // A dies: wait past both the partition lease and the abandoned run lease.
    tokio::time::sleep(Duration::from_millis(short_ttl as u64 + 250)).await;

    // B reacquires pf and drains it in order (reclaiming the abandoned pf-0). The
    // reclaimed head arrives with attempts==1 (A's first claim was FREE — attempts
    // counts crash evidence, and A dying holding the lease is the first unit) —
    // proof it is the SAME abandoned in-flight run redelivered, not a
    // fresh/duplicate dispatch. Together with B being blocked while A's lease was
    // live (b_blocked_claim), this is the exactly-once-in-flight guarantee: pf-0 was
    // never dispatched to two owners at once, and it is delivered again only after
    // the first owner provably released.
    let b_reacq = b.query(&acquire, &[&"B", &600_000i64]).await?;
    let b_got = b_reacq
        .iter()
        .any(|r| r.get::<_, String>("partition_key") == "pf");
    let mut order: Vec<String> = Vec::new();
    let mut pf0_reclaim_attempts: i32 = 0;
    loop {
        let claimed = b.query(&claim, &[&"B", &600_000i64]).await?;
        if claimed.is_empty() {
            break;
        }
        for row in &claimed {
            let run_id: String = row.get("run_id");
            if run_id == "pf-0" {
                pf0_reclaim_attempts = row.get("attempts");
            }
            order.push(run_id.clone());
            b.execute(&mark_running, &[&run_id]).await?;
            b.execute(&dequeue, &[&run_id]).await?;
        }
    }
    let in_order = order == ["pf-0", "pf-1", "pf-2"];
    let reclaimed_once = pf0_reclaim_attempts == 1;

    let pass = a_owns
        && a_got_head
        && b_blocked_acq
        && b_blocked_claim
        && b_got
        && in_order
        && reclaimed_once;
    println!(
        "A owns pf={a_owns} claims head={a_got_head} | B blocked (acq={b_blocked_acq}, claim={b_blocked_claim}) | B reacquired={b_got} order={order:?} pf-0 reclaim attempts={pf0_reclaim_attempts}"
    );
    println!("PASS(partition failover in-order + exactly-once reclaim): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// ceiling (EVT-C7): full-lifecycle transitions/sec — measurement, not gates
// (docs/event-plane-jetstream.md §10; curves + CSVs, only sanity asserts gate)
// ---------------------------------------------------------------------------

/// Which lifecycle shape the claimers drive.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LifecyclePath {
    /// The production run-worker path (fqg.18): `claim_dispatch` (claim +
    /// mark-running + dispatch read, one statement) then `complete_dequeue`
    /// (complete + dequeue, one statement). The number of record.
    Combined,
    /// The pre-fqg.18 split-builder path — `claim_batch(batch)` then per run
    /// `select_run_dispatch` + `mark_running` + `update_run_completed` +
    /// `dequeue` — the comparison curve, swept across batch-claim sizes.
    Split { batch: usize },
}

impl LifecyclePath {
    fn label(self) -> String {
        match self {
            LifecyclePath::Combined => "combined".to_string(),
            LifecyclePath::Split { batch } => format!("split-b{batch}"),
        }
    }
}

/// Shared state between the open-loop producers and the claimers of one
/// offered-load window.
#[derive(Default)]
struct CeilingShared {
    /// run_id → enqueue start. Inserted just BEFORE the write-ahead txn so a
    /// fast completion can never race past its entry; the sojourn therefore
    /// includes the enqueue write itself (the full lifecycle).
    enq_at: Mutex<HashMap<String, Instant>>,
    /// (completion instant, enqueue→completion sojourn).
    samples: Mutex<Vec<(Instant, Duration)>>,
    /// Every completed run_id — the exactly-once witness. Shared (not
    /// task-local) so an aborted drain cannot lose it.
    completed: Mutex<Vec<String>>,
    enqueued: AtomicU64,
    /// false: keep polling for work; true: exit once the queue is empty.
    drain: AtomicBool,
}

fn record_completion(shared: &CeilingShared, run_id: String) {
    let start = shared.enq_at.lock().unwrap().remove(&run_id);
    let now = Instant::now();
    if let Some(s) = start {
        shared.samples.lock().unwrap().push((now, now - s));
    }
    shared.completed.lock().unwrap().push(run_id);
}

/// On an empty claim: park briefly, or — in drain mode — exit once the queue
/// is actually empty (a leased-but-uncompleted row is still work in flight).
async fn drained_or_done(client: &Client, shared: &CeilingShared) -> anyhow::Result<bool> {
    if shared.drain.load(Ordering::Relaxed) {
        let left: i64 = client
            .query_one("SELECT count(*) FROM run_queue", &[])
            .await?
            .get(0);
        if left == 0 {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    } else {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    Ok(false)
}

/// Closed-loop claimers driving the chosen lifecycle path. Statements are
/// prepared once per connection, matching the plugin's `prepare_cached` wire
/// shape (the measurement is the mechanism, not Parse overhead).
fn spawn_claimers(
    set: &mut tokio::task::JoinSet<anyhow::Result<()>>,
    app_url: &str,
    path: LifecyclePath,
    n: usize,
    shared: &Arc<CeilingShared>,
) {
    for w in 0..n {
        let app_url = app_url.to_string();
        let shared = shared.clone();
        let owner = format!("ceil-{w}");
        set.spawn(async move {
            let (client, _h) = connect_app(&app_url).await?;
            let ttl: i64 = 60_000;
            let result = serde_json::json!({"ok": true});
            match path {
                LifecyclePath::Combined => {
                    let claim = client.prepare(&claim_dispatch_sql()).await?;
                    let complete = client.prepare(&complete_dequeue_sql()).await?;
                    loop {
                        let rows = client.query(&claim, &[&owner, &ttl]).await?;
                        let Some(row) = rows.first() else {
                            if drained_or_done(&client, &shared).await? {
                                break;
                            }
                            continue;
                        };
                        let run_id: String = row.get("run_id");
                        client.execute(&complete, &[&run_id, &result]).await?;
                        record_completion(&shared, run_id);
                    }
                }
                LifecyclePath::Split { batch } => {
                    let claim = client.prepare(&claim_batch_sql(batch)).await?;
                    let read = client
                        .prepare(&wamn_run_store::sql::select_run_dispatch_sql())
                        .await?;
                    let mark = client.prepare(&mark_running_sql()).await?;
                    let complete = client
                        .prepare(&wamn_run_store::sql::update_run_completed_sql())
                        .await?;
                    let deq = client.prepare(&dequeue_sql()).await?;
                    loop {
                        let rows = client.query(&claim, &[&owner, &ttl]).await?;
                        if rows.is_empty() {
                            if drained_or_done(&client, &shared).await? {
                                break;
                            }
                            continue;
                        }
                        for row in &rows {
                            let run_id: String = row.get("run_id");
                            client.query(&read, &[&run_id]).await?;
                            client.execute(&mark, &[&run_id]).await?;
                            client.execute(&complete, &[&run_id, &result]).await?;
                            client.execute(&deq, &[&run_id]).await?;
                            record_completion(&shared, run_id);
                        }
                    }
                }
            }
            anyhow::Ok(())
        });
    }
}

/// Open-loop producers: `producers_n` connections each pacing
/// `offered / producers_n` lifecycles/sec of write-ahead + enqueue for `secs`.
/// Catch-up pacing: a slow txn is followed by a burst back onto schedule, so
/// `offered` is a schedule, not a closed loop (the achieved rate is reported
/// separately and divergence is a saturation signal).
async fn produce_window(
    app_url: &str,
    offered: f64,
    secs: u64,
    producers_n: usize,
    shared: Arc<CeilingShared>,
    tag: String,
) -> anyhow::Result<()> {
    let mut set = tokio::task::JoinSet::new();
    for w in 0..producers_n {
        let app_url = app_url.to_string();
        let shared = shared.clone();
        let tag = tag.clone();
        let per_sec = offered / producers_n as f64;
        set.spawn(async move {
            let (mut client, _h) = connect_app(&app_url).await?;
            let wa = client.prepare(&write_ahead_run_sql()).await?;
            let enq = client.prepare(&enqueue_sql()).await?;
            let window = Duration::from_secs(secs);
            let start = Instant::now();
            let mut sent = 0u64;
            while start.elapsed() < window {
                let due = (start.elapsed().as_secs_f64() * per_sec) as u64 + 1;
                while sent < due && start.elapsed() < window {
                    let run_id = format!("cl-{tag}-{w}-{sent}");
                    shared
                        .enq_at
                        .lock()
                        .unwrap()
                        .insert(run_id.clone(), Instant::now());
                    let tx = client.transaction().await?;
                    tx.execute(&wa, &[&run_id, &"f", &1i32]).await?;
                    tx.execute(&enq, &[&run_id, &Option::<&str>::None, &0i32, &0i64])
                        .await?;
                    tx.commit().await?;
                    shared.enqueued.fetch_add(1, Ordering::Relaxed);
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

/// Join the claimers within `budget`; a timeout aborts them (the level's
/// backlog never drained — itself a saturation signal).
async fn drain_claimers(
    claimers: &mut tokio::task::JoinSet<anyhow::Result<()>>,
    budget: Duration,
) -> anyhow::Result<bool> {
    match tokio::time::timeout(budget, async {
        while let Some(r) = claimers.join_next().await {
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
            claimers.abort_all();
            while claimers.join_next().await.is_some() {}
            Ok(false)
        }
    }
}

/// Exactly-once (always) + completeness (when the backlog drained) over the
/// window — the sanity asserts that gate the campaign.
fn ceiling_sanity(shared: &CeilingShared, drained: bool, context: &str) -> bool {
    let completed = shared.completed.lock().unwrap();
    let unique: HashSet<&str> = completed.iter().map(String::as_str).collect();
    let exactly_once = unique.len() == completed.len();
    let enqueued = shared.enqueued.load(Ordering::Relaxed);
    let complete = !drained || completed.len() as u64 == enqueued;
    if !exactly_once || !complete {
        println!(
            "  SANITY FAIL ({context}): enqueued={enqueued} completed={} unique={} drained={drained}",
            completed.len(),
            unique.len()
        );
    }
    if !drained {
        println!(
            "  ({context}: drain timed out — completeness unchecked, level counted saturated)"
        );
    }
    exactly_once && complete
}

/// One ramp level: `level_secs` of offered load, then a drain. Stats cover the
/// offered window only (drain completions are excluded from the percentiles).
async fn ceiling_level(
    app_url: &str,
    admin_url: &str,
    path: LifecyclePath,
    offered: f64,
    args: &QueueBenchArgs,
    tag: &str,
) -> anyhow::Result<(ceiling::LevelStats, bool)> {
    reset(admin_url).await?;
    let shared = Arc::new(CeilingShared::default());
    let mut claimers = tokio::task::JoinSet::new();
    spawn_claimers(&mut claimers, app_url, path, args.concurrency, &shared);

    let window_start = Instant::now();
    produce_window(
        app_url,
        offered,
        args.level_secs,
        args.producers,
        shared.clone(),
        tag.to_string(),
    )
    .await?;
    let window_end = Instant::now();
    shared.drain.store(true, Ordering::Relaxed);
    let drained =
        drain_claimers(&mut claimers, Duration::from_secs(args.level_secs * 3 + 60)).await?;

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
        achieved_enqueue: shared.enqueued.load(Ordering::Relaxed) as f64 / window,
        achieved_complete: wc as f64 / window,
        p50_ms: percentile(&window_sojourns, 0.50).as_secs_f64() * 1e3,
        p99_ms: percentile(&window_sojourns, 0.99).as_secs_f64() * 1e3,
        p999_ms: percentile(&window_sojourns, 0.999).as_secs_f64() * 1e3,
        window_completed: wc,
        drained,
    };
    let sanity = ceiling_sanity(&shared, drained, tag);
    Ok((stats, sanity))
}

/// Drive the find-knee ramp (coarse doubling, then bisect) for one path.
async fn ceiling_ramp(
    app_url: &str,
    admin_url: &str,
    path: LifecyclePath,
    args: &QueueBenchArgs,
) -> anyhow::Result<(ceiling::Ramp, bool)> {
    println!(
        "\n### ramp — {} path (saturation = p99 doubling / rate divergence / drain timeout)",
        path.label()
    );
    // A discarded warmup level: the FIRST measured level is the p99-doubling
    // baseline, so it must not carry cold-cache noise.
    let warm_tag = format!("{}-warm", path.label());
    let (_, warm_ok) =
        ceiling_level(app_url, admin_url, path, args.base_rate, args, &warm_tag).await?;
    let mut ramp = ceiling::Ramp::new(args.base_rate, 0.15, 16);
    let mut sanity = warm_ok;
    while let Some(offered) = ramp.next_offered() {
        let tag = format!("{}-{offered:.0}", path.label());
        let (stats, ok) = ceiling_level(app_url, admin_url, path, offered, args, &tag).await?;
        println!(
            "  offered {:>7.0}/s | enq {:>7.1}/s | done {:>7.1}/s | p50 {:>8.2}ms p99 {:>8.2}ms p999 {:>8.2}ms | drained={}{}",
            stats.offered,
            stats.achieved_enqueue,
            stats.achieved_complete,
            stats.p50_ms,
            stats.p99_ms,
            stats.p999_ms,
            stats.drained,
            if stats.achieved_enqueue < 0.95 * stats.offered {
                " (producer-limited)"
            } else {
                ""
            }
        );
        sanity &= ok;
        ramp.record(stats);
    }
    match ramp.knee() {
        Some(k) => println!(
            "  knee({}) = {:.0}/s offered → {:.0}/s sustained transitions",
            path.label(),
            k.offered,
            k.achieved_complete
        ),
        None => println!(
            "  knee({}) — none: saturated at the base rate",
            path.label()
        ),
    }
    Ok((ramp, sanity))
}

/// One bloat/depth probe row: (t_secs, relation bytes, dead tuples, queue depth).
type BloatRow = (u64, i64, i64, i64);

/// Probe `run_queue` size / dead tuples / depth on the admin connection every
/// `every_secs` (plus a final sample when stopped).
fn spawn_bloat_sampler(
    admin_url: String,
    stop: Arc<AtomicBool>,
    every_secs: u64,
) -> tokio::task::JoinHandle<anyhow::Result<Vec<BloatRow>>> {
    tokio::spawn(async move {
        let (client, conn) = tokio_postgres::connect(&admin_url, NoTls).await?;
        let _task = tokio::spawn(async move {
            let _ = conn.await;
        });
        let probe = format!(
            "SELECT pg_relation_size('{SCHEMA}.run_queue'), \
                    COALESCE((SELECT n_dead_tup FROM pg_stat_all_tables \
                               WHERE schemaname = '{SCHEMA}' AND relname = 'run_queue'), 0)::bigint, \
                    (SELECT count(*) FROM {SCHEMA}.run_queue)"
        );
        let start = Instant::now();
        let mut rows = Vec::new();
        loop {
            let r = client.query_one(&probe, &[]).await?;
            rows.push((
                start.elapsed().as_secs(),
                r.get::<_, i64>(0),
                r.get::<_, i64>(1),
                r.get::<_, i64>(2),
            ));
            if stop.load(Ordering::Relaxed) {
                break;
            }
            for _ in 0..every_secs {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        anyhow::Ok(rows)
    })
}

/// The sustained soak at 80% of knee: per-30s completion windows + the bloat
/// curve (relation size, dead tuples) over the run.
async fn ceiling_soak(
    app_url: &str,
    admin_url: &str,
    rate: f64,
    args: &QueueBenchArgs,
) -> anyhow::Result<bool> {
    println!(
        "\n### soak — {rate:.0}/s (80% of knee) for {}s, combined path, bloat probed",
        args.soak_secs
    );
    reset(admin_url).await?;
    let shared = Arc::new(CeilingShared::default());
    let mut claimers = tokio::task::JoinSet::new();
    spawn_claimers(
        &mut claimers,
        app_url,
        LifecyclePath::Combined,
        args.concurrency,
        &shared,
    );
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = spawn_bloat_sampler(admin_url.to_string(), stop.clone(), 30);

    let start = Instant::now();
    produce_window(
        app_url,
        rate,
        args.soak_secs,
        args.producers,
        shared.clone(),
        "soak".to_string(),
    )
    .await?;
    shared.drain.store(true, Ordering::Relaxed);
    let drained =
        drain_claimers(&mut claimers, Duration::from_secs(args.soak_secs / 2 + 120)).await?;
    stop.store(true, Ordering::Relaxed);
    let bloat = sampler.await??;

    let n_windows = args.soak_secs.div_ceil(30) as usize;
    let mut buckets: Vec<Vec<Duration>> = vec![Vec::new(); n_windows];
    for (at, d) in shared.samples.lock().unwrap().iter() {
        let t = (at.saturating_duration_since(start).as_secs() / 30) as usize;
        if let Some(b) = buckets.get_mut(t) {
            b.push(*d);
        }
    }
    let mut windows_csv = String::from("t_end_secs,completed,rate_per_s,p99_ms\n");
    for (i, mut b) in buckets.into_iter().enumerate() {
        b.sort();
        windows_csv.push_str(&format!(
            "{},{},{:.1},{:.3}\n",
            (i + 1) * 30,
            b.len(),
            b.len() as f64 / 30.0,
            percentile(&b, 0.99).as_secs_f64() * 1e3
        ));
    }
    emit_csv("ceiling-soak-windows", &windows_csv, &args.ceiling_out);
    let mut bloat_csv = String::from("t_secs,rel_size_bytes,dead_tup,queue_depth\n");
    for (t, rel, dead, depth) in &bloat {
        bloat_csv.push_str(&format!("{t},{rel},{dead},{depth}\n"));
    }
    emit_csv("ceiling-soak-bloat", &bloat_csv, &args.ceiling_out);
    if let (Some(first), Some(last)) = (bloat.first(), bloat.last()) {
        println!(
            "  run_queue {} → {} bytes, dead tuples {} → {}",
            first.1, last.1, first.2, last.2
        );
    }
    Ok(ceiling_sanity(&shared, drained, "soak"))
}

/// The burst profile: a steady baseline, a 10× spike for `burst_secs`, then
/// recovery — peak backlog depth and time-to-recover measured.
async fn ceiling_burst(
    app_url: &str,
    admin_url: &str,
    base: f64,
    args: &QueueBenchArgs,
) -> anyhow::Result<bool> {
    let burst_rate = base * 10.0;
    println!(
        "\n### burst — {base:.0}/s baseline, 10× ({burst_rate:.0}/s) for {}s, recovery measured",
        args.burst_secs
    );
    reset(admin_url).await?;
    let shared = Arc::new(CeilingShared::default());
    let mut claimers = tokio::task::JoinSet::new();
    spawn_claimers(
        &mut claimers,
        app_url,
        LifecyclePath::Combined,
        args.concurrency,
        &shared,
    );
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = spawn_bloat_sampler(admin_url.to_string(), stop.clone(), 2);

    let t0 = Instant::now();
    produce_window(
        app_url,
        base,
        30,
        args.producers,
        shared.clone(),
        "burst-base".to_string(),
    )
    .await?;
    let burst_start = t0.elapsed();
    produce_window(
        app_url,
        burst_rate,
        args.burst_secs,
        args.producers,
        shared.clone(),
        "burst-spike".to_string(),
    )
    .await?;
    let burst_end = t0.elapsed();
    produce_window(
        app_url,
        base,
        30,
        args.producers,
        shared.clone(),
        "burst-recovery".to_string(),
    )
    .await?;
    shared.drain.store(true, Ordering::Relaxed);
    let drained = drain_claimers(
        &mut claimers,
        Duration::from_secs(args.burst_secs * 10 + 120),
    )
    .await?;
    let drain_done = t0.elapsed();
    stop.store(true, Ordering::Relaxed);
    let depth_rows = sampler.await??;

    let mut csv = String::from("t_secs,queue_depth\n");
    for (t, _, _, depth) in &depth_rows {
        csv.push_str(&format!("{t},{depth}\n"));
    }
    emit_csv("ceiling-burst-depth", &csv, &args.ceiling_out);

    let baseline_depth = depth_rows
        .iter()
        .filter(|(t, ..)| *t < burst_start.as_secs())
        .map(|r| r.3)
        .max()
        .unwrap_or(0);
    let peak_depth = depth_rows.iter().map(|r| r.3).max().unwrap_or(0);
    let threshold = (2 * baseline_depth).max(50);
    let recovery_secs = depth_rows
        .iter()
        .find(|(t, _, _, d)| *t > burst_end.as_secs() && *d <= threshold)
        .map(|(t, ..)| t.saturating_sub(burst_end.as_secs()));

    let phase_p99 = |from: Duration, to: Duration| -> f64 {
        let mut v: Vec<Duration> = shared
            .samples
            .lock()
            .unwrap()
            .iter()
            .filter(|(at, _)| {
                let e = at.saturating_duration_since(t0);
                e > from && e <= to
            })
            .map(|(_, d)| *d)
            .collect();
        v.sort();
        percentile(&v, 0.99).as_secs_f64() * 1e3
    };
    println!(
        "  base p99 {:.1}ms | spike p99 {:.1}ms | recovery p99 {:.1}ms | peak depth {peak_depth} | \
         depth back under {threshold} {} after burst end | fully drained {:.0}s after burst end",
        phase_p99(Duration::ZERO, burst_start),
        phase_p99(burst_start, burst_end),
        phase_p99(burst_end, drain_done),
        recovery_secs
            .map(|s| format!("{s}s"))
            .unwrap_or_else(|| "NOT REACHED".to_string()),
        (drain_done - burst_end).as_secs_f64(),
    );
    Ok(ceiling_sanity(&shared, drained, "burst"))
}

async fn ceiling_phase(
    app_url: &str,
    admin_url: &str,
    args: &QueueBenchArgs,
) -> anyhow::Result<bool> {
    println!(
        "\n## ceiling (EVT-C7) — full-lifecycle transitions/sec (measurement; only sanity asserts gate)\n\
         level {}s, base {:.0}/s, {} producers, {} claimers",
        args.level_secs, args.base_rate, args.producers, args.concurrency
    );
    let mut sanity = true;
    // (label, (offered, sustained transitions/sec) at the knee level).
    let mut knees: Vec<(String, Option<(f64, f64)>)> = Vec::new();

    // The number of record: the production combined-statement path (fqg.18).
    let (ramp, ok) = ceiling_ramp(app_url, admin_url, LifecyclePath::Combined, args).await?;
    sanity &= ok;
    emit_csv(
        "ceiling-ramp-combined",
        &ceiling::ramp_csv(ramp.levels()),
        &args.ceiling_out,
    );
    let knee_combined = ramp.knee().map(|k| (k.offered, k.achieved_complete));
    knees.push((
        "combined (claim_dispatch + complete_dequeue)".to_string(),
        knee_combined,
    ));

    // Comparison curves: the split-builder path across the batch-claim sizes.
    for batch in [1usize, 8, 32] {
        let (ramp, ok) =
            ceiling_ramp(app_url, admin_url, LifecyclePath::Split { batch }, args).await?;
        sanity &= ok;
        emit_csv(
            &format!("ceiling-ramp-split-b{batch}"),
            &ceiling::ramp_csv(ramp.levels()),
            &args.ceiling_out,
        );
        knees.push((
            format!("split batch={batch}"),
            ramp.knee().map(|k| (k.offered, k.achieved_complete)),
        ));
    }

    // Soak + burst at 80% of the combined knee's SUSTAINED rate (the number of
    // record's path).
    if let Some((_, sustained)) = knee_combined {
        sanity &= ceiling_soak(app_url, admin_url, 0.8 * sustained, args).await?;
        sanity &= ceiling_burst(app_url, admin_url, 0.8 * sustained, args).await?;
    } else {
        println!("(combined path saturated at the base rate — soak/burst skipped)");
    }

    println!("\nknees (full lifecycle enqueue→complete, transitions/sec):");
    for (label, knee) in &knees {
        match knee {
            Some((offered, sustained)) => {
                println!("  {label}: {offered:.0}/s offered → {sustained:.0}/s sustained")
            }
            None => println!("  {label}: none — saturated at the base rate"),
        }
    }
    println!("PASS(ceiling sanity — exactly-once + completeness at every level): {sanity}");
    Ok(sanity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_drift::{Need, assert_stand_in};

    /// wamn-9mg8 [GATE-DRIFT]: queuebench's `run_queue` stand-in vs the schema of
    /// record, through the uniform guard. queuebench drives the per-partition
    /// ownership path (`partition_owner` + the `run_queue_partition` index), so
    /// both are Required; it has no guest terminal-failure path, so
    /// `run_dead_letters` is AbsentByDesign.
    #[test]
    fn queuebench_stand_in_tracks_run_queue_schema_of_record() {
        assert_stand_in(
            "queuebench",
            &queue_ddl("wamn_run"),
            &[
                ("run_queue", Need::Required),
                ("partition_owner", Need::Required),
                ("run_dead_letters", Need::AbsentByDesign),
            ],
        );
    }
}
