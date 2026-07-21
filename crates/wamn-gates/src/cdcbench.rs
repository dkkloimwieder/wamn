//! The `cdcbench` subcommand: the [EVT-C-CDC] ceiling campaign (wamn-l5i9.14,
//! docs/event-plane-jetstream.md §7/§8, docs/ceilings.md § C-CDC).
//!
//! A MEASUREMENT campaign, not a regression gate (§8: curves and knees, no
//! pass/fail — only sanity/completeness asserts gate). Four axes:
//!
//!   drain      — decode drain rate after a bulk import: the slot exists, the
//!                bulk import lands with the reader DOWN, then the REAL reader
//!                (`wamn_cdc_reader::run_with_token`) starts and the gate
//!                samples stream depth + slot lag until every row event is on
//!                the `EVT_` stream. Serial decode per project-env is the
//!                capture ceiling (§11) — this measures it. Variants: batched
//!                narrow txns, ONE giant narrow txn, and ONE giant wide/TOASTy
//!                txn whose reorder buffer crosses the default 64 MB
//!                `logical_decoding_work_mem` — `pg_stat_replication_slots`
//!                spill counters are the wamn-mu4h evidence.
//!   lag        — slot-lag knee vs sustained write rate: reader live, offered
//!                single-row-txn rate step-ramped across several writer
//!                connections; slot lag (`confirmed_flush_lsn` vs the insert
//!                LSN) sampled through every step. The knee is where end-of-
//!                step lag stops returning toward the floor (§8: lag
//!                divergence). Eventual completeness is asserted.
//!   ri         — WAL delta under REPLICA IDENTITY FULL per table class
//!                (supersedes wamn-32d): walbench's per-op WAL bracketing
//!                (`pg_current_wal_insert_lsn`, VACUUM+CHECKPOINT normalize)
//!                over narrow (`suppliers`) and wide/TOASTy (`users`) shapes,
//!                measured at DEFAULT then FULL — flipped by the REAL
//!                l5i9.31/l5i9.61 reconcile driven by seeded delete
//!                registrations, not a hand ALTER. Includes the wide
//!                non-TOAST-column update (the l5i9.63 probe: FULL flattens
//!                the unchanged 6 KiB old image into WAL). C-WAL-0 (DEFAULT @
//!                `wal_level=replica`) is the historical denominator; the
//!                in-run DEFAULT leg isolates the `wal_level=logical` tax.
//!   switchover — the CNPG availability drill, TIMED: reconnecting writer +
//!                the REAL reader (its R11 re-open ladder is the recovery
//!                path) across an operator-triggered promotion/primary
//!                restart; write blackout, publish gap, catch-up time, and
//!                the cdc1 no-gap check (every committed row on the stream
//!                exactly once) from commit wall-times + JetStream ingest
//!                timestamps. TOPOLOGY: live wamn-pg is single-instance, so
//!                the live drill is a timed primary recreate; the F2 spike's
//!                multi-instance graceful switchover is the reference.
//!   all        — drain, lag, ri (NOT switchover: it needs an external
//!                trigger and usually a different target cluster).
//!
//! Substrate = the rie2ebench pattern: a gate-owned throwaway DATABASE
//! (`wamn_ccdc`) on a `wal_level=logical` Postgres — created and dropped WITH
//! (FORCE), the REAL deploy/sql DDL via `include_str!`, the REAL
//! wamn-provision/wamn-registry builders, the slot created LAST (provisioning
//! and seed writes stay uncaptured), and zero residue: teardown ALWAYS runs on
//! FRESH connections (a switchover kills the provisioning-time connections),
//! drops the slot, the database (which takes any idle slot with it), the
//! replication role, and the `EVT_` stream.
//!
//! Needs: `--admin-database-url` (SUPERUSER, path `/postgres`, on a
//! `wal_level=logical` PG) + `--nats-url` (JetStream). Recipe:
//! docs/build-and-test.md [EVT-C-CDC].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy};
use clap::{Args, ValueEnum};
use futures_util::StreamExt as _;
use pg_walstream::CancellationToken;
use tokio_postgres::{Client, NoTls};

use wamn_cdc_reader::{EventReaderArgs, run_with_token};
use wamn_ddl::{Confirmation, Migration};
use wamn_gate_harness::{check, emit_csv, percentile};
use wamn_provision::{cdc_object_name, event_stream_name, sql as provision_sql};
use wamn_registry::sql::{
    upsert_event_reader_sql, upsert_org_sql, upsert_project_env_sql, upsert_project_sql,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Drain,
    Lag,
    Ri,
    Switchover,
    All,
}

#[derive(Debug, Args)]
pub struct CdcBenchArgs {
    /// SUPERUSER URL (path `/postgres`) on a `wal_level=logical` Postgres —
    /// the gate owns the throwaway `wamn_ccdc` database, slot, and role.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// JetStream-enabled NATS the reader publishes the `EVT_` stream to.
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    pub nats_url: String,

    /// Which axis to run (`all` = drain, lag, ri — switchover is explicit).
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,

    /// drain: narrow rows per import (batched + single-txn variants).
    #[arg(long, default_value_t = 50_000)]
    pub drain_rows: usize,

    /// drain: rows per transaction in the batched variant.
    #[arg(long, default_value_t = 250)]
    pub drain_txn_rows: usize,

    /// drain: wide rows in the single-txn wide variant (12k × 6 KiB ≈ 72 MB
    /// of reorder-buffer content — past the default 64 MB
    /// `logical_decoding_work_mem`, the spill probe).
    #[arg(long, default_value_t = 12_000)]
    pub drain_wide_rows: usize,

    /// Bytes of incompressible content in a wide `users.display_name`
    /// (out-of-line TOAST — matches the C-WAL-0 wide leg).
    #[arg(long, default_value_t = 6144)]
    pub wide_bytes: usize,

    /// drain: seconds to wait for one variant's catch-up before failing.
    #[arg(long, default_value_t = 600)]
    pub drain_deadline_secs: u64,

    /// lag: offered write rates (single-row txns/sec), comma-separated steps.
    #[arg(long, default_value = "100,200,400,800,1600,3200")]
    pub lag_rates: String,

    /// lag: seconds per step.
    #[arg(long, default_value_t = 20)]
    pub lag_step_secs: u64,

    /// lag: concurrent writer connections the offered rate is split across.
    #[arg(long, default_value_t = 4)]
    pub lag_writers: usize,

    /// ri: single-row operations per measured batch.
    #[arg(long, default_value_t = 1000)]
    pub ri_iters: usize,

    /// switchover: drill window seconds (trigger the promotion/restart inside it).
    #[arg(long, default_value_t = 90)]
    pub secs: u64,

    /// switchover: milliseconds between writer rows.
    #[arg(long, default_value_t = 200)]
    pub write_interval_ms: u64,

    /// Also write each CSV to this directory (stdout always carries them
    /// between `=== BEGIN/END CSV <name> ===` markers).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

const DB: &str = "wamn_ccdc";
const ORG: &str = "ccdc";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const TENANT: &str = "ccdc-tenant";
const CDC_PW: &str = "wamn_cdc_pw";
const CATALOG_ID: &str = "poc-material-receiving";

// The REAL shipped DDL + the real POC app model, compiled in (drift-proof).
const SYSTEM_SQL: &str = include_str!("../../../deploy/sql/system-schema.sql");
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const CATALOG_JSON: &str = include_str!("../../../deploy/poc/poc-material-receiving.catalog.json");

fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("poc-receiving catalog parse: {e}"))
}

/// A delete-only registration on `entity` — exactly what drives the l5i9.31
/// reconcile to REPLICA IDENTITY FULL for that entity's table (the ri axis
/// flips through the REAL machinery, not a hand ALTER).
fn registration_json(reg_id: &str, entity: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "registration-id": reg_id,
        "catalog-id": CATALOG_ID,
        "flow-id": "ccdc-flow",
        "entity": entity,
        "ops": ["delete"],
        "condition": null,
        "partition-key": null,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Small helpers (the rie2ebench / walbench idioms)
// ---------------------------------------------------------------------------

/// Swap the database path segment of a libpq URL (the gate controls the URL —
/// no query string).
fn swap_db(url: &str, db: &str) -> String {
    let (base, _) = url.rsplit_once('/').expect("url has a path");
    format!("{base}/{db}")
}

/// `host:port` with credentials swapped out → a role's plain URL into DB.
fn role_url(super_url: &str, role: &str, pw: &str) -> String {
    let after_scheme = super_url.strip_prefix("postgres://").expect("postgres://");
    let (_, host_and_path) = after_scheme.rsplit_once('@').expect("url has userinfo");
    let (host_port, _) = host_and_path.split_once('/').expect("url has a path");
    format!("postgres://{role}:{pw}@{host_port}/{DB}")
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("connect {url}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(client)
}

/// A wamn_app writer pinned to the app schema + tenant claim (the RLS floor
/// the production write path runs under — the C-WAL-0 discipline).
async fn connect_app(admin_url: &str) -> anyhow::Result<Client> {
    let client = connect(&role_url(admin_url, "wamn_app", "wamn_app")).await?;
    client
        .batch_execute(&format!(
            "SET search_path TO app; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("set search_path + tenant claim")?;
    Ok(client)
}

/// The instance WAL INSERT position — WAL *generated*, exact under
/// `fsync=off`/`synchronous_commit=off` (the C2/C-WAL-0 instrument lesson).
async fn wal_lsn(admin: &Client) -> anyhow::Result<String> {
    Ok(admin
        .query_one("SELECT pg_current_wal_insert_lsn()::text", &[])
        .await?
        .get(0))
}

async fn wal_since(admin: &Client, before: &str) -> anyhow::Result<i64> {
    Ok(admin
        .query_one(
            "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(), $1::text::pg_lsn)::bigint",
            &[&before],
        )
        .await?
        .get(0))
}

/// The slot's decode backlog: insert LSN minus the confirmed flush position
/// (-1 when the slot is absent). Instance-global — on a shared pod ambient WAL
/// folds in (recorded in provenance; a per-event bracket cannot express a
/// *slot* position, which is the phenomenon here).
async fn slot_lag(db: &Client, slot: &str) -> anyhow::Result<i64> {
    Ok(db
        .query_opt(
            "SELECT COALESCE(pg_wal_lsn_diff(pg_current_wal_insert_lsn(), confirmed_flush_lsn), -1)::bigint \
             FROM pg_replication_slots WHERE slot_name = $1",
            &[&slot],
        )
        .await?
        .map(|r| r.get(0))
        .unwrap_or(-1))
}

/// `pg_stat_replication_slots` spill/stream/total counters — the reorder-buffer
/// evidence (`logical_decoding_work_mem`, wamn-mu4h). Zeroed per variant by the
/// fresh slot (stats live and die with the slot).
#[derive(Debug, Default, Clone, Copy)]
struct SlotStats {
    spill_txns: i64,
    spill_count: i64,
    spill_bytes: i64,
    stream_txns: i64,
    stream_bytes: i64,
    total_txns: i64,
    total_bytes: i64,
}

async fn slot_stats(db: &Client, slot: &str) -> anyhow::Result<SlotStats> {
    let row = db
        .query_opt(
            "SELECT spill_txns, spill_count, spill_bytes, stream_txns, stream_bytes, \
                    total_txns, total_bytes \
             FROM pg_stat_replication_slots WHERE slot_name = $1",
            &[&slot],
        )
        .await
        .context("read pg_stat_replication_slots")?;
    Ok(row
        .map(|r| SlotStats {
            spill_txns: r.get(0),
            spill_count: r.get(1),
            spill_bytes: r.get(2),
            stream_txns: r.get(3),
            stream_bytes: r.get(4),
            total_txns: r.get(5),
            total_bytes: r.get(6),
        })
        .unwrap_or_default())
}

/// Terminate + drop the slot, retrying briefly (a just-cancelled reader's
/// walsender may linger a beat). Best-effort — DROP DATABASE WITH (FORCE)
/// takes any idle slot with it as the backstop.
async fn drop_slot(db: &Client, slot: &str) {
    for _ in 0..6 {
        let _ = db
            .execute(
                "SELECT pg_terminate_backend(active_pid) FROM pg_replication_slots \
                 WHERE slot_name = $1 AND active",
                &[&slot],
            )
            .await;
        let gone = db
            .execute(
                "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots \
                 WHERE slot_name = $1",
                &[&slot],
            )
            .await
            .is_ok();
        if gone {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Server-side stream depth (0 while the reader hasn't created it yet).
async fn stream_msgs(js: &async_nats::jetstream::Context, name: &str) -> u64 {
    match js.get_stream(name).await {
        Ok(mut s) => s.info().await.map(|i| i.state.messages).unwrap_or(0),
        Err(_) => 0,
    }
}

/// VACUUM (ANALYZE) + CHECKPOINT before a measured batch — the walbench
/// normalize: no dead-tuple backlog, consistent first-touch FPI regime.
async fn normalize(db: &Client, tables: &[&str]) -> anyhow::Result<()> {
    for t in tables {
        db.batch_execute(&format!("VACUUM (ANALYZE) app.\"{t}\""))
            .await?;
    }
    db.batch_execute("CHECKPOINT").await?;
    Ok(())
}

async fn relreplident(db: &Client, table: &str) -> anyhow::Result<String> {
    Ok(db
        .query_one(
            "SELECT c.relreplident::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'app' AND c.relname = $1",
            &[&table],
        )
        .await
        .context("read relreplident")?
        .get(0))
}

fn parse_rates(s: &str) -> anyhow::Result<Vec<f64>> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .with_context(|| format!("bad --lag-rates {s:?} (want e.g. \"100,200\")"))?;
    if v.is_empty() || v.iter().any(|r| *r <= 0.0) {
        bail!("--lag-rates must be non-empty positive rates");
    }
    Ok(v)
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Provisioning + teardown (the rie2ebench substrate, reader-only scope)
// ---------------------------------------------------------------------------

/// Fresh throwaway DB with the REAL substrate: shipped system+catalog DDL,
/// registry rows (the reader reads its registration from here), the 3.2 app
/// floor for the poc-receiving catalog, replication role + publication +
/// entity map + grants. NO slot — each axis creates it at its own moment.
async fn provision(admin_url: &str) -> anyhow::Result<(Client, Client)> {
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    let stream_name = event_stream_name(ORG, ENV);

    // Hermetic preamble (leftovers mask): slot, database, role.
    let admin = connect(admin_url).await?;
    drop_slot(&admin, &cdc_name).await;
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

    let db = connect(&swap_db(admin_url, DB)).await?;
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
        ("catalog-schema.sql", CATALOG_SQL),
    ] {
        db.batch_execute(ddl)
            .await
            .with_context(|| format!("apply deploy/sql/{name}"))?;
    }

    // Registry rows for the reader's registration.
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
            &"wamn-db-ccdc--app--dev",
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

    // The app floor (the REAL 3.2 DDL for the poc-receiving catalog — the
    // same model C-WAL-0 measured, so the delta divides cleanly) + CDC.
    db.batch_execute(&provision_sql::ensure_schema_sql("app"))
        .await
        .context("app schema")?;
    // The floor grants table privileges to wamn_app (wamn-ddl emit); schema
    // USAGE comes from the project-provisioning verb in production — grant it
    // here explicitly (the walbench ephemeral-schema precedent).
    db.batch_execute("GRANT USAGE ON SCHEMA app TO wamn_app")
        .await
        .context("app schema usage grant")?;
    let floor = Migration::create(&catalog()?)
        .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
        .sql(Confirmation::None)
        .map_err(|e| anyhow::anyhow!("floor sql: {e}"))?;
    db.batch_execute(&format!("SET search_path TO app; {floor}"))
        .await
        .context("apply the 3.2 floor")?;
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
    for entity in &catalog()?.entities {
        db.execute(
            &provision_sql::upsert_entity_map_sql("app"),
            &[&entity.id.as_str(), &entity.name.as_str()],
        )
        .await
        .with_context(|| format!("map entity {}", entity.id))?;
    }
    db.batch_execute(&provision_sql::grant_replication_access_sql(
        DB, &cdc_name, "app",
    ))
    .await
    .context("grants")?;
    println!("provisioned {DB} from deploy/sql + the real builders (drift-proof)");
    Ok((admin, db))
}

/// ALWAYS-run teardown on FRESH connections (the provisioning-time connections
/// are dead after a switchover): slot, database (WITH FORCE — takes any idle
/// slot with it), role, stream. Zero residue — the §11 never-leave-a-slot rule.
async fn teardown(admin_url: &str, nats_url: &str) {
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    if let Ok(admin) = connect(admin_url).await {
        // Slot drop must run on the slot's database; DROP DATABASE WITH FORCE
        // is the backstop that takes an idle slot down with the DB.
        if let Ok(db) = connect(&swap_db(admin_url, DB)).await {
            drop_slot(&db, &cdc_name).await;
        }
        let _ = admin
            .batch_execute(&format!("DROP DATABASE IF EXISTS {DB} WITH (FORCE)"))
            .await;
        let _ = admin
            .batch_execute(&format!("DROP ROLE IF EXISTS {cdc_name}"))
            .await;
    }
    if let Ok(nats) = async_nats::connect(nats_url).await {
        let js = async_nats::jetstream::new(nats);
        let _ = js.delete_stream(&event_stream_name(ORG, ENV)).await;
    }
}

/// The embedded REAL reader — the same service body the deployment runs.
fn spawn_reader(
    admin_url: &str,
    nats_url: &str,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    let token = CancellationToken::new();
    let handle = tokio::spawn(run_with_token(
        EventReaderArgs {
            org: ORG.into(),
            project: PROJECT.into(),
            env: ENV.into(),
            system_database_url: swap_db(admin_url, DB),
            cdc_url: role_url(admin_url, &cdc_name, CDC_PW),
            nats_url: nats_url.to_string(),
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
    (token, handle)
}

async fn stop_reader(
    token: CancellationToken,
    handle: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> bool {
    token.cancel();
    matches!(
        tokio::time::timeout(Duration::from_secs(15), handle).await,
        Ok(Ok(Ok(())))
    )
}

// ---------------------------------------------------------------------------
// drain — decode drain rate after a bulk import (+ the mu4h spill evidence)
// ---------------------------------------------------------------------------

struct DrainVariant {
    name: &'static str,
    rows: usize,
    txn_rows: usize,
    wide: bool,
    /// `ALTER ROLE <cdc> SET logical_decoding_work_mem` for this variant's
    /// walsender session (None = the server default). The knob is USERSET, so
    /// a role-level setting reaches the reader's session without touching the
    /// shared instance — the forced-spill leg of the wamn-mu4h evidence.
    work_mem: Option<&'static str>,
}

async fn drain_mode(args: &CdcBenchArgs, pass: &mut bool) -> anyhow::Result<()> {
    println!(
        "\n## drain (C-CDC axis 1) — reader catch-up after a bulk import; \
         spill counters = the logical_decoding_work_mem evidence (wamn-mu4h)"
    );
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    let stream_name = event_stream_name(ORG, ENV);
    let (_admin, db) = provision(&args.admin_database_url).await?;
    let app = connect_app(&args.admin_database_url).await?;
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats);

    let ins_narrow = app
        .prepare(
            "INSERT INTO \"suppliers\" (tenant_id, name, contact_email, standard_cost) \
             VALUES (current_setting('app.tenant', true), $1, $2, $3::text::numeric)",
        )
        .await?;
    let ins_wide = app
        .prepare(
            "INSERT INTO \"users\" (tenant_id, email, display_name, cert_level) \
             VALUES (current_setting('app.tenant', true), $1, $2, $3)",
        )
        .await?;

    let variants = [
        DrainVariant {
            name: "batched-narrow",
            rows: args.drain_rows,
            txn_rows: args.drain_txn_rows.max(1),
            wide: false,
            work_mem: None,
        },
        DrainVariant {
            name: "singletxn-narrow",
            rows: args.drain_rows,
            txn_rows: args.drain_rows.max(1),
            wide: false,
            work_mem: None,
        },
        // The SAME single-txn import decoded under a starved reorder buffer
        // (the 64kB GUC minimum — the buffer holds ~190 B/change, tuple +
        // TOAST pointers, so any non-trivial txn spills): spill_txns/
        // spill_bytes prove the walsender picked the role GUC up, and the
        // drain-rate delta vs `singletxn-narrow` is what spilling costs
        // (wamn-mu4h: whether raising logical_decoding_work_mem always-on has
        // evidence at our txn shapes).
        DrainVariant {
            name: "singletxn-narrow-spill64kb",
            rows: args.drain_rows,
            txn_rows: args.drain_rows.max(1),
            wide: false,
            work_mem: Some("64kB"),
        },
        DrainVariant {
            name: "singletxn-wide",
            rows: args.drain_wide_rows,
            txn_rows: args.drain_wide_rows.max(1),
            wide: true,
            work_mem: None,
        },
    ];

    let mut csv = String::from(
        "variant,rows,txns,import_secs,import_wal_bytes,backlog_bytes,drain_secs,rows_per_sec,\
         backlog_mb_per_sec,spill_txns,spill_count,spill_bytes,stream_txns,stream_bytes,\
         total_txns,total_bytes\n",
    );
    let mut series = String::from("variant,t_ms,stream_msgs,lag_bytes\n");

    for v in &variants {
        println!(
            "\n### drain variant {} ({} rows, {} rows/txn)",
            v.name, v.rows, v.txn_rows
        );
        db.batch_execute("TRUNCATE app.\"suppliers\", app.\"users\" CASCADE")
            .await?;
        normalize(&db, &["suppliers", "users"]).await?;
        let _ = js.delete_stream(&stream_name).await;
        drop_slot(&db, &cdc_name).await;
        db.batch_execute(&provision_sql::create_failover_slot_sql(&cdc_name))
            .await
            .context("create failover slot")?;
        // Role-level logical_decoding_work_mem (USERSET) reaches the reader's
        // fresh walsender session; RESET restores the server default.
        match v.work_mem {
            Some(mem) => {
                db.batch_execute(&format!(
                    "ALTER ROLE {cdc_name} SET logical_decoding_work_mem = '{mem}'"
                ))
                .await
                .context("set variant logical_decoding_work_mem")?;
            }
            None => {
                db.batch_execute(&format!(
                    "ALTER ROLE {cdc_name} RESET logical_decoding_work_mem"
                ))
                .await
                .context("reset logical_decoding_work_mem")?;
            }
        }

        // The bulk import, reader DOWN — WAL accrues behind the slot. Import
        // WAL bracketed per txn (short brackets exclude ambient idle-gap WAL,
        // the C-WAL-0 discipline).
        let import_t0 = Instant::now();
        let mut import_wal: i64 = 0;
        let mut txns = 0usize;
        let mut sent = 0usize;
        while sent < v.rows {
            let n = v.txn_rows.min(v.rows - sent);
            let w0 = wal_lsn(&db).await?;
            app.batch_execute("BEGIN").await?;
            for i in sent..sent + n {
                if v.wide {
                    app.execute(
                        &ins_wide,
                        &[
                            &format!("u{i}@example.test"),
                            &Some(crate::walbench::wide_blob(i, args.wide_bytes)),
                            &Some("L1"),
                        ],
                    )
                    .await?;
                } else {
                    app.execute(
                        &ins_narrow,
                        &[
                            &format!("sup-{i}"),
                            &Some(format!("s{i}@example.test")),
                            &"12.50",
                        ],
                    )
                    .await?;
                }
            }
            app.batch_execute("COMMIT").await?;
            import_wal += wal_since(&db, &w0).await?;
            sent += n;
            txns += 1;
        }
        let import_secs = import_t0.elapsed().as_secs_f64();
        let backlog = slot_lag(&db, &cdc_name).await?;
        println!(
            "  imported {} rows in {} txns, {:.1}s — import WAL {} B, slot backlog {} B",
            v.rows, txns, import_secs, import_wal, backlog
        );

        // Reader up — the drain window starts here (includes session open).
        let (token, handle) = spawn_reader(&args.admin_database_url, &args.nats_url);
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(args.drain_deadline_secs);
        let drain_secs = loop {
            let msgs = stream_msgs(&js, &stream_name).await;
            let lag = slot_lag(&db, &cdc_name).await?;
            series.push_str(&format!(
                "{},{},{msgs},{lag}\n",
                v.name,
                t0.elapsed().as_millis()
            ));
            if msgs >= v.rows as u64 {
                break t0.elapsed().as_secs_f64();
            }
            if handle.is_finished() {
                bail!("reader died mid-drain: {:?}", handle.await);
            }
            if Instant::now() > deadline {
                bail!(
                    "drain {}: stream holds {msgs}/{} after {}s",
                    v.name,
                    v.rows,
                    args.drain_deadline_secs
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        // Settle a beat, then the exactness check (dedupe means == not >=).
        tokio::time::sleep(Duration::from_millis(750)).await;
        let final_msgs = stream_msgs(&js, &stream_name).await;
        check(
            pass,
            &format!(
                "drain {}: stream holds exactly {} row events (got {final_msgs})",
                v.name, v.rows
            ),
            final_msgs == v.rows as u64,
        );
        let stats = slot_stats(&db, &cdc_name).await?;
        check(
            pass,
            &format!("drain {}: reader alive through the drain", v.name),
            !handle.is_finished(),
        );
        if v.work_mem.is_some() {
            // The starved-buffer leg must actually spill — otherwise the
            // variant is measuring nothing and the mu4h comparison is vacuous.
            check(
                pass,
                &format!(
                    "drain {}: the starved reorder buffer spilled to disk (spill_txns {}, {} B)",
                    v.name, stats.spill_txns, stats.spill_bytes
                ),
                stats.spill_txns > 0 && stats.spill_bytes > 0,
            );
        }
        let clean = stop_reader(token, handle).await;
        check(
            pass,
            &format!("drain {}: reader cancelled cleanly", v.name),
            clean,
        );
        drop_slot(&db, &cdc_name).await;

        let rows_per_sec = v.rows as f64 / drain_secs;
        let mb_per_sec = backlog as f64 / drain_secs / 1e6;
        println!(
            "  drained in {drain_secs:.2}s — {rows_per_sec:.0} rows/s, {mb_per_sec:.1} MB/s of backlog; \
             spill: txns {} count {} bytes {}",
            stats.spill_txns, stats.spill_count, stats.spill_bytes
        );
        csv.push_str(&format!(
            "{},{},{txns},{import_secs:.1},{import_wal},{backlog},{drain_secs:.2},{rows_per_sec:.0},\
             {mb_per_sec:.2},{},{},{},{},{},{},{}\n",
            v.name,
            v.rows,
            stats.spill_txns,
            stats.spill_count,
            stats.spill_bytes,
            stats.stream_txns,
            stats.stream_bytes,
            stats.total_txns,
            stats.total_bytes,
        ));
    }

    emit_csv("ccdc-drain", &csv, &args.out);
    emit_csv("ccdc-drain-series", &series, &args.out);
    Ok(())
}

// ---------------------------------------------------------------------------
// lag — slot-lag knee vs sustained write rate
// ---------------------------------------------------------------------------

async fn lag_mode(args: &CdcBenchArgs, pass: &mut bool) -> anyhow::Result<()> {
    let rates = parse_rates(&args.lag_rates)?;
    println!(
        "\n## lag (C-CDC axis 2) — slot-lag knee vs sustained write rate; steps {rates:?}/s × {}s, \
         {} writers",
        args.lag_step_secs, args.lag_writers
    );
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    let stream_name = event_stream_name(ORG, ENV);
    let (_admin, db) = provision(&args.admin_database_url).await?;
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats);
    let _ = js.delete_stream(&stream_name).await;
    db.batch_execute(&provision_sql::create_failover_slot_sql(&cdc_name))
        .await
        .context("create failover slot")?;
    let (token, handle) = spawn_reader(&args.admin_database_url, &args.nats_url);

    // Warm write: proves the pipeline is live before the first step.
    let app = connect_app(&args.admin_database_url).await?;
    app.execute(
        "INSERT INTO \"suppliers\" (tenant_id, name) \
         VALUES (current_setting('app.tenant', true), 'lag-warm')",
        &[],
    )
    .await?;
    let warm_deadline = Instant::now() + Duration::from_secs(60);
    while stream_msgs(&js, &stream_name).await < 1 {
        if Instant::now() > warm_deadline {
            bail!("lag: warm write never reached the stream (reader dead?)");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mut total_written: u64 = 1; // the warm row

    let mut csv = String::from(
        "rate_target,writers,step_secs,written,achieved_rate,published_in_step,publish_rate,\
         lag_start_bytes,lag_end_bytes,lag_max_bytes\n",
    );
    let mut series = String::from("rate_target,t_ms,stream_msgs,lag_bytes\n");

    for &rate in &rates {
        let per_writer = rate / args.lag_writers as f64;
        let msgs_start = stream_msgs(&js, &stream_name).await;
        let lag_start = slot_lag(&db, &cdc_name).await?;
        let mut handles = Vec::new();
        for w in 0..args.lag_writers {
            let admin_url = args.admin_database_url.clone();
            let step_secs = args.lag_step_secs;
            handles.push(tokio::spawn(async move {
                let app = connect_app(&admin_url).await?;
                let ins = app
                    .prepare(
                        "INSERT INTO \"suppliers\" (tenant_id, name) \
                         VALUES (current_setting('app.tenant', true), $1)",
                    )
                    .await?;
                let start = Instant::now();
                let mut sent: u64 = 0;
                while start.elapsed().as_secs_f64() < step_secs as f64 {
                    let due = (start.elapsed().as_secs_f64() * per_writer) as u64 + 1;
                    while sent < due {
                        app.execute(&ins, &[&format!("lag-{rate}-{w}-{sent}")])
                            .await?;
                        sent += 1;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                anyhow::Ok(sent)
            }));
        }
        // Sample while the step runs.
        let t0 = Instant::now();
        let mut lag_max = lag_start;
        loop {
            let msgs = stream_msgs(&js, &stream_name).await;
            let lag = slot_lag(&db, &cdc_name).await?;
            lag_max = lag_max.max(lag);
            series.push_str(&format!(
                "{rate},{},{msgs},{lag}\n",
                t0.elapsed().as_millis()
            ));
            if handles.iter().all(|h| h.is_finished()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut written: u64 = 0;
        for h in handles {
            written += h.await.context("writer task")??;
        }
        total_written += written;
        let elapsed = t0.elapsed().as_secs_f64();
        let msgs_end = stream_msgs(&js, &stream_name).await;
        let lag_end = slot_lag(&db, &cdc_name).await?;
        let achieved = written as f64 / elapsed;
        let published = msgs_end.saturating_sub(msgs_start);
        let pub_rate = published as f64 / elapsed;
        println!(
            "  rate {rate:>6.0}/s  wrote {written:>6} ({achieved:>6.0}/s)  published {published:>6} \
             ({pub_rate:>6.0}/s)  lag start/end/max {lag_start}/{lag_end}/{lag_max} B"
        );
        csv.push_str(&format!(
            "{rate:.0},{},{},{written},{achieved:.0},{published},{pub_rate:.0},{lag_start},{lag_end},{lag_max}\n",
            args.lag_writers, args.lag_step_secs
        ));
        if handle.is_finished() {
            bail!("reader died mid-lag-step");
        }
    }

    // Eventual completeness: every committed row reaches the stream.
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(300);
    loop {
        let msgs = stream_msgs(&js, &stream_name).await;
        if msgs >= total_written {
            break;
        }
        if Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let catchup_secs = t0.elapsed().as_secs_f64();
    let final_msgs = stream_msgs(&js, &stream_name).await;
    check(
        pass,
        &format!(
            "lag: eventual completeness — stream holds exactly {total_written} events (got {final_msgs}, \
             caught up {catchup_secs:.1}s after the last step)"
        ),
        final_msgs == total_written,
    );
    println!("  post-ramp catch-up: {catchup_secs:.1}s");
    let clean = stop_reader(token, handle).await;
    check(pass, "lag: reader cancelled cleanly", clean);
    drop_slot(&db, &cdc_name).await;

    emit_csv("ccdc-lag", &csv, &args.out);
    emit_csv("ccdc-lag-series", &series, &args.out);
    Ok(())
}

// ---------------------------------------------------------------------------
// ri — WAL delta under REPLICA IDENTITY FULL per table class
// ---------------------------------------------------------------------------

struct OpStats {
    p50_ms: f64,
    p99_ms: f64,
    /// Batch WAL / n — FPI-inclusive, the C-WAL-0-comparable figure.
    wal_mean: f64,
    /// Median of PER-OP WAL brackets — the RI-delta statistic: the handful of
    /// first-touch-after-checkpoint ops that pay an 8 KB full-page image are
    /// outliers the median excludes (they'd otherwise drown a ~50 B old-image
    /// delta), and on a shared instance ambient WAL outside the op's sub-ms
    /// bracket never enters it (the C-WAL-0 per-event discipline).
    wal_p50: f64,
}

/// One measured batch (the walbench discipline): normalize, bracket the
/// instance insert LSN, run `n` ops, divide.
async fn op_batch(
    admin: &Client,
    tables: &[&str],
    n: usize,
    mut op: impl AsyncFnMut(usize) -> anyhow::Result<()>,
) -> anyhow::Result<OpStats> {
    normalize(admin, tables).await?;
    let wal0 = wal_lsn(admin).await?;
    let mut samples: Vec<Duration> = Vec::with_capacity(n);
    let mut wals: Vec<i64> = Vec::with_capacity(n);
    for i in 0..n {
        let w0 = wal_lsn(admin).await?;
        let t = Instant::now();
        op(i).await?;
        samples.push(t.elapsed());
        wals.push(wal_since(admin, &w0).await?);
    }
    let wal = wal_since(admin, &wal0).await?;
    samples.sort();
    wals.sort_unstable();
    Ok(OpStats {
        p50_ms: percentile(&samples, 0.50).as_secs_f64() * 1e3,
        p99_ms: percentile(&samples, 0.99).as_secs_f64() * 1e3,
        wal_mean: wal as f64 / n as f64,
        wal_p50: wals.get(wals.len() / 2).copied().unwrap_or(0) as f64,
    })
}

/// One regime leg: narrow ins/upd/del + wide ins/upd/upd-slim/del, returning
/// `(shape, op) → per-op-median WAL` for the physics asserts.
async fn ri_leg(
    admin: &Client,
    app: &Client,
    regime: &str,
    n: usize,
    wide_bytes: usize,
    csv: &mut String,
) -> anyhow::Result<std::collections::HashMap<(String, String), f64>> {
    // Identical page regime for both legs: zero-page relations (TRUNCATE
    // resets heap + indexes + FSM), then the normalize. Without this the
    // DEFAULT leg pays brand-new-relation FPI/extension costs the FULL leg
    // doesn't, and the ratios drown at small batch sizes.
    admin
        .batch_execute("TRUNCATE app.\"suppliers\", app.\"users\" CASCADE")
        .await?;
    normalize(admin, &["suppliers", "users"]).await?;
    let mut out = std::collections::HashMap::new();
    let mut record = |csv: &mut String, shape: &str, op: &str, s: &OpStats| {
        println!(
            "  {regime:<15} {shape:<6} {op:<9}  p50 {:>7.3}ms  p99 {:>7.3}ms  \
             wal/op p50 {:>7.0}B  mean {:>7.0}B",
            s.p50_ms, s.p99_ms, s.wal_p50, s.wal_mean
        );
        csv.push_str(&format!(
            "{shape},{op},{regime},{n},{:.3},{:.3},{:.0},{:.0}\n",
            s.p50_ms, s.p99_ms, s.wal_p50, s.wal_mean
        ));
        out.insert((shape.to_string(), op.to_string()), s.wal_p50);
    };

    // narrow: suppliers.
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
    let mut ids: Vec<String> = Vec::with_capacity(n);
    let st = op_batch(admin, &["suppliers"], n, async |i| {
        let id: String = app
            .query_one(
                &s_ins,
                &[
                    &format!("sup-{regime}-{i}"),
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
    record(csv, "narrow", "insert", &st);
    let st = op_batch(admin, &["suppliers"], n, async |i| {
        app.execute(&s_upd, &[&ids[i], &Some("touched")]).await?;
        Ok(())
    })
    .await?;
    record(csv, "narrow", "update", &st);
    let st = op_batch(admin, &["suppliers"], n, async |i| {
        app.execute(&s_del, &[&ids[i]]).await?;
        Ok(())
    })
    .await?;
    record(csv, "narrow", "delete", &st);

    // wide/TOASTy: users. `upd-slim` touches only the small `cert_level`
    // column — under FULL the UNCHANGED 6 KiB old image is still flattened
    // into WAL (the l5i9.63 probe).
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
    let u_upd_slim = app
        .prepare("UPDATE \"users\" SET cert_level = $2 WHERE id = $1::text::uuid")
        .await?;
    let u_del = app
        .prepare("DELETE FROM \"users\" WHERE id = $1::text::uuid")
        .await?;
    let mut wids: Vec<String> = Vec::with_capacity(n);
    let st = op_batch(admin, &["users"], n, async |i| {
        let id: String = app
            .query_one(
                &u_ins,
                &[
                    &format!("u-{regime}-{i}@example.test"),
                    &Some(crate::walbench::wide_blob(i, wide_bytes)),
                    &Some("L1"),
                ],
            )
            .await?
            .get(0);
        wids.push(id);
        Ok(())
    })
    .await?;
    record(csv, "wide", "insert", &st);
    let st = op_batch(admin, &["users"], n, async |i| {
        app.execute(
            &u_upd,
            &[
                &wids[i],
                &Some(crate::walbench::wide_blob(i + n, wide_bytes)),
            ],
        )
        .await?;
        Ok(())
    })
    .await?;
    record(csv, "wide", "update", &st);
    let st = op_batch(admin, &["users"], n, async |i| {
        app.execute(&u_upd_slim, &[&wids[i], &Some("L2")]).await?;
        Ok(())
    })
    .await?;
    record(csv, "wide", "upd-slim", &st);
    let st = op_batch(admin, &["users"], n, async |i| {
        app.execute(&u_del, &[&wids[i]]).await?;
        Ok(())
    })
    .await?;
    record(csv, "wide", "delete", &st);

    Ok(out)
}

async fn ri_mode(args: &CdcBenchArgs, pass: &mut bool) -> anyhow::Result<()> {
    println!(
        "\n## ri (C-CDC axis 3) — per-op WAL, REPLICA IDENTITY DEFAULT vs FULL @ wal_level=logical \
         ({} ops/batch; C-WAL-0 is the wal_level=replica denominator)",
        args.ri_iters
    );
    let (_admin, db) = provision(&args.admin_database_url).await?;
    let app = connect_app(&args.admin_database_url).await?;
    let n = args.ri_iters;

    // The delete registrations that DRIVE the flip (the real l5i9.31 path:
    // requires_replica_identity_full over the registration, not a hand ALTER).
    for (reg_id, entity) in [("r-sup", "suppliers"), ("r-usr", "users")] {
        db.execute(
            "INSERT INTO catalog.event_registrations \
             (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb)",
            &[
                &TENANT,
                &CATALOG_ID,
                &reg_id,
                &"ccdc-flow",
                &entity,
                &registration_json(reg_id, entity),
            ],
        )
        .await
        .with_context(|| format!("seed registration {reg_id}"))?;
    }

    check(
        pass,
        "ri: suppliers + users start at REPLICA IDENTITY DEFAULT",
        relreplident(&db, "suppliers").await? == "d" && relreplident(&db, "users").await? == "d",
    );
    let mut csv = String::from(
        "shape,op,regime,n,p50_ms,p99_ms,wal_bytes_per_op_p50,wal_bytes_per_op_mean\n",
    );
    let d = ri_leg(&db, &app, "default-logical", n, args.wide_bytes, &mut csv).await?;

    // The REAL reconcile (l5i9.31/l5i9.61) — the delete registrations demand
    // FULL for exactly these two tables.
    let plan =
        wamn_ctl::reconcile_replica_identity::reconcile(&db, &catalog()?, "app", true).await?;
    let flipped: Vec<&str> = plan.flips.iter().map(|f| f.table.as_str()).collect();
    check(
        pass,
        &format!("ri: reconcile flipped exactly suppliers + users to FULL (flips: {flipped:?})"),
        plan.flips.len() == 2
            && flipped.contains(&"suppliers")
            && flipped.contains(&"users")
            && relreplident(&db, "suppliers").await? == "f"
            && relreplident(&db, "users").await? == "f",
    );

    let f = ri_leg(&db, &app, "full-logical", n, args.wide_bytes, &mut csv).await?;

    // Physics sanity: the FULL old image must show up as WAL where the
    // mechanism says it does — and NOT where it doesn't (insert). A skipped
    // flip (the mutation target) makes these legs identical and fails here.
    let get = |m: &std::collections::HashMap<(String, String), f64>, s: &str, o: &str| {
        m[&(s.to_string(), o.to_string())]
    };
    check(
        pass,
        &format!(
            "ri: narrow DELETE grows under FULL (default {:.0} B → full {:.0} B)",
            get(&d, "narrow", "delete"),
            get(&f, "narrow", "delete")
        ),
        get(&f, "narrow", "delete") > get(&d, "narrow", "delete") * 1.1,
    );
    check(
        pass,
        &format!(
            "ri: wide upd-slim pays the flattened old image under FULL (default {:.0} B → full {:.0} B)",
            get(&d, "wide", "upd-slim"),
            get(&f, "wide", "upd-slim")
        ),
        get(&f, "wide", "upd-slim") > get(&d, "wide", "upd-slim") * 2.0,
    );
    check(
        pass,
        &format!(
            "ri: wide DELETE grows under FULL (default {:.0} B → full {:.0} B)",
            get(&d, "wide", "delete"),
            get(&f, "wide", "delete")
        ),
        get(&f, "wide", "delete") > get(&d, "wide", "delete") * 1.3,
    );
    // INSERT is RI-independent — a large drift here means the environment
    // moved between legs, which would poison every ratio above.
    let (di, fi) = (get(&d, "narrow", "insert"), get(&f, "narrow", "insert"));
    check(
        pass,
        &format!(
            "ri: narrow INSERT unchanged by RI (default {di:.0} B vs full {fi:.0} B, within 30%)"
        ),
        (fi - di).abs() / di < 0.30,
    );

    emit_csv("ccdc-ri", &csv, &args.out);
    Ok(())
}

// ---------------------------------------------------------------------------
// switchover — the timed availability drill (cdc1 shape + the REAL reader)
// ---------------------------------------------------------------------------

async fn switchover_mode(args: &CdcBenchArgs, pass: &mut bool) -> anyhow::Result<()> {
    println!(
        "\n## switchover (C-CDC axis 4) — timed availability drill: trigger the promotion / \
         primary restart INSIDE the {}s window",
        args.secs
    );
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV);
    let stream_name = event_stream_name(ORG, ENV);
    let (_admin, db) = provision(&args.admin_database_url).await?;
    let nats = async_nats::connect(&args.nats_url)
        .await
        .with_context(|| format!("connect NATS at {}", args.nats_url))?;
    let js = async_nats::jetstream::new(nats);
    let _ = js.delete_stream(&stream_name).await;
    db.batch_execute(&provision_sql::create_failover_slot_sql(&cdc_name))
        .await
        .context("create failover slot")?;
    let (token, handle) = spawn_reader(&args.admin_database_url, &args.nats_url);

    // Warm write as the superuser with an explicit tenant (no dependence on a
    // wamn_app password on a shared cluster); `sw-warm` never parses as a seq.
    db.execute(
        "INSERT INTO app.suppliers (tenant_id, name) VALUES ($1, 'sw-warm')",
        &[&TENANT],
    )
    .await?;
    let warm_deadline = Instant::now() + Duration::from_secs(60);
    while stream_msgs(&js, &stream_name).await < 1 {
        if Instant::now() > warm_deadline {
            bail!("switchover: warm write never reached the stream (reader dead?)");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Stream tail: collect `sw-<seq>` insert envelopes with their JetStream
    // ingest timestamps (the publish-side clock) + a dupe count.
    type Tail = Arc<Mutex<std::collections::BTreeMap<i64, (i64, u32)>>>;
    let received: Tail = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
    let tail_token = CancellationToken::new();
    let tail = {
        let received = received.clone();
        let token = tail_token.clone();
        let js = js.clone();
        let stream_name = stream_name.clone();
        tokio::spawn(async move {
            let stream = js
                .get_stream(&stream_name)
                .await
                .map_err(|e| anyhow::anyhow!("tail get_stream: {e}"))?;
            let consumer = stream
                .create_consumer(async_nats::jetstream::consumer::pull::Config {
                    deliver_policy: DeliverPolicy::All,
                    ack_policy: AckPolicy::None,
                    ..Default::default()
                })
                .await
                .map_err(|e| anyhow::anyhow!("tail consumer: {e}"))?;
            while !token.is_cancelled() {
                let mut batch = match consumer.fetch().max_messages(500).messages().await {
                    Ok(b) => b,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                };
                let mut got_any = false;
                while let Some(Ok(msg)) = batch.next().await {
                    got_any = true;
                    let Ok(env) = serde_json::from_slice::<wamn_event_wire::Envelope>(&msg.payload)
                    else {
                        continue;
                    };
                    if env.table != "suppliers" || !matches!(env.op, wamn_event_wire::Op::Insert) {
                        continue;
                    }
                    let Some(seq) = env
                        .new
                        .as_ref()
                        .and_then(|m| m.get("name"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.strip_prefix("sw-"))
                        .and_then(|s| s.parse::<i64>().ok())
                    else {
                        continue;
                    };
                    let ingest_ms = msg
                        .info()
                        .map(|i| (i.published.unix_timestamp_nanos() / 1_000_000) as i64)
                        .unwrap_or_else(|_| unix_ms());
                    let mut map = received.lock().unwrap();
                    map.entry(seq)
                        .and_modify(|(_, c)| *c += 1)
                        .or_insert((ingest_ms, 1));
                }
                if !got_any {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
            anyhow::Ok(())
        })
    };

    println!(
        "\n>>> DRILL WINDOW OPEN ({}s) — TRIGGER NOW: multi-instance: `kubectl cnpg promote <cluster> <standby>`; \
         single-instance wamn-pg: `kubectl -n wamn-system delete pod wamn-pg-1` (CNPG recreates the primary) <<<\n",
        args.secs
    );

    // Reconnecting writer (the cdc1 shape): committed ONLY on a clean 1-row
    // result — an errored commit is unknown-outcome and never counted (it can
    // only surface as an on-stream EXTRA, reported, not failed).
    let mut committed: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
    let mut seq: i64 = 0;
    let started = Instant::now();
    let mut writer: Option<Client> = None;
    while started.elapsed() < Duration::from_secs(args.secs) {
        if writer.is_none() {
            match connect(&swap_db(&args.admin_database_url, DB)).await {
                Ok(c) => {
                    println!(
                        "[writer {:>5.1}s] connected",
                        started.elapsed().as_secs_f32()
                    );
                    writer = Some(c);
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            }
        }
        seq += 1;
        let res = writer
            .as_ref()
            .unwrap()
            .execute(
                "INSERT INTO app.suppliers (tenant_id, name) VALUES ($1, $2)",
                &[&TENANT, &format!("sw-{seq}")],
            )
            .await;
        match res {
            Ok(1) => {
                committed.insert(seq, unix_ms());
            }
            Ok(_) => {}
            Err(e) => {
                println!(
                    "[writer {:>5.1}s] write error (reconnecting): {e}",
                    started.elapsed().as_secs_f32()
                );
                writer = None;
            }
        }
        tokio::time::sleep(Duration::from_millis(args.write_interval_ms)).await;
    }
    drop(writer);

    // Catch-up: every committed row must reach the stream (delayed, never
    // lost — the reader's R11 re-open ladder is the recovery under test).
    let catchup_t0 = Instant::now();
    let catchup_deadline = catchup_t0 + Duration::from_secs(300);
    loop {
        let missing = {
            let rec = received.lock().unwrap();
            committed.keys().filter(|s| !rec.contains_key(s)).count()
        };
        if missing == 0 || Instant::now() > catchup_deadline {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let catchup_secs = catchup_t0.elapsed().as_secs_f64();
    tail_token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), tail).await;
    let reader_alive = !handle.is_finished();
    let clean = stop_reader(token, handle).await;

    // Metrics.
    let rec = received.lock().unwrap().clone();
    let missing: Vec<i64> = committed
        .keys()
        .filter(|s| !rec.contains_key(s))
        .copied()
        .collect();
    let extras = rec.keys().filter(|s| !committed.contains_key(s)).count();
    let dupes: u32 = rec.values().map(|(_, c)| c.saturating_sub(1)).sum();
    let commit_times: Vec<i64> = committed.values().copied().collect();
    let write_blackout_ms = commit_times
        .windows(2)
        .map(|w| w[1] - w[0])
        .max()
        .unwrap_or(0);
    let mut ingests: Vec<i64> = rec
        .iter()
        .filter(|(s, _)| committed.contains_key(s))
        .map(|(_, (t, _))| *t)
        .collect();
    ingests.sort_unstable();
    let publish_gap_ms = ingests.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0);
    let mut latencies: Vec<i64> = committed
        .iter()
        .filter_map(|(s, t)| rec.get(s).map(|(i, _)| i - t))
        .collect();
    latencies.sort_unstable();
    let lat = |p: f64| {
        latencies
            .get(((latencies.len() as f64 - 1.0) * p).round() as usize)
            .copied()
            .unwrap_or(0)
    };
    println!(
        "\nSWITCHOVER: committed={} received={} missing={} extras={extras} dupes={dupes} \
         write_blackout={write_blackout_ms}ms publish_gap={publish_gap_ms}ms catchup={catchup_secs:.1}s \
         commit→ingest p50/p95/max {}/{}/{} ms",
        committed.len(),
        rec.len(),
        missing.len(),
        lat(0.50),
        lat(0.95),
        latencies.last().copied().unwrap_or(0),
    );
    check(
        pass,
        &format!(
            "switchover: NO GAP — every committed row on the stream ({} committed, missing {:?})",
            committed.len(),
            missing
        ),
        missing.is_empty() && !committed.is_empty(),
    );
    check(
        pass,
        &format!("switchover: exactly-once on-stream (dupes {dupes} — Msg-Id dedupe held)"),
        dupes == 0,
    );
    check(
        pass,
        &format!(
            "switchover: the drill actually severed the pipeline (publish gap {publish_gap_ms}ms > 2000ms)"
        ),
        publish_gap_ms > 2000,
    );
    check(
        pass,
        "switchover: reader survived to the end (re-open ladder, not death)",
        reader_alive,
    );
    check(pass, "switchover: reader cancelled cleanly", clean);

    let mut csv = String::from(
        "committed,received,missing,extras,dupes,write_blackout_ms,publish_gap_ms,catchup_secs,\
         latency_p50_ms,latency_p95_ms,latency_max_ms\n",
    );
    csv.push_str(&format!(
        "{},{},{},{extras},{dupes},{write_blackout_ms},{publish_gap_ms},{catchup_secs:.1},{},{},{}\n",
        committed.len(),
        rec.len(),
        missing.len(),
        lat(0.50),
        lat(0.95),
        latencies.last().copied().unwrap_or(0),
    ));
    let mut series = String::from("seq,commit_unix_ms,ingest_unix_ms\n");
    for (s, t) in &committed {
        if let Some((i, _)) = rec.get(s) {
            series.push_str(&format!("{s},{t},{i}\n"));
        }
    }
    emit_csv("ccdc-switchover", &csv, &args.out);
    emit_csv("ccdc-switchover-series", &series, &args.out);
    Ok(())
}

// ---------------------------------------------------------------------------
// The dispatcher
// ---------------------------------------------------------------------------

pub async fn run(args: CdcBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates cdcbench (wamn-l5i9.14 EVT-C-CDC — measurement, not a gate)");
    {
        // Provenance header: the knobs every number depends on.
        let admin = connect(&args.admin_database_url).await?;
        let wal_level: String = admin.query_one("SHOW wal_level", &[]).await?.get(0);
        let ldwm: String = admin
            .query_one("SHOW logical_decoding_work_mem", &[])
            .await?
            .get(0);
        let keep: String = admin
            .query_one("SHOW max_slot_wal_keep_size", &[])
            .await?
            .get(0);
        let ver: String = admin.query_one("SHOW server_version", &[]).await?.get(0);
        println!(
            "provenance: pg {ver}, wal_level={wal_level}, logical_decoding_work_mem={ldwm}, \
             max_slot_wal_keep_size={keep}"
        );
        if wal_level != "logical" {
            bail!("cdcbench needs wal_level=logical (got {wal_level})");
        }
    }

    let run_all = args.mode == Mode::All;
    let mut pass = true;
    let mut outcome: anyhow::Result<()> = Ok(());
    for (selected, body) in [
        (run_all || args.mode == Mode::Drain, "drain"),
        (run_all || args.mode == Mode::Lag, "lag"),
        (run_all || args.mode == Mode::Ri, "ri"),
        (args.mode == Mode::Switchover, "switchover"),
    ] {
        if !selected {
            continue;
        }
        let r = match body {
            "drain" => drain_mode(&args, &mut pass).await,
            "lag" => lag_mode(&args, &mut pass).await,
            "ri" => ri_mode(&args, &mut pass).await,
            _ => switchover_mode(&args, &mut pass).await,
        };
        // Zero residue whatever happened (fresh connections — a switchover
        // kills the mode's own).
        teardown(&args.admin_database_url, &args.nats_url).await;
        if let Err(e) = r {
            outcome = Err(e);
            break;
        }
    }
    outcome?;

    println!("\ncdcbench complete — overall PASS: {pass}");
    if !pass {
        bail!("a C-CDC sanity/completeness assert failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_parse_and_reject_junk() {
        assert_eq!(parse_rates("100,200").unwrap(), vec![100.0, 200.0]);
        assert!(parse_rates("").is_err());
        assert!(parse_rates("100,x").is_err());
        assert!(parse_rates("0").is_err());
    }

    /// The registration fixture parses as the FROZEN wamn-event-reg type and
    /// demands the old image (delete-subscribed) — the property that makes the
    /// REAL reconcile flip the ri axis's tables, before the gate needs infra.
    #[test]
    fn registrations_parse_frozen_and_drive_the_flip() {
        for (reg_id, entity) in [("r-sup", "suppliers"), ("r-usr", "users")] {
            let reg =
                wamn_event_reg::EventRegistration::from_json(&registration_json(reg_id, entity))
                    .expect("frozen EventRegistration parses");
            assert!(
                reg.requires_replica_identity_full(),
                "{reg_id} must demand FULL (it drives the reconcile)"
            );
        }
    }

    /// The catalog compiles the floor for exactly the shapes the axes write,
    /// and its id matches the registrations' catalog binding.
    #[test]
    fn catalog_matches_the_measured_shapes() {
        let cat = catalog().expect("poc-receiving catalog parses");
        assert_eq!(cat.catalog_id, CATALOG_ID);
        let floor = Migration::create(&cat)
            .unwrap()
            .sql(Confirmation::None)
            .unwrap();
        for t in ["suppliers", "users"] {
            assert!(
                floor.contains(&format!("CREATE TABLE \"{t}\"")),
                "floor creates {t}"
            );
        }
        assert!(
            !floor.to_lowercase().contains("trigger"),
            "no trigger in the CDC-era floor (outbox retired, l5i9.19)"
        );
    }

    #[test]
    fn url_helpers_swap_only_their_segment() {
        let u = "postgres://postgres:pw@host:5432/postgres";
        assert_eq!(swap_db(u, DB), "postgres://postgres:pw@host:5432/wamn_ccdc");
        assert_eq!(
            role_url(u, "r", "p"),
            format!("postgres://r:p@host:5432/{DB}")
        );
    }
}
