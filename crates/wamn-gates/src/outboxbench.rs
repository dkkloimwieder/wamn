//! The `outboxbench` subcommand: the EVT-C2 outbox-trigger overhead campaign
//! (docs/event-plane-jetstream.md §10) — the cost the customer pays for D4
//! row events, quantifying R8c with numbers instead of adjectives.
//!
//! A MEASUREMENT campaign, not a regression gate: curves land in
//! `docs/ceilings.md` + `docs/ceilings-data/` (§11 provenance), and only the
//! sanity asserts gate — the trigger fires exactly once per written row, and
//! GC never prunes a pending row. Pure host-side (raw `tokio_postgres`, no
//! wasm): the trigger is a Postgres mechanism. It provisions a fresh ephemeral
//! schema (`wamn_outbox_bench`) through the superuser, applies the REAL 3.2
//! tenant floor (`Migration::create`) for a small entity catalog plus the
//! run-queue outbox clone, and toggles the REAL emitter plans
//! (`Migration::outbox_triggers` / `drop_outbox_triggers`) between phases —
//! paired same-table A/B, so the with/without delta is the trigger and nothing
//! else (a closing baseline re-measure bounds heap/cache drift).
//!
//! Modes:
//!   trigger — single-row INSERT/UPDATE/DELETE p50/p99 + WAL bytes/row,
//!             measured baseline → with-trigger → baseline-after on the SAME
//!             table (VACUUM + CHECKPOINT before each op batch so the
//!             full-page-image regime is comparable across phases).
//!   bulk    — one single-statement UPDATE of 1k/10k/100k registered rows,
//!             with and without the trigger: txn duration, WAL bytes, outbox
//!             rows, and the amplification factor (the number wamn-vbl's
//!             registration-driven emission is sized against).
//!   growth  — sustained row-event INSERT load (catch-up pacing) with a
//!             dispatcher-shaped acker, sweeping the prune cadence
//!             {off, 60 s, 600 s = the d8v maintenance interval} while a
//!             bloat probe samples outbox relation size / dead tuples /
//!             pending / total. Pending sentinel rows prove GC never touches
//!             an undispatched event.
//!   all     — trigger, bulk, growth in sequence.
//!
//! Not `--mode all` of some other gate: run it explicitly via
//! deploy/gates/outboxbench-job.yaml. A single run is the record (unlike the C7
//! two-run practice): there is no knee search a one-sided disk stall can
//! poison — the headline numbers are byte counts and medians, and a stall
//! shows up visibly as a p99 outlier rather than corrupting the result.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};
use wamn_ddl::{Confirmation, Migration, OutboxOptions};
use wamn_gate_harness::{check, emit_csv, percentile};
use wamn_run_queue::outbox_prune_sql;

const SCHEMA: &str = "wamn_outbox_bench";
const TENANT: &str = "outbox-tenant";
/// Pending sentinel rows seeded per growth cadence: never acked, so the prune
/// must never touch them (`dispatched_at IS NULL`). Their `table_name` keeps
/// the acker's hands off them too.
const SENTINEL_TABLE: &str = "c2-sentinel";
const SENTINELS: i64 = 100;

/// The registered entity the trigger rides on: one modest row shape (row WIDTH
/// as a payload-size axis is phase 2 — noted, not built). The floor adds the
/// managed `id uuid` PK + `tenant_id`.
const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "c2-outbox-bench",
  "version": 1,
  "entities": [
    { "id": "items", "name": "items", "fields": [
      { "id": "sku", "name": "sku", "type": { "kind": "text", "max-len": 64 } },
      { "id": "qty", "name": "qty", "type": { "kind": "int" } },
      { "id": "price", "name": "price", "type": { "kind": "numeric", "precision": 12, "scale": 2 } },
      { "id": "flag", "name": "flag", "type": { "kind": "bool" } },
      { "id": "note", "name": "note", "type": { "kind": "text" }, "nullable": true }
    ] }
  ]
}"#;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Trigger,
    Bulk,
    Growth,
    All,
}

#[derive(Debug, Args)]
pub struct OutboxBenchArgs {
    /// App (writer) Postgres URL — the NOSUPERUSER wamn_app role whose writes
    /// fire the trigger. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema, applies the
    /// trigger plans, VACUUM/CHECKPOINT, and reads WAL LSNs.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which measurement to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// Trigger mode: single-row operations per op batch (per phase).
    #[arg(long, default_value_t = 1000)]
    pub iters: usize,

    /// Growth mode: sustained row-event INSERT rate (rows/sec, open-loop with
    /// catch-up pacing).
    #[arg(long, default_value_t = 100.0)]
    pub growth_rate: f64,

    /// Growth mode: minimum seconds of load per cadence (a cadence runs for
    /// `max(growth_secs, 2*cadence + 60)` so even the slowest sweep prunes at
    /// least twice).
    #[arg(long, default_value_t = 120)]
    pub growth_secs: u64,

    /// Growth mode: prune cadences to sweep, comma-separated seconds
    /// (0 = never prune). The default's 600 is the d8v production maintenance
    /// interval.
    #[arg(long, default_value = "0,60,600")]
    pub growth_cadences: String,

    /// Growth mode: rows per outbox_prune_sql batch (each cadence tick loops
    /// batches until one comes back short — the d8v backlog-drain semantics).
    #[arg(long, default_value_t = 5000)]
    pub prune_batch: usize,

    /// Growth mode: prune retention in milliseconds (production is 7 days; the
    /// bench shortens it so rows become prunable within the run — the swept
    /// variable is the cadence, not the retention).
    #[arg(long, default_value_t = 5000)]
    pub retention_ms: i64,

    /// Also write each CSV to this directory (stdout always carries them
    /// between `=== BEGIN/END CSV <name> ===` markers).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// Parse the `--growth-cadences` list (seconds; 0 = never prune).
fn parse_cadences(s: &str) -> anyhow::Result<Vec<u64>> {
    let v: Vec<u64> = s
        .split(',')
        .map(|p| p.trim().parse::<u64>())
        .collect::<Result<_, _>>()
        .with_context(|| format!("bad --growth-cadences {s:?} (want e.g. \"0,60,600\")"))?;
    if v.is_empty() {
        bail!("--growth-cadences is empty");
    }
    Ok(v)
}

/// Seconds of sustained load for one cadence: at least `growth_secs`, and long
/// enough for the slowest cadence to prune at least twice.
fn cadence_duration(growth_secs: u64, cadence: u64) -> u64 {
    growth_secs.max(2 * cadence + 60)
}

fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("bench catalog parse: {e}"))
}

/// The run-queue outbox clone in the ephemeral schema (deploy/sql/run-queue.sql
/// shape: identity seq, event CHECK, pending partial index, hardened tenant
/// floor). The trigger plan targets it schema-qualified.
fn outbox_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.outbox (\
            tenant_id text NOT NULL CHECK (tenant_id <> ''), \
            seq bigint GENERATED ALWAYS AS IDENTITY, \
            table_name text NOT NULL, \
            event text NOT NULL CHECK (event IN ('insert', 'update', 'delete')), \
            payload jsonb, \
            created_at timestamptz NOT NULL DEFAULT now(), \
            dispatched_at timestamptz, \
            PRIMARY KEY (tenant_id, seq));\
         CREATE INDEX outbox_pending ON {schema}.outbox (tenant_id, seq) WHERE dispatched_at IS NULL;\
         ALTER TABLE {schema}.outbox ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.outbox FORCE ROW LEVEL SECURITY;\
         CREATE POLICY outbox_tenant ON {schema}.outbox \
            USING (tenant_id = NULLIF(current_setting('app.tenant', true), '')) \
            WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.outbox TO wamn_app;"
    )
}

/// Drop-and-recreate the ephemeral schema: the 3.2 floor for the bench catalog
/// (applied under `search_path` so the unqualified generated DDL lands here)
/// plus the outbox clone. No trigger yet — phases toggle it.
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
            .batch_execute(&outbox_ddl(SCHEMA))
            .await
            .context("apply the outbox clone")?;
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

/// A long-lived admin connection (trigger toggles, VACUUM/CHECKPOINT, WAL
/// LSNs, truncates, the bloat probe). `search_path` pinned to the bench schema
/// so the emitter's unqualified `CREATE OR REPLACE FUNCTION` lands here.
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

/// A wamn_app writer connection pinned to the schema + tenant claim (the RLS
/// floor the production write path runs under).
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

/// Apply the REAL emitter plan (shared function + per-table trigger) under the
/// admin connection's `search_path`.
async fn apply_trigger(admin: &Client) -> anyhow::Result<()> {
    let plan = Migration::outbox_triggers(
        &catalog()?,
        &OutboxOptions {
            schema: SCHEMA.into(),
        },
    )
    .map_err(|e| anyhow::anyhow!("outbox plan: {e}"))?;
    let sql = plan
        .sql(Confirmation::None)
        .map_err(|e| anyhow::anyhow!("outbox sql: {e}"))?;
    admin
        .batch_execute(&sql)
        .await
        .context("apply outbox trigger plan")
}

/// Apply the REAL drop counterpart (destructive → ConfirmedWithBackup gated).
async fn drop_trigger(admin: &Client) -> anyhow::Result<()> {
    let plan = Migration::drop_outbox_triggers(&catalog()?)
        .map_err(|e| anyhow::anyhow!("drop plan: {e}"))?;
    let sql = plan
        .sql(Confirmation::ConfirmedWithBackup)
        .map_err(|e| anyhow::anyhow!("drop sql: {e}"))?;
    admin
        .batch_execute(&sql)
        .await
        .context("apply outbox trigger drop plan")
}

/// The instance WAL INSERT position (LSN as text; read on the admin
/// connection — the LSN is instance-global, so bracketing the app
/// connection's sequential statements from here is exact). The insert
/// position, not `pg_current_wal_lsn()` (the flushed position): under
/// `synchronous_commit=off`/`fsync=off` (the in-cluster fixture pod) nothing
/// flushes inside a fast batch and the flushed position reads ~0 bytes moved —
/// the insert position measures WAL *generated*, which is the quantity here,
/// regardless of flush policy.
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

/// VACUUM (ANALYZE) both tables, then CHECKPOINT — run before every measured
/// batch so each starts from the same regime: no backlog of dead tuples, and a
/// fresh checkpoint means every first page touch pays a full-page image in
/// EVERY phase (comparable pairs; the FPI share is a real cost the outbox
/// pages also pay in production). VACUUM can't run inside an (implicit) txn
/// block, so each is its own simple-query round trip.
async fn normalize(admin: &Client) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!("VACUUM (ANALYZE) {SCHEMA}.\"items\""))
        .await?;
    admin
        .batch_execute(&format!("VACUUM (ANALYZE) {SCHEMA}.outbox"))
        .await?;
    admin.batch_execute("CHECKPOINT").await?;
    Ok(())
}

async fn outbox_counts(admin: &Client) -> anyhow::Result<(i64, i64, i64, i64)> {
    let r = admin
        .query_one(
            &format!(
                "SELECT count(*), \
                        count(*) FILTER (WHERE event = 'insert' AND table_name = 'items'), \
                        count(*) FILTER (WHERE event = 'update' AND table_name = 'items'), \
                        count(*) FILTER (WHERE event = 'delete' AND table_name = 'items') \
                   FROM {SCHEMA}.outbox"
            ),
            &[],
        )
        .await?;
    Ok((r.get(0), r.get(1), r.get(2), r.get(3)))
}

pub async fn run(args: OutboxBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "outboxbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;
    let cadences = parse_cadences(&args.growth_cadences)?;

    println!("# wamn-gates EVT-C2 outboxbench (schema {SCHEMA}, tenant {TENANT})");
    provision(&admin_url)
        .await
        .context("provision ephemeral schema")?;

    let run_all = args.mode == Mode::All;
    let mut pass = true;
    let outcome = async {
        if run_all || args.mode == Mode::Trigger {
            pass &= trigger_phase(&app_url, &admin_url, &args).await?;
        }
        if run_all || args.mode == Mode::Bulk {
            pass &= bulk_phase(&app_url, &admin_url, &args).await?;
        }
        if run_all || args.mode == Mode::Growth {
            pass &= growth_phase(&app_url, &admin_url, &args, &cadences).await?;
        }
        anyhow::Ok(())
    }
    .await;

    let _ = teardown(&admin_url).await;
    outcome?;

    println!("\noutboxbench complete — overall PASS: {pass}");
    if !pass {
        bail!("an EVT-C2 sanity assert failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// trigger: paired same-table single-row op latency + WAL/row
// ---------------------------------------------------------------------------

struct OpStats {
    p50_ms: f64,
    p99_ms: f64,
    wal_per_row: f64,
}

/// One op batch on the app connection: run `n` statements (prepared once — the
/// plugin's `prepare_cached` wire shape), timing each, with WAL bracketed on
/// the admin connection.
async fn op_batch(
    admin: &Client,
    n: usize,
    mut op: impl AsyncFnMut(usize) -> anyhow::Result<()>,
) -> anyhow::Result<OpStats> {
    normalize(admin).await?;
    let wal0 = wal_lsn(admin).await?;
    let mut samples: Vec<Duration> = Vec::with_capacity(n);
    for i in 0..n {
        let t = Instant::now();
        op(i).await?;
        samples.push(t.elapsed());
    }
    let wal = wal_since(admin, &wal0).await?;
    samples.sort();
    Ok(OpStats {
        p50_ms: percentile(&samples, 0.50).as_secs_f64() * 1e3,
        p99_ms: percentile(&samples, 0.99).as_secs_f64() * 1e3,
        wal_per_row: wal as f64 / n as f64,
    })
}

async fn trigger_phase(
    app_url: &str,
    admin_url: &str,
    args: &OutboxBenchArgs,
) -> anyhow::Result<bool> {
    let n = args.iters;
    println!(
        "\n## trigger (C2) — single-row op latency + WAL/row, {n} ops/batch, \
         paired same-table baseline → with-trigger → baseline-after"
    );
    let (admin, _ah) = connect_admin(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    let insert = app
        .prepare(
            "INSERT INTO \"items\" (tenant_id, sku, qty, price, flag, note) \
             VALUES (current_setting('app.tenant', true), $1, $2, $3::text::numeric, $4, $5) \
             RETURNING id::text",
        )
        .await?;
    // `$1::text::uuid`, not `$1::uuid`: a bare uuid cast types the parameter
    // itself as uuid and tokio_postgres cannot serialize a String into it (the
    // publish-catalog `::text::jsonb` lesson).
    let update = app
        .prepare("UPDATE \"items\" SET qty = qty + 1, note = $2 WHERE id = $1::text::uuid")
        .await?;
    let delete = app
        .prepare("DELETE FROM \"items\" WHERE id = $1::text::uuid")
        .await?;

    let mut csv = String::from("phase,op,n,p50_ms,p99_ms,wal_bytes_per_row\n");
    let mut pass = true;
    // (phase label, trigger attached). The closing baseline bounds drift: if
    // baseline-after ≈ baseline, the with-trigger delta is the trigger.
    for (phase, on) in [
        ("baseline", false),
        ("with-trigger", true),
        ("baseline-after", false),
    ] {
        if on {
            apply_trigger(&admin).await?;
        } else {
            drop_trigger(&admin).await?;
        }
        let outbox_before = outbox_counts(&admin).await?;
        // Warm the statements once outside the measured batches.
        let warm: String = app
            .query_one(&insert, &[&"warm", &0i32, &"1.00", &false, &None::<&str>])
            .await?
            .get(0);
        app.execute(&update, &[&warm, &Some("w")]).await?;
        app.execute(&delete, &[&warm]).await?;

        // insert: n fresh rows (ids kept for the update/delete batches).
        let mut ids: Vec<String> = Vec::with_capacity(n);
        let s_ins = op_batch(&admin, n, async |i| {
            let id: String = app
                .query_one(
                    &insert,
                    &[
                        &format!("sku-{phase}-{i}"),
                        &(i as i32),
                        &"12.50",
                        &(i % 2 == 0),
                        &None::<&str>,
                    ],
                )
                .await?
                .get(0);
            ids.push(id);
            Ok(())
        })
        .await?;
        // update: each of those rows once.
        let s_upd = op_batch(&admin, n, async |i| {
            app.execute(&update, &[&ids[i], &Some("touched")]).await?;
            Ok(())
        })
        .await?;
        // delete: each row once — the table returns to empty every phase.
        let s_del = op_batch(&admin, n, async |i| {
            app.execute(&delete, &[&ids[i]]).await?;
            Ok(())
        })
        .await?;

        let after = outbox_counts(&admin).await?;
        let wrote = (
            after.1 - outbox_before.1,
            after.2 - outbox_before.2,
            after.3 - outbox_before.3,
        );
        for (op, s) in [("insert", &s_ins), ("update", &s_upd), ("delete", &s_del)] {
            println!(
                "  {phase:<14} {op:<6}  p50 {:>7.3}ms  p99 {:>7.3}ms  wal/row {:>7.0}B",
                s.p50_ms, s.p99_ms, s.wal_per_row
            );
            csv.push_str(&format!(
                "{phase},{op},{n},{:.3},{:.3},{:.0}\n",
                s.p50_ms, s.p99_ms, s.wal_per_row
            ));
        }
        // Sanity: exactly one outbox row per written row (per event) with the
        // trigger, exactly zero without. The warm-up triplet fires too.
        let want = if on {
            (n as i64 + 1, n as i64 + 1, n as i64 + 1)
        } else {
            (0, 0, 0)
        };
        check(
            &mut pass,
            &format!("{phase}: outbox rows per event = {want:?} (got {wrote:?})"),
            wrote == want,
        );
    }
    emit_csv("c2-trigger", &csv, &args.out);
    Ok(pass)
}

// ---------------------------------------------------------------------------
// bulk: single-statement UPDATE amplification at 1k/10k/100k rows
// ---------------------------------------------------------------------------

async fn bulk_phase(
    app_url: &str,
    admin_url: &str,
    args: &OutboxBenchArgs,
) -> anyhow::Result<bool> {
    println!("\n## bulk (C2) — single-statement UPDATE of N registered rows, with/without trigger");
    let (admin, _ah) = connect_admin(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    let mut csv =
        String::from("rows,trigger,duration_ms,wal_bytes,wal_bytes_per_row,outbox_rows\n");
    let mut pass = true;
    for rows in [1_000i64, 10_000, 100_000] {
        let mut measured: Vec<(bool, f64, i64)> = Vec::new();
        for on in [false, true] {
            // Seed with the trigger OFF always (seeding is not the measured
            // statement), truncating both tables first; then attach the
            // trigger for the "on" leg.
            drop_trigger(&admin).await?;
            admin
                .batch_execute(&format!(
                    "TRUNCATE {SCHEMA}.\"items\"; TRUNCATE {SCHEMA}.outbox;"
                ))
                .await?;
            admin
                .execute(
                    &format!(
                        "INSERT INTO {SCHEMA}.\"items\" (tenant_id, sku, qty, price, flag) \
                         SELECT '{TENANT}', 'sku-' || g, g::int, 1.00, false \
                           FROM generate_series(1, $1::bigint) g"
                    ),
                    &[&rows],
                )
                .await?;
            if on {
                apply_trigger(&admin).await?;
            }
            normalize(&admin).await?;

            let wal0 = wal_lsn(&admin).await?;
            let t = Instant::now();
            let touched = app
                .execute("UPDATE \"items\" SET qty = qty + 1", &[])
                .await?;
            let dur_ms = t.elapsed().as_secs_f64() * 1e3;
            let wal = wal_since(&admin, &wal0).await?;
            let outbox: i64 = admin
                .query_one(&format!("SELECT count(*) FROM {SCHEMA}.outbox"), &[])
                .await?
                .get(0);

            println!(
                "  {rows:>7} rows  trigger {}  {dur_ms:>9.1}ms  wal {wal:>12}B ({:>5.0}B/row)  outbox {outbox}",
                if on { "on " } else { "off" },
                wal as f64 / rows as f64
            );
            csv.push_str(&format!(
                "{rows},{},{dur_ms:.1},{wal},{:.0},{outbox}\n",
                if on { "on" } else { "off" },
                wal as f64 / rows as f64
            ));
            // Sanity: the statement touched every row; the trigger wrote
            // exactly one outbox row per updated row (and zero when absent).
            check(
                &mut pass,
                &format!("bulk {rows} trigger={on}: touched {touched}, outbox {outbox}"),
                touched == rows as u64 && outbox == if on { rows } else { 0 },
            );
            measured.push((on, dur_ms, wal));
        }
        if let [(_, d_off, w_off), (_, d_on, w_on)] = measured[..] {
            println!(
                "  {rows:>7} rows  amplification: duration ×{:.2}, WAL ×{:.2}",
                d_on / d_off,
                w_on as f64 / w_off as f64
            );
        }
    }
    emit_csv("c2-bulk", &csv, &args.out);
    Ok(pass)
}

// ---------------------------------------------------------------------------
// growth: sustained row-event load vs prune cadence
// ---------------------------------------------------------------------------

async fn growth_phase(
    app_url: &str,
    admin_url: &str,
    args: &OutboxBenchArgs,
    cadences: &[u64],
) -> anyhow::Result<bool> {
    println!(
        "\n## growth (C2) — {}/s row-event INSERT load, prune cadence sweep {:?}s \
         (0 = off), retention {}ms, batch {}",
        args.growth_rate, cadences, args.retention_ms, args.prune_batch
    );
    let (admin, _ah) = connect_admin(admin_url).await?;
    let mut pass = true;

    for &cadence in cadences {
        let secs = cadence_duration(args.growth_secs, cadence);
        println!("\n### cadence {cadence}s — {secs}s of load");
        // Fresh slate per cadence: trigger ON (the realistic producer is the
        // entity write), empty tables, pending sentinels seeded.
        apply_trigger(&admin).await?;
        admin
            .batch_execute(&format!(
                "TRUNCATE {SCHEMA}.\"items\"; TRUNCATE {SCHEMA}.outbox;"
            ))
            .await?;
        admin
            .execute(
                &format!(
                    "INSERT INTO {SCHEMA}.outbox (tenant_id, table_name, event, payload) \
                     SELECT '{TENANT}', '{SENTINEL_TABLE}', 'insert', '{{}}'::jsonb \
                       FROM generate_series(1, $1::bigint)"
                ),
                &[&SENTINELS],
            )
            .await?;
        normalize(&admin).await?;

        let stop = Arc::new(AtomicBool::new(false));
        let pruned = Arc::new(AtomicU64::new(0));

        // Bloat probe (admin): outbox relation size / dead tuples / pending /
        // total every 5 s.
        let sampler = {
            let url = admin_url.to_string();
            let stop = stop.clone();
            tokio::spawn(async move {
                let (c, conn) = tokio_postgres::connect(&url, NoTls).await?;
                let _t = tokio::spawn(async move {
                    let _ = conn.await;
                });
                let probe = format!(
                    "SELECT pg_relation_size('{SCHEMA}.outbox'), \
                            COALESCE((SELECT n_dead_tup FROM pg_stat_all_tables \
                                       WHERE schemaname = '{SCHEMA}' AND relname = 'outbox'), 0)::bigint, \
                            (SELECT count(*) FROM {SCHEMA}.outbox WHERE dispatched_at IS NULL), \
                            (SELECT count(*) FROM {SCHEMA}.outbox)"
                );
                let start = Instant::now();
                let mut rows: Vec<(u64, i64, i64, i64, i64)> = Vec::new();
                loop {
                    let r = c.query_one(&probe, &[]).await?;
                    rows.push((
                        start.elapsed().as_secs(),
                        r.get(0),
                        r.get(1),
                        r.get(2),
                        r.get(3),
                    ));
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    for _ in 0..5 {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
                anyhow::Ok(rows)
            })
        };

        // Acker (app conn): the dispatcher stand-in — every second, stamp all
        // pending item events dispatched. Sentinels are deliberately excluded
        // so they stay pending for the never-pruned proof.
        let acker = {
            let url = app_url.to_string();
            let stop = stop.clone();
            tokio::spawn(async move {
                let (c, _h) = connect_app(&url).await?;
                let ack = c
                    .prepare(&format!(
                        "UPDATE outbox SET dispatched_at = now() \
                          WHERE dispatched_at IS NULL AND table_name <> '{SENTINEL_TABLE}'"
                    ))
                    .await?;
                while !stop.load(Ordering::Relaxed) {
                    c.execute(&ack, &[]).await?;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                anyhow::Ok(())
            })
        };

        // Pruner (app conn): every `cadence` seconds, loop prune batches until
        // one comes back short (the d8v saturated-batch drain semantics).
        let pruner = (cadence > 0).then(|| {
            let url = app_url.to_string();
            let stop = stop.clone();
            let pruned = pruned.clone();
            let batch = args.prune_batch;
            let retention = args.retention_ms;
            tokio::spawn(async move {
                let (c, _h) = connect_app(&url).await?;
                let prune = c.prepare(&outbox_prune_sql(batch)).await?;
                'outer: while !stop.load(Ordering::Relaxed) {
                    for _ in 0..cadence {
                        if stop.load(Ordering::Relaxed) {
                            break 'outer;
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    loop {
                        let n = c.execute(&prune, &[&retention]).await?;
                        pruned.fetch_add(n, Ordering::Relaxed);
                        if (n as usize) < batch {
                            break;
                        }
                    }
                }
                anyhow::Ok(())
            })
        });

        // Producer (app conn): open-loop entity INSERTs at the target rate
        // with catch-up pacing — each write fires the trigger, so this IS the
        // row-event load.
        let ins = {
            let (c, _h) = connect_app(app_url).await?;
            let insert = c
                .prepare(
                    "INSERT INTO \"items\" (tenant_id, sku, qty, price, flag) \
                     VALUES (current_setting('app.tenant', true), $1, $2, '1.00', false)",
                )
                .await?;
            let start = Instant::now();
            let mut sent: u64 = 0;
            while start.elapsed().as_secs() < secs {
                let due = (start.elapsed().as_secs_f64() * args.growth_rate) as u64 + 1;
                while sent < due {
                    c.execute(&insert, &[&format!("g-{sent}"), &(sent as i32)])
                        .await?;
                    sent += 1;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            sent as i64
        };

        stop.store(true, Ordering::Relaxed);
        acker.await??;
        if let Some(p) = pruner {
            p.await??;
        }
        let samples = sampler.await??;

        let mut csv = String::from("t_secs,rel_size_bytes,dead_tup,pending,total\n");
        for (t, rel, dead, pending, total) in &samples {
            csv.push_str(&format!("{t},{rel},{dead},{pending},{total}\n"));
        }
        emit_csv(&format!("c2-growth-c{cadence}"), &csv, &args.out);

        let pr = pruned.load(Ordering::Relaxed) as i64;
        let (sentinels_left, total): (i64, i64) = {
            let r = admin
                .query_one(
                    &format!(
                        "SELECT count(*) FILTER (WHERE table_name = '{SENTINEL_TABLE}'), count(*) \
                           FROM {SCHEMA}.outbox"
                    ),
                    &[],
                )
                .await?;
            (r.get(0), r.get(1))
        };
        let peak_rel = samples.iter().map(|s| s.1).max().unwrap_or(0);
        let final_rel = samples.last().map(|s| s.1).unwrap_or(0);
        println!(
            "  inserted {ins} | pruned {pr} | final outbox {total} | \
             rel_size peak {peak_rel}B final {final_rel}B"
        );

        // Sanity: GC never touches a pending row; a live cadence actually
        // pruned; cadence-off pruned nothing (total = inserted + sentinels).
        check(
            &mut pass,
            &format!(
                "cadence {cadence}: all {SENTINELS} pending sentinels survive (got {sentinels_left})"
            ),
            sentinels_left == SENTINELS,
        );
        if cadence == 0 {
            check(
                &mut pass,
                &format!(
                    "cadence 0: nothing pruned, outbox = inserted + sentinels ({total} vs {})",
                    ins + SENTINELS
                ),
                pr == 0 && total == ins + SENTINELS,
            );
        } else {
            check(
                &mut pass,
                &format!("cadence {cadence}: the pruner pruned (pruned {pr} > 0)"),
                pr > 0,
            );
        }
    }
    Ok(pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_list_parses_and_rejects_junk() {
        assert_eq!(parse_cadences("0,60,600").unwrap(), vec![0, 60, 600]);
        assert_eq!(parse_cadences(" 5 , 15 ").unwrap(), vec![5, 15]);
        assert!(parse_cadences("").is_err());
        assert!(parse_cadences("0,x").is_err());
    }

    #[test]
    fn cadence_duration_covers_two_prunes_of_the_slowest_sweep() {
        // Fast cadences run the configured floor; slow ones stretch so at
        // least two prune ticks land inside the load window.
        assert_eq!(cadence_duration(120, 0), 120);
        assert_eq!(cadence_duration(120, 60), 180);
        assert_eq!(cadence_duration(300, 600), 1260);
    }

    #[test]
    fn bench_catalog_parses_and_compiles_the_floor_and_trigger_plans() {
        let cat = catalog().expect("catalog parses");
        let floor = Migration::create(&cat)
            .unwrap()
            .sql(Confirmation::None)
            .unwrap();
        assert!(floor.contains("CREATE TABLE \"items\""));
        let plan = Migration::outbox_triggers(
            &cat,
            &OutboxOptions {
                schema: SCHEMA.into(),
            },
        )
        .unwrap()
        .sql(Confirmation::None)
        .unwrap();
        assert!(plan.contains(&format!("\"{SCHEMA}\".\"outbox\"")));
        assert!(plan.contains("CREATE OR REPLACE TRIGGER") || plan.contains("CREATE TRIGGER"));
    }
}
