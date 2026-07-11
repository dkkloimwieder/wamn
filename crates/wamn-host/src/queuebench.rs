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
//!   janitor    — an abandoned (expired-lease, budget-spent) run is swept to
//!                `infrastructure-failure` and dequeued; a healthy run is untouched.
//!   doorbell   — enqueue publishes a NATS-core hint; a subscriber wakes and
//!                claims with no polling (async warm p50 < 25 ms / p99 < 100 ms).
//!   all        — every mode in sequence.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};
use wamn_run_queue::{
    claim_batch_sql, dequeue_sql, enqueue_sql, janitor_sweep_sql, mark_running_sql,
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
    Janitor,
    Doorbell,
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
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// The ephemeral-schema clone: the 5.7 `runs` (the write-ahead target + the FK)
/// and the 5.14 `run_queue`, schema-qualified, with the house tenant floor. A
/// faithful, self-contained stand-in for `deploy/run-state.sql` + `run-queue.sql`
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
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.run_queue (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text, \
            priority int NOT NULL DEFAULT 0, available_at timestamptz NOT NULL DEFAULT now(), \
            lease_owner text, lease_expires_at timestamptz, \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX run_queue_claimable ON {schema}.run_queue (tenant_id, available_at, lease_expires_at);\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;"
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
        if run_all || args.mode == Mode::Janitor {
            pass &= janitor_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Doorbell {
            pass &=
                doorbell_phase(&app_url, &admin_url, &args, args.mode == Mode::Doorbell).await?;
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
    let got_a = a.query(&claim, &[&"A", &short_ttl]).await?;
    let a_ok = got_a.len() == 1 && got_a[0].get::<_, String>("run_id") == "rc-1";

    // B cannot steal a live lease.
    let blocked = b.query(&claim, &[&"B", &short_ttl]).await?;
    let b_blocked = blocked.is_empty();

    // After the lease expires, B reclaims it — attempts bumped to 2.
    tokio::time::sleep(Duration::from_millis(short_ttl as u64 + 250)).await;
    let reclaimed = b.query(&claim, &[&"B", &short_ttl]).await?;
    let b_reclaimed = reclaimed.len() == 1
        && reclaimed[0].get::<_, String>("run_id") == "rc-1"
        && reclaimed[0].get::<_, i32>("attempts") == 2;

    println!(
        "A claimed={a_ok}, B blocked while lease live={b_blocked}, B reclaimed after expiry={b_reclaimed}"
    );
    let pass = a_ok && b_blocked && b_reclaimed;
    println!("PASS(lease failover): {pass}");
    Ok(pass)
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
