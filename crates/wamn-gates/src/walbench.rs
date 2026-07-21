//! The `walbench` subcommand: the EVT-C-WAL-0 pre-CDC WAL-volume baseline
//! (docs/event-plane-jetstream.md §7/§8, docs/ceilings.md § C-WAL-0) — the
//! *denominator* every later C-CDC WAL-delta claim (wamn-l5i9.14) divides by.
//!
//! A MEASUREMENT campaign, not a regression gate: curves land in
//! `docs/ceilings.md` + `docs/ceilings-data/` (§11 provenance), and only the
//! sanity asserts gate — the run is genuinely PRE-CDC (no publication, no
//! replication slot, every table at DEFAULT replica identity), every op moved
//! WAL (the instrument self-check), and the op counts are exact. Pure
//! host-side (raw `tokio_postgres`, no wasm): WAL is a Postgres mechanism. It
//! provisions a fresh ephemeral schema (`wamn_walbench`) through the
//! superuser and applies the REAL 3.2 tenant floor (`Migration::create`) for
//! the poc-receiving catalog — the actual POC app model, so the baseline's
//! schema matches the `FOR TABLES IN SCHEMA app` publication l5i9.14 measures
//! the delta against (app-schema WAL only; the run-plane is context, not the
//! denominator).
//!
//! Modes:
//!   perop — per-op WAL bytes + p50/p99 across two row shapes × three ops:
//!           narrow (`suppliers`, a small no-FK row) and wide+TOASTy
//!           (`users`, a large incompressible `display_name` that TOASTs
//!           out-of-line — the width axis the C-CDC full-identity delta scales
//!           with) × insert / update / delete. VACUUM + CHECKPOINT before every
//!           measured batch (comparable full-page-image regime, matching C2/C7).
//!   mixed — representative receiving-event write mix (one transaction per
//!           event: a receipt + N receipt_lines, and a quality_hold +
//!           disposition on every 4th) at a small set of offered rates
//!           (catch-up pacing) → WAL bytes/s and bytes/event. This is the
//!           "representative app load" bytes/s baseline.
//!   all   — perop then mixed.
//!
//! Not `--mode all` of some other gate: run it explicitly via
//! deploy/gates/walbench-job.yaml. A single run is the record (unlike the C7 two-run
//! practice): there is no knee search a one-sided disk stall can poison — the
//! headline numbers are byte counts and medians, and a stall shows up visibly
//! as a p99 outlier. The insert position (`pg_current_wal_insert_lsn`), not the
//! flushed position, so the byte counts are exact even on the fixture pod's
//! `fsync=off`/`synchronous_commit=off` (the C2 instrument lesson).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};
use wamn_ddl::{Confirmation, Migration};
use wamn_gate_harness::{check, emit_csv, percentile};

const SCHEMA: &str = "wamn_walbench";
const TENANT: &str = "walbench-tenant";
/// The poc-receiving catalog (POC-DM1's promoted artifact) — the real POC app
/// model. `include_str!` bakes it into the binary at compile time (the builder
/// COPYs `deploy/` before `cargo build`), so no runtime file dependency.
const CATALOG_JSON: &str = include_str!("../../../deploy/poc/poc-material-receiving.catalog.json");

/// Reference (master) data seeded once for the mixed leg — the FK parents
/// every receiving event references.
const N_SITES: usize = 3;
const N_SUPPLIERS: usize = 5;
const N_MATERIALS: usize = 8;
const N_USERS: usize = 4;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Perop,
    Mixed,
    All,
}

#[derive(Debug, Args)]
pub struct WalBenchArgs {
    /// App (writer) Postgres URL — the NOSUPERUSER wamn_app role that writes
    /// under the production RLS floor. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema, VACUUM/CHECKPOINT,
    /// reads WAL LSNs + the pre-CDC provenance, and TRUNCATEs between rates.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which measurement to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// perop mode: single-row operations per op batch.
    #[arg(long, default_value_t = 1000)]
    pub iters: usize,

    /// perop mode: bytes of incompressible content in the wide `users`
    /// `display_name` (forces out-of-line TOAST — the width axis probe).
    #[arg(long, default_value_t = 6144)]
    pub wide_bytes: usize,

    /// mixed mode: offered receiving-event rates (events/sec), comma-separated.
    #[arg(long, default_value = "20,50")]
    pub mixed_rates: String,

    /// mixed mode: seconds of sustained load per rate.
    #[arg(long, default_value_t = 60)]
    pub mixed_secs: u64,

    /// mixed mode: receipt_lines per receiving event.
    #[arg(long, default_value_t = 3)]
    pub mixed_lines: usize,

    /// Also write each CSV to this directory (stdout always carries them
    /// between `=== BEGIN/END CSV <name> ===` markers).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// Parse the `--mixed-rates` list (events/sec).
fn parse_rates(s: &str) -> anyhow::Result<Vec<f64>> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .with_context(|| format!("bad --mixed-rates {s:?} (want e.g. \"20,50\")"))?;
    if v.is_empty() {
        bail!("--mixed-rates is empty");
    }
    Ok(v)
}

/// A deterministic, poorly-compressible `size`-byte string so a wide `users`
/// row genuinely TOASTs out-of-line (pglz cannot shrink high-entropy content
/// below the ~2 KB TOAST threshold). Not `rand` (`Math.random`/`Date` are
/// unavailable in the sandbox and we want a reproducible record) — an LCG
/// seeded by the row index, its high bits mapped onto a 64-char alphabet.
pub(crate) fn wide_blob(seed: usize, size: usize) -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut x = (seed as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x1234_5678_9ABC_DEF1);
    let mut s = String::with_capacity(size);
    for _ in 0..size {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        s.push(ALPHA[((x >> 33) as usize) & 63] as char);
    }
    s
}

fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("poc-receiving catalog parse: {e}"))
}

/// Drop-and-recreate the ephemeral schema and apply the REAL 3.2 floor for the
/// poc-receiving catalog (under `search_path` so the unqualified generated DDL
/// lands here). No publication, no slot, no trigger — this IS the pre-CDC env.
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

/// A long-lived admin connection (VACUUM/CHECKPOINT, WAL LSNs, TRUNCATE,
/// provenance probes). `search_path` pinned to the bench schema.
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

/// The instance WAL INSERT position (LSN as text). The insert position, not
/// `pg_current_wal_lsn()` (the flushed position): under
/// `synchronous_commit=off`/`fsync=off` (the in-cluster fixture pod) nothing
/// flushes inside a fast batch and the flushed position reads ~0 bytes moved —
/// the insert position measures WAL *generated*, the quantity here, regardless
/// of flush policy (the C2 instrument lesson).
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

/// VACUUM (ANALYZE) the given tables, then CHECKPOINT — before every measured
/// batch so each starts from the same regime: no dead-tuple backlog, and a
/// fresh checkpoint so first page touches pay a full-page image consistently
/// (the FPI share is a real production cost the app pages also pay; it cancels
/// in the C-CDC ratio). VACUUM can't run inside a txn block, so each is its own
/// simple-query round trip.
async fn normalize(admin: &Client, tables: &[&str]) -> anyhow::Result<()> {
    for t in tables {
        admin
            .batch_execute(&format!("VACUUM (ANALYZE) {SCHEMA}.\"{t}\""))
            .await?;
    }
    admin.batch_execute("CHECKPOINT").await?;
    Ok(())
}

async fn count(admin: &Client, table: &str) -> anyhow::Result<i64> {
    Ok(admin
        .query_one(&format!("SELECT count(*) FROM {SCHEMA}.\"{table}\""), &[])
        .await?
        .get(0))
}

/// Size of a table's out-of-line TOAST relation (0 if nothing TOASTed).
async fn toast_size(admin: &Client, table: &str) -> anyhow::Result<i64> {
    Ok(admin
        .query_one(
            &format!(
                "SELECT COALESCE(pg_relation_size(reltoastrelid), 0)::bigint \
                   FROM pg_class \
                  WHERE relnamespace = '{SCHEMA}'::regnamespace AND relname = '{table}'"
            ),
            &[],
        )
        .await?
        .get(0))
}

pub async fn run(args: WalBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("walbench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL")?;
    let rates = parse_rates(&args.mixed_rates)?;

    println!(
        "# wamn-gates EVT-C-WAL-0 walbench (schema {SCHEMA}, tenant {TENANT}) — pre-CDC baseline WAL volume"
    );
    provision(&admin_url)
        .await
        .context("provision ephemeral schema")?;

    let mut pass = true;
    let outcome = async {
        // Pre-CDC provenance + assert (measured DB, tables freshly created).
        {
            let (admin, _ah) = connect_admin(&admin_url).await?;
            precheck(&admin, &mut pass).await?;
        }
        let run_all = args.mode == Mode::All;
        if run_all || args.mode == Mode::Perop {
            pass &= perop_phase(&app_url, &admin_url, &args).await?;
        }
        if run_all || args.mode == Mode::Mixed {
            pass &= mixed_phase(&app_url, &admin_url, &args, &rates).await?;
        }
        anyhow::Ok(())
    }
    .await;

    let _ = teardown(&admin_url).await;
    outcome?;

    println!("\nwalbench complete — overall PASS: {pass}");
    if !pass {
        bail!("an EVT-C-WAL-0 sanity assert failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// pre-CDC provenance: the run's denominator claim, made checkable
// ---------------------------------------------------------------------------

/// Record the WAL level + prove the DB is genuinely pre-CDC: no publication, no
/// replication slot, every measured table at DEFAULT replica identity (`d`).
/// This is what makes "the pre-CDC denominator" a checkable property rather
/// than an assumption — a stray leftover slot on the fixture pod would fail it.
async fn precheck(admin: &Client, pass: &mut bool) -> anyhow::Result<()> {
    let wal_level: String = admin.query_one("SHOW wal_level", &[]).await?.get(0);
    let pubs: i64 = admin
        .query_one("SELECT count(*) FROM pg_publication", &[])
        .await?
        .get(0);
    let slots: i64 = admin
        .query_one("SELECT count(*) FROM pg_replication_slots", &[])
        .await?
        .get(0);
    let idents = admin
        .query(
            &format!(
                "SELECT relname::text, relreplident::text \
                   FROM pg_class \
                  WHERE relnamespace = '{SCHEMA}'::regnamespace AND relkind = 'r' \
                  ORDER BY relname"
            ),
            &[],
        )
        .await?;
    let tables: Vec<String> = idents.iter().map(|r| r.get::<_, String>(0)).collect();
    let all_default = idents.iter().all(|r| r.get::<_, String>(1) == "d");

    println!("\n## pre-CDC provenance");
    println!("  wal_level = {wal_level}");
    println!("  publications = {pubs}, replication slots = {slots}");
    println!(
        "  measured tables = {} ({}), all DEFAULT replica identity: {all_default}",
        tables.len(),
        tables.join(", ")
    );
    check(
        pass,
        &format!(
            "pre-CDC: no publication and no replication slot on the measured DB (pubs {pubs}, slots {slots})"
        ),
        pubs == 0 && slots == 0,
    );
    check(
        pass,
        &format!(
            "pre-CDC: every measured table carries the DEFAULT replica identity ({} tables)",
            tables.len()
        ),
        all_default && !tables.is_empty(),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// perop: per-op WAL/op + p50/p99 across narrow (suppliers) and wide (users)
// ---------------------------------------------------------------------------

struct OpStats {
    p50_ms: f64,
    p99_ms: f64,
    wal_per_op: f64,
}

/// One op batch on the app connection: `n` statements (prepared once — the
/// plugin's `prepare_cached` wire shape), timing each, with WAL bracketed on
/// the admin connection after a normalize of `tables`.
async fn op_batch(
    admin: &Client,
    tables: &[&str],
    n: usize,
    mut op: impl AsyncFnMut(usize) -> anyhow::Result<()>,
) -> anyhow::Result<OpStats> {
    normalize(admin, tables).await?;
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
        wal_per_op: wal as f64 / n as f64,
    })
}

fn push_row(csv: &mut String, shape: &str, op: &str, n: usize, s: &OpStats) {
    println!(
        "  {shape:<6} {op:<6}  p50 {:>7.3}ms  p99 {:>7.3}ms  wal/op {:>8.0}B",
        s.p50_ms, s.p99_ms, s.wal_per_op
    );
    csv.push_str(&format!(
        "{shape},{op},{n},{:.3},{:.3},{:.0}\n",
        s.p50_ms, s.p99_ms, s.wal_per_op
    ));
}

async fn perop_phase(app_url: &str, admin_url: &str, args: &WalBenchArgs) -> anyhow::Result<bool> {
    let n = args.iters;
    let w = args.wide_bytes;
    println!(
        "\n## per-op (C-WAL-0) — WAL bytes/op + p50/p99, {n} ops/batch, \
         narrow (suppliers) + wide/TOASTy (users, {w}B display_name), default replica identity"
    );
    let (admin, _ah) = connect_admin(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    // narrow: suppliers — a small row, no FK, no large columns.
    let s_ins = app
        .prepare(
            "INSERT INTO \"suppliers\" (tenant_id, name, contact_email, standard_cost) \
             VALUES (current_setting('app.tenant', true), $1, $2, $3::text::numeric) \
             RETURNING id::text",
        )
        .await?;
    let s_upd = app
        .prepare("UPDATE \"suppliers\" SET contact_email = $2 WHERE id = $1::text::uuid")
        .await?;
    let s_del = app
        .prepare("DELETE FROM \"suppliers\" WHERE id = $1::text::uuid")
        .await?;
    // wide: users — a large, incompressible `display_name` that TOASTs.
    let u_ins = app
        .prepare(
            "INSERT INTO \"users\" (tenant_id, email, display_name, cert_level) \
             VALUES (current_setting('app.tenant', true), $1, $2, $3) \
             RETURNING id::text",
        )
        .await?;
    let u_upd = app
        .prepare("UPDATE \"users\" SET display_name = $2 WHERE id = $1::text::uuid")
        .await?;
    let u_del = app
        .prepare("DELETE FROM \"users\" WHERE id = $1::text::uuid")
        .await?;

    let mut csv = String::from("shape,op,n,p50_ms,p99_ms,wal_bytes_per_op\n");
    let mut pass = true;

    // ---- narrow (suppliers) ----
    {
        // Warm the plan cache once, outside the measured batches (net 0 rows).
        let warm: String = app
            .query_one(&s_ins, &[&"warm", &None::<&str>, &"1.00"])
            .await?
            .get(0);
        app.execute(&s_upd, &[&warm, &Some("w")]).await?;
        app.execute(&s_del, &[&warm]).await?;
    }
    let mut ids: Vec<String> = Vec::with_capacity(n);
    let n_ins = op_batch(&admin, &["suppliers"], n, async |i| {
        let id: String = app
            .query_one(
                &s_ins,
                &[
                    &format!("sup-{i}"),
                    &Some(format!("s{i}@example.test")),
                    &"12.50",
                ],
            )
            .await?
            .get(0);
        ids.push(id);
        Ok(())
    })
    .await?;
    check(
        &mut pass,
        &format!(
            "narrow: inserted exactly {n} supplier rows (got {})",
            count(&admin, "suppliers").await?
        ),
        count(&admin, "suppliers").await? == n as i64,
    );
    let n_upd = op_batch(&admin, &["suppliers"], n, async |i| {
        app.execute(&s_upd, &[&ids[i], &Some("touched")]).await?;
        Ok(())
    })
    .await?;
    let n_del = op_batch(&admin, &["suppliers"], n, async |i| {
        app.execute(&s_del, &[&ids[i]]).await?;
        Ok(())
    })
    .await?;
    check(
        &mut pass,
        &format!(
            "narrow: deleted all rows, table empty (got {})",
            count(&admin, "suppliers").await?
        ),
        count(&admin, "suppliers").await? == 0,
    );
    for (op, s) in [("insert", &n_ins), ("update", &n_upd), ("delete", &n_del)] {
        push_row(&mut csv, "narrow", op, n, s);
        check(
            &mut pass,
            &format!(
                "narrow {op}: WAL moved (> 24 B/op, got {:.0} B)",
                s.wal_per_op
            ),
            s.wal_per_op > 24.0,
        );
    }

    // ---- wide/TOASTy (users) ----
    {
        let warm: String = app
            .query_one(&u_ins, &[&"warm@example.test", &Some("w"), &None::<&str>])
            .await?
            .get(0);
        app.execute(&u_upd, &[&warm, &Some("w2")]).await?;
        app.execute(&u_del, &[&warm]).await?;
    }
    let mut wids: Vec<String> = Vec::with_capacity(n);
    let w_ins = op_batch(&admin, &["users"], n, async |i| {
        let cert = if i.is_multiple_of(2) {
            Some("L1")
        } else {
            Some("L2")
        };
        let id: String = app
            .query_one(
                &u_ins,
                &[&format!("u{i}@example.test"), &Some(wide_blob(i, w)), &cert],
            )
            .await?
            .get(0);
        wids.push(id);
        Ok(())
    })
    .await?;
    check(
        &mut pass,
        &format!(
            "wide: inserted exactly {n} user rows (got {})",
            count(&admin, "users").await?
        ),
        count(&admin, "users").await? == n as i64,
    );
    // The wide leg's whole point: the rows genuinely TOASTed out-of-line.
    let ts = toast_size(&admin, "users").await?;
    check(
        &mut pass,
        &format!("wide: users TOAST relation is non-empty — the wide rows TOASTed ({ts} B)"),
        ts > 0,
    );
    let w_upd = op_batch(&admin, &["users"], n, async |i| {
        app.execute(&u_upd, &[&wids[i], &Some(wide_blob(i + n, w))])
            .await?;
        Ok(())
    })
    .await?;
    let w_del = op_batch(&admin, &["users"], n, async |i| {
        app.execute(&u_del, &[&wids[i]]).await?;
        Ok(())
    })
    .await?;
    check(
        &mut pass,
        &format!(
            "wide: deleted all rows, table empty (got {})",
            count(&admin, "users").await?
        ),
        count(&admin, "users").await? == 0,
    );
    for (op, s) in [("insert", &w_ins), ("update", &w_upd), ("delete", &w_del)] {
        push_row(&mut csv, "wide", op, n, s);
        check(
            &mut pass,
            &format!(
                "wide {op}: WAL moved (> 24 B/op, got {:.0} B)",
                s.wal_per_op
            ),
            s.wal_per_op > 24.0,
        );
    }

    emit_csv("cwal0-perop", &csv, &args.out);
    Ok(pass)
}

// ---------------------------------------------------------------------------
// mixed: representative receiving-event write mix → WAL bytes/s
// ---------------------------------------------------------------------------

/// The FK parents (master data) every receiving event references.
struct Reference {
    sites: Vec<String>,
    suppliers: Vec<String>,
    materials: Vec<String>,
    users: Vec<String>,
}

/// Seed the master data once, under wamn_app + the tenant claim (server-side
/// `tenant_id`). Outside every measured window, so this WAL is never counted.
async fn seed_reference(app: &Client) -> anyhow::Result<Reference> {
    let ins_site = app
        .prepare(
            "INSERT INTO \"sites\" (tenant_id, name, code) \
             VALUES (current_setting('app.tenant', true), $1, $2) RETURNING id::text",
        )
        .await?;
    let mut sites = Vec::new();
    for i in 0..N_SITES {
        sites.push(
            app.query_one(&ins_site, &[&format!("Site {i}"), &format!("SITE{i}")])
                .await?
                .get::<_, String>(0),
        );
    }
    let ins_sup = app
        .prepare(
            "INSERT INTO \"suppliers\" (tenant_id, name, contact_email, standard_cost) \
             VALUES (current_setting('app.tenant', true), $1, $2, $3::text::numeric) RETURNING id::text",
        )
        .await?;
    let mut suppliers = Vec::new();
    for i in 0..N_SUPPLIERS {
        suppliers.push(
            app.query_one(
                &ins_sup,
                &[
                    &format!("Supplier {i}"),
                    &Some(format!("sup{i}@example.test")),
                    &"100.00",
                ],
            )
            .await?
            .get::<_, String>(0),
        );
    }
    let ins_mat = app
        .prepare(
            "INSERT INTO \"materials\" (tenant_id, name, moisture_max_pct, weight_tolerance_kg) \
             VALUES (current_setting('app.tenant', true), $1, $2::text::numeric, $3::text::numeric) RETURNING id::text",
        )
        .await?;
    let mut materials = Vec::new();
    for i in 0..N_MATERIALS {
        materials.push(
            app.query_one(&ins_mat, &[&format!("Material {i}"), &"12.50", &"0.050"])
                .await?
                .get::<_, String>(0),
        );
    }
    let ins_user = app
        .prepare(
            "INSERT INTO \"users\" (tenant_id, email, display_name, cert_level) \
             VALUES (current_setting('app.tenant', true), $1, $2, $3) RETURNING id::text",
        )
        .await?;
    let mut users = Vec::new();
    for i in 0..N_USERS {
        users.push(
            app.query_one(
                &ins_user,
                &[
                    &format!("insp{i}@example.test"),
                    &Some(format!("Inspector {i}")),
                    &Some("L1"),
                ],
            )
            .await?
            .get::<_, String>(0),
        );
    }
    Ok(Reference {
        sites,
        suppliers,
        materials,
        users,
    })
}

/// The prepared statements of one receiving event.
struct EventStmts {
    receipt: tokio_postgres::Statement,
    line: tokio_postgres::Statement,
    hold: tokio_postgres::Statement,
    disposition: tokio_postgres::Statement,
}

impl EventStmts {
    async fn prepare(app: &Client) -> anyhow::Result<Self> {
        Ok(Self {
            receipt: app
                .prepare(
                    "INSERT INTO \"receipts\" (tenant_id, receipt_no, supplier_id, site_id, received_at) \
                     VALUES (current_setting('app.tenant', true), $1, $2::text::uuid, $3::text::uuid, now()) \
                     RETURNING id::text",
                )
                .await?,
            line: app
                .prepare(
                    "INSERT INTO \"receipt_lines\" (tenant_id, receipt_id, material_id, quantity) \
                     VALUES (current_setting('app.tenant', true), $1::text::uuid, $2::text::uuid, $3::text::numeric) \
                     RETURNING id::text",
                )
                .await?,
            hold: app
                .prepare(
                    "INSERT INTO \"quality_holds\" (tenant_id, line_id, site_id, status, opened_at) \
                     VALUES (current_setting('app.tenant', true), $1::text::uuid, $2::text::uuid, 'open', now()) \
                     RETURNING id::text",
                )
                .await?,
            disposition: app
                .prepare(
                    "INSERT INTO \"dispositions\" (tenant_id, hold_id, inspector_id, decision, decided_at) \
                     VALUES (current_setting('app.tenant', true), $1::text::uuid, $2::text::uuid, 'accept', now())",
                )
                .await?,
        })
    }
}

/// One receiving event = one transaction: a receipt + `lines` receipt_lines,
/// and on every 4th event a quality_hold + disposition (the workflow tables;
/// most receipts pass, a minority get held).
async fn receiving_event(
    app: &Client,
    stmts: &EventStmts,
    r: &Reference,
    seq: u64,
    lines: usize,
) -> anyhow::Result<()> {
    let supplier = &r.suppliers[(seq as usize) % r.suppliers.len()];
    let site = &r.sites[(seq as usize) % r.sites.len()];
    app.batch_execute("BEGIN").await?;
    let receipt: String = app
        .query_one(&stmts.receipt, &[&format!("R-{seq}"), supplier, site])
        .await?
        .get(0);
    let mut first_line: Option<String> = None;
    for k in 0..lines {
        let material = &r.materials[(seq as usize + k) % r.materials.len()];
        let line: String = app
            .query_one(&stmts.line, &[&receipt, material, &"1.500"])
            .await?
            .get(0);
        first_line.get_or_insert(line);
    }
    if seq.is_multiple_of(4)
        && let Some(line) = &first_line
    {
        let hold: String = app.query_one(&stmts.hold, &[line, site]).await?.get(0);
        let inspector = &r.users[(seq as usize) % r.users.len()];
        app.execute(&stmts.disposition, &[&hold, inspector]).await?;
    }
    app.batch_execute("COMMIT").await?;
    Ok(())
}

async fn mixed_phase(
    app_url: &str,
    admin_url: &str,
    args: &WalBenchArgs,
    rates: &[f64],
) -> anyhow::Result<bool> {
    println!(
        "\n## mixed-load (C-WAL-0) — representative receiving-event write mix, WAL bytes/s at rates {rates:?}/s, \
         {} lines/event, hold+disposition every 4th",
        args.mixed_lines
    );
    let (admin, _ah) = connect_admin(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;
    let reference = seed_reference(&app).await?;
    let stmts = EventStmts::prepare(&app).await?;

    // The transactional tables the event writes (the reference tables are only
    // read via FK checks, generating no WAL in the window).
    let tables = ["receipts", "receipt_lines", "quality_holds", "dispositions"];
    let mut csv = String::from(
        "rate_target,events,seconds,our_wal_bytes,wal_per_event_mean,wal_per_event_p50,wal_bytes_per_sec,events_per_sec\n",
    );
    let mut pass = true;
    for &rate in rates {
        // Reset to a consistent regime per rate: TRUNCATE the transactional
        // tables (admin — TRUNCATE isn't in the wamn_app grant and RLS doesn't
        // gate it; CASCADE handles the FK order), then VACUUM + CHECKPOINT.
        admin
            .batch_execute(&format!(
                "TRUNCATE {SCHEMA}.\"receipts\", {SCHEMA}.\"receipt_lines\", \
                 {SCHEMA}.\"quality_holds\", {SCHEMA}.\"dispositions\" CASCADE"
            ))
            .await?;
        normalize(&admin, &tables).await?;

        // Bracket WAL PER EVENT, not over the whole window: the insert LSN is
        // instance-global, so a window-long bracket on the *shared* fixture pod
        // would fold in other tenants' WAL (an early run showed one window at
        // ~5× another). A per-event bracket is a sub-ms window; summing them
        // excludes the idle gaps where other tenants write, so the total is OUR
        // load's WAL — robust to cross-talk, the same reason the per-op batches
        // (also short brackets) stayed clean.
        let mut per_event: Vec<i64> = Vec::new();
        let start = Instant::now();
        let mut sent: u64 = 0;
        while start.elapsed().as_secs_f64() < args.mixed_secs as f64 {
            let due = (start.elapsed().as_secs_f64() * rate) as u64 + 1;
            while sent < due {
                let w0 = wal_lsn(&admin).await?;
                receiving_event(&app, &stmts, &reference, sent, args.mixed_lines).await?;
                per_event.push(wal_since(&admin, &w0).await?);
                sent += 1;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let elapsed = start.elapsed().as_secs_f64();
        let total: i64 = per_event.iter().sum();
        let mean = total as f64 / sent.max(1) as f64;
        let p50 = {
            let mut v = per_event.clone();
            v.sort_unstable();
            v.get(v.len() / 2).copied().unwrap_or(0)
        };
        let per_sec = total as f64 / elapsed;
        let ev_per_sec = sent as f64 / elapsed;
        println!(
            "  rate {rate:>5.0}/s  events {sent:>6}  {elapsed:>5.1}s  our-wal {total:>11}B  \
             mean {mean:>6.0}B/event  p50 {p50:>5}B  {per_sec:>10.0}B/s  {ev_per_sec:>6.1} ev/s"
        );
        csv.push_str(&format!(
            "{rate:.0},{sent},{elapsed:.1},{total},{mean:.0},{p50},{per_sec:.0},{ev_per_sec:.1}\n"
        ));
        check(
            &mut pass,
            &format!(
                "mixed rate {rate}: produced events and WAL moved ({sent} events, mean {mean:.0} B/event > 100)"
            ),
            sent > 0 && mean > 100.0,
        );
    }
    emit_csv("cwal0-mixed", &csv, &args.out);
    Ok(pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_parse_and_reject_junk() {
        assert_eq!(parse_rates("20,50").unwrap(), vec![20.0, 50.0]);
        assert_eq!(parse_rates(" 10 ").unwrap(), vec![10.0]);
        assert!(parse_rates("").is_err());
        assert!(parse_rates("20,x").is_err());
    }

    #[test]
    fn wide_blob_is_deterministic_sized_and_high_entropy() {
        let a = wide_blob(1, 6144);
        assert_eq!(a.len(), 6144);
        assert_eq!(a, wide_blob(1, 6144), "same seed → same content");
        assert_ne!(a, wide_blob(2, 6144), "different seed → different content");
        // High-entropy: a run-length-friendly value would compress; here the
        // 64-char alphabet is well spread (no single char dominates).
        let max_share = {
            let mut counts = [0usize; 128];
            for b in a.bytes() {
                counts[b as usize] += 1;
            }
            *counts.iter().max().unwrap() as f64 / a.len() as f64
        };
        assert!(
            max_share < 0.05,
            "no byte should dominate (got {max_share})"
        );
    }

    #[test]
    fn poc_catalog_parses_and_compiles_the_floor() {
        let cat = catalog().expect("poc-receiving catalog parses");
        let floor = Migration::create(&cat)
            .unwrap()
            .sql(Confirmation::None)
            .unwrap();
        // The representative app tables the baseline writes.
        for t in [
            "suppliers",
            "users",
            "receipts",
            "receipt_lines",
            "quality_holds",
            "dispositions",
        ] {
            assert!(
                floor.contains(&format!("CREATE TABLE \"{t}\"")),
                "floor creates {t}"
            );
        }
        // No outbox / trigger in this floor — pre-CDC, app tables only.
        assert!(
            !floor.to_lowercase().contains("trigger"),
            "no trigger in the pre-CDC floor"
        );
    }
}
