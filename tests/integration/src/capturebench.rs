//! The `capturebench` subcommand: admitted-run node I/O capture gates.
//!
//! Pure host-side (no wasm guest): it applies the REAL
//! `deploy/sql/run-state.sql` into a throwaway ephemeral schema, then exercises
//! the SAME pure capture logic (`wamn_run_state::capture`) and the SAME `node_runs`
//! insert builders the flowrunner guest binds — so the admitted mode's facts,
//! the reconstruction verdict, the secret-containment property, and the
//! retention verb all run against real Postgres over the real prepared statements
//! (SR12b), without standing up the wasm runtime.
//!
//! Gate phases:
//!   off              — `off` writes no node I/O facts and reconstructs to CaptureOff.
//!   output-too-large — a full-capture output over the fixed ceiling is NULL with
//!                      its full size and content hash retained.
//!   redaction        — full-capture success and error rows contain no known raw secret.
//!   retention — old + recent terminal runs (plus a non-terminal run) are
//!               seeded; the REAL `prune-run-history` verb
//!               logic prunes the old terminal run (cascading its node_runs), keeps
//!               the recent one and the non-terminal one.
//!   all       — every mode in sequence.

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};

use wamn_flow::{Flow, ResolvedInterfaces};
use wamn_run_state::{
    CaptureMode, NodeRunRecord, ReconstructError, RunRecord, capture, reconstruct,
};
use wamn_runner::Plan;

use crate::ctl_process;

const SCHEMA: &str = "wamn_capture";
const CATALOG_SCHEMA: &str = "wamn_capture_catalog";
const TENANT: &str = "capture-t";
/// The known secret seeded through the scrub gate — asserted to appear NOWHERE.
const SECRET: &str = "hunter2-TOPSECRET-9f3c";

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Off,
    OutputTooLarge,
    Redaction,
    Retention,
    All,
}

#[derive(Debug, Args)]
pub struct CaptureBenchArgs {
    /// App (wamn_app) Postgres URL — the NOSUPERUSER/NOBYPASSRLS role that writes
    /// node_runs and prunes. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: applies/drops the ephemeral run-state schema.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which gate to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,
}

// ---------------------------------------------------------------------------
// Ephemeral schema: the REAL run-state.sql, schema-rewritten (no stand-in DDL,
// so the node_runs shape can never drift from the schema of record).
// ---------------------------------------------------------------------------

fn run_state_ddl() -> String {
    include_str!("../../../deploy/sql/run-state.sql")
        .replace("wamn_run", SCHEMA)
        .replace(
            "catalog.catalog_heads",
            &format!("{CATALOG_SCHEMA}.catalog_heads"),
        )
        .replace(
            "catalog.release_manifests",
            &format!("{CATALOG_SCHEMA}.release_manifests"),
        )
        .replace(
            "catalog.execution_bundles",
            &format!("{CATALOG_SCHEMA}.execution_bundles"),
        )
}

async fn admin_exec(admin_url: &str, sql: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    // `.context`, not `anyhow!("{e}")`: tokio_postgres::Error's Display is only
    // its kind ("db error"); the server's message — e.g. `role
    // "wamn_scenario_author" does not exist` — hangs off `source()`. Formatting
    // the error away collapsed every provisioning failure to "admin exec: db
    // error"; chaining keeps the cause in the `Caused by:` report.
    let r = client.batch_execute(sql).await.context("admin exec");
    drop(client);
    let _ = conn_task.await;
    r
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS {CATALOG_SCHEMA} CASCADE; \
             CREATE SCHEMA {CATALOG_SCHEMA}; \
             CREATE TABLE {CATALOG_SCHEMA}.release_manifests ( \
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL, \
               PRIMARY KEY (tenant_id, catalog_id, catalog_version) \
             ); \
             CREATE TABLE {CATALOG_SCHEMA}.execution_bundles ( \
               tenant_id text NOT NULL, execution_bundle_hash text NOT NULL, \
               PRIMARY KEY (tenant_id, execution_bundle_hash) \
             ); \
             INSERT INTO {CATALOG_SCHEMA}.release_manifests \
             VALUES ('capture-t','capture-fixture',1); \
             INSERT INTO {CATALOG_SCHEMA}.execution_bundles \
             VALUES ('capture-t', \
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a');"
        ),
    )
    .await?;
    admin_exec(admin_url, &run_state_ddl()).await
}

async fn teardown(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS {CATALOG_SCHEMA} CASCADE;"
        ),
    )
    .await
}

async fn reset(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(admin_url, &format!("TRUNCATE {SCHEMA}.runs CASCADE;")).await
}

/// A wamn_app session pinned to the capture schema + tenant claim.
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

// ---------------------------------------------------------------------------
// Row helpers — the SAME insert builders the flowrunner guest binds.
// ---------------------------------------------------------------------------

/// Insert a `runs` row (the node_runs FK parent). `created_at` is `now()` shifted
/// back `age_days` days so the retention gate can seed aged history.
async fn seed_run(
    admin_url: &str,
    run_id: &str,
    status: &str,
    age_days: i64,
    capture_mode: CaptureMode,
) -> anyhow::Result<()> {
    let trigger_source = match capture_mode {
        CaptureMode::Full => "scenario-draft",
        CaptureMode::Off => "test",
    };
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin seed-run connect")?;
    let conn_task = tokio::spawn(conn);
    let result = client
        .execute(
            &format!("INSERT INTO {SCHEMA}.runs ( \
               tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment, \
               execution_bundle_hash, status, created_at, trigger_source, capture_mode \
             ) VALUES ($1, $2, 'f', 1, \
                     'capture-fixture', 1, 'test', \
                     'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', $3, \
                     now() - ($4::bigint * interval '1 day'), $5, $6)"),
            &[
                &TENANT,
                &run_id,
                &status,
                &age_days,
                &trigger_source,
                &capture_mode.as_str(),
            ],
        )
        .await
        .context("seed run");
    drop(client);
    let _ = conn_task.await;
    result.map(|_| ())
}

fn to_jsonb(s: &Option<String>) -> Option<Value> {
    s.as_deref()
        .map(|t| serde_json::from_str(t).expect("captured json re-parses"))
}

/// Write a completed `success` node-run via `insert_node_run_success_sql` with the
/// capture columns `capture::derive` produced — the exact nine-param bind the guest
/// makes.
async fn write_success(
    client: &Client,
    run_id: &str,
    node_id: &str,
    seq: i32,
    port: &str,
    c: &capture::Captured,
) -> anyhow::Result<()> {
    let out_j = to_jsonb(&c.output_json);
    let in_j = to_jsonb(&c.input_json);
    let occ: i32 = 0;
    client
        .execute(
            &wamn_run_state::sql::insert_node_run_success_sql(),
            &[
                &run_id,
                &node_id,
                &occ,
                &seq,
                &port,
                &out_j,
                &in_j,
                &c.output_size,
                &c.payload_hash,
            ],
        )
        .await
        .context("write success node_run")?;
    Ok(())
}

/// Write a completed `error` node-run via `insert_node_run_error_sql`. `detail` is
/// the taxonomy blob — scrubbed here when the payloads were scrubbed, mirroring the
/// guest's error path (the detail can echo the payload).
async fn write_error(
    client: &Client,
    run_id: &str,
    node_id: &str,
    seq: i32,
    kind: &str,
    mut detail: Value,
    c: &capture::Captured,
) -> anyhow::Result<()> {
    capture::scrub(&mut detail);
    let out_j = to_jsonb(&c.output_json);
    let in_j = to_jsonb(&c.input_json);
    let occ: i32 = 0;
    client
        .execute(
            &wamn_run_state::sql::insert_node_run_error_sql(),
            &[
                &run_id,
                &node_id,
                &occ,
                &seq,
                &out_j,
                &in_j,
                &kind,
                &detail,
                &c.output_size,
                &c.payload_hash,
            ],
        )
        .await
        .context("write error node_run")?;
    Ok(())
}

/// Completion ports for the fixture's one ordinary node type. 9b7e2a7 gave
/// validation this port map, so a plan can no longer be compiled without it; the
/// engine owns the `cron` entry's ports, which is why entry types are absent.
fn resolved_interfaces() -> ResolvedInterfaces {
    ResolvedInterfaces::from([("echo".to_string(), vec!["main".to_string()])])
}

/// A minimal linear flow `a -> b` behind a typed entry.
///
/// 9b7e2a7 (wamn-5wd1.34) replaced the document's `trigger` + `entry` fields with
/// a typed entry NODE (`request`/`cron`/`event`). Capture is an admission fact and
/// is deliberately absent from this authored document, so
/// `cron`, the config-free entry that imposes no `respond` discipline, stands in
/// for the old `{"type": "manual"}` trigger, and `a` stays an ordinary captured
/// work node rather than becoming the entry itself.
fn linear_flow() -> Flow {
    let graph = json!({
        "schema-version": "0.1", "flow-id": "cap", "version": 1,
        "nodes": [
            {"id": "in", "type": "cron"},
            {"id": "a", "type": "echo"},
            {"id": "b", "type": "echo"}
        ],
        "edges": [{"from": "in", "to": "a"}, {"from": "a", "to": "b"}],
    });
    Flow::from_json(&graph.to_string()).expect("capture fixture flow parses")
}

/// Read a run's completed node-runs back and fold them through reconstruction —
/// the driver's exact resume path, so a NULL `output_json` (capture off) surfaces
/// as CaptureOff.
fn load_node_runs(rows: &[tokio_postgres::Row], run_id: &str) -> Vec<NodeRunRecord> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let node_id: String = row.get(0);
        let current_plan_hash: String = row.get(1);
        let occurrence: i32 = row.get(2);
        let seq: i32 = row.get(3);
        let port: Option<String> = row.get(4);
        let output_text: Option<String> = row.get(5);
        // SQL NULL output => None => CaptureOff, exactly as the guest maps it.
        let output = output_text.map(|s| serde_json::from_str::<Value>(&s).expect("output json"));
        let mut rec = NodeRunRecord::success(
            run_id,
            current_plan_hash,
            &node_id,
            seq as u32,
            port.unwrap_or_else(|| "main".into()),
            Value::Null,
        );
        rec.occurrence = occurrence as u32;
        rec.output = output;
        out.push(rec);
    }
    out
}

async fn reconstruct_verdict(
    client: &Client,
    flow: &Flow,
    run_id: &str,
) -> anyhow::Result<Result<(), ReconstructError>> {
    let rows = client
        .query(
            &wamn_run_state::sql::select_completed_node_runs_sql(),
            &[&run_id],
        )
        .await
        .context("read completed node_runs")?;
    let node_runs = load_node_runs(&rows, run_id);
    let plan =
        Plan::compile(flow, &resolved_interfaces()).map_err(|e| anyhow::anyhow!("compile: {e}"))?;
    let run = RunRecord::new(run_id, "cap", 1, json!({ "trig": 1 }));
    Ok(reconstruct(&plan, &run, &node_runs).map(|_| ()))
}

// ---------------------------------------------------------------------------
// off: no I/O facts + CaptureOff replay
// ---------------------------------------------------------------------------

async fn off_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!("\n## off — capture writes no I/O facts and reconstructs CaptureOff");
    reset(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    let run_id = "cap-off";
    seed_run(admin_url, run_id, "running", 0, CaptureMode::Off).await?;
    let captured = capture::derive(
        CaptureMode::Off,
        &json!({ "at": "a" }),
        &json!({ "at": "a" }),
    );
    write_success(&app, run_id, "a", 0, "main", &captured).await?;

    let row = app
        .query_one(
            "SELECT output_json IS NULL, input_json IS NULL, output_size IS NULL, \
                    payload_hash IS NULL FROM node_runs \
             WHERE run_id = $1 AND local_node_id = 'a'",
            &[&run_id],
        )
        .await?;
    let no_facts = row.get::<_, bool>(0)
        && row.get::<_, bool>(1)
        && row.get::<_, bool>(2)
        && row.get::<_, bool>(3);
    let verdict = reconstruct_verdict(&app, &linear_flow(), run_id).await?;
    let capture_off = matches!(verdict, Err(ReconstructError::CaptureOff { .. }));
    let pass = no_facts && capture_off;
    println!("PASS(off: no I/O facts + CaptureOff replay): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// output-too-large: an oversized output is NULL with size/hash retained
// ---------------------------------------------------------------------------

async fn output_too_large_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!("\n## output-too-large — an output over the fixed ceiling retains size/hash only");
    reset(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    let big = "x".repeat(capture::OUTPUT_CAPTURE_CEILING_BYTES);
    let output = json!({ "blob": big });
    let raw_len = output.to_string().len() as i64;
    let expected_hash = format!("{:016x}", capture::fnv1a64(output.to_string().as_bytes()));
    let c = capture::derive(CaptureMode::Full, &output, &json!({ "in": 1 }));

    seed_run(admin_url, "cap-big", "running", 0, CaptureMode::Full).await?;
    write_success(&app, "cap-big", "a", 0, "main", &c).await?;

    let row = app
        .query_one(
            "SELECT output_json IS NULL, input_json IS NOT NULL, output_size, payload_hash \
               FROM node_runs WHERE run_id = 'cap-big' AND local_node_id = 'a'",
            &[],
        )
        .await?;
    let out_null: bool = row.get(0);
    let input_present: bool = row.get(1);
    let size: Option<i64> = row.get(2);
    let hash: Option<String> = row.get(3);
    let size_ok = size == Some(raw_len);
    let hash_ok = hash.as_deref() == Some(expected_hash.as_str());
    let projected = capture::project_output(None, size, hash.clone())
        .map(|value| serde_json::to_value(value).expect("projection serializes"));
    let metadata_ok = projected
        == Some(json!({
            "kind": "output-too-large",
            "size": raw_len,
            "hash": expected_hash,
        }));

    let pass = out_null && input_present && size_ok && hash_ok && metadata_ok;
    println!(
        "  output_null={out_null} input_present={input_present} \
         size={size:?}=={raw_len} hash_ok={hash_ok} metadata_ok={metadata_ok}"
    );
    println!("PASS(output-too-large: oversized output NULL with size/hash metadata): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// redaction: full capture always scrubs stored node I/O
// ---------------------------------------------------------------------------

async fn redaction_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!(
        "\n## redaction — a known secret under full capture appears NOWHERE in node_runs \
         (f3proof-style containment)"
    );
    reset(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    seed_run(admin_url, "cap-scrub", "running", 0, CaptureMode::Full).await?;

    // The secret rides ONLY positions the v0 scrubber is designed to catch: a
    // secret-KEY value (`token`/`api_key`), a nested secret key, and a value-shape
    // (`Bearer `) prefix. (A secret buried in free text under an innocent key is a
    // known v0 gap — no content scanning — so seeding it there would honestly
    // fail; the gate proves the CATCHABLE cases are airtight everywhere.)
    let output = json!({ "token": SECRET, "auth": format!("Bearer {SECRET}") });
    let input = json!({ "api_key": SECRET, "nested": { "private_key": SECRET } });
    let cs = capture::derive(CaptureMode::Full, &output, &input);
    write_success(&app, "cap-scrub", "a", 0, "main", &cs).await?;

    // An error row: the secret rides the error payload AND the taxonomy detail
    // under secret keys, exercising the guest's error-path detail scrub.
    let err_payload = json!({ "error": { "token": SECRET, "code": "x" } });
    let err_detail = json!({ "message": "node failed", "code": "x", "data": { "secret": SECRET } });
    let ce = capture::derive(CaptureMode::Full, &err_payload, &input);
    write_error(&app, "cap-scrub", "b", 1, "terminal", err_detail, &ce).await?;

    // Containment scan (f3proof shape): concatenate every text-bearing column of
    // every node_runs row for the run and assert the raw secret is absent.
    let rows = app
        .query(
            "SELECT coalesce(output_json::text, '') || coalesce(input_json::text, '') || \
                    coalesce(payload_hash, '') || coalesce(error_detail::text, '') AS blob \
               FROM node_runs WHERE run_id = 'cap-scrub'",
            &[],
        )
        .await?;
    let mut leaked = false;
    let mut placeholder_seen = false;
    for row in &rows {
        let blob: String = row.get(0);
        if blob.contains(SECRET) {
            leaked = true;
        }
        if blob.contains(capture::REDACTED) {
            placeholder_seen = true;
        }
    }

    let pass = !leaked && placeholder_seen && rows.len() == 2;
    println!(
        "  rows={} leaked={leaked} placeholder_seen={placeholder_seen}",
        rows.len()
    );
    println!("PASS(redaction: raw secret nowhere in full-capture node rows): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// retention: the real prune verb removes old terminal runs, keeps the rest,
// ---------------------------------------------------------------------------

async fn run_exists(client: &Client, run_id: &str) -> anyhow::Result<bool> {
    Ok(client
        .query_one("SELECT count(*) FROM runs WHERE run_id = $1", &[&run_id])
        .await?
        .get::<_, i64>(0)
        == 1)
}

async fn retention_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!(
        "\n## retention — the real prune-run-history verb prunes old TERMINAL runs \
         (cascading node_runs), keeps recent + non-terminal"
    );
    reset(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    // Seed: an old completed run (with a node_run, to prove the cascade), a recent
    // completed run, and an OLD but RUNNING run (terminal-only guard).
    seed_run(admin_url, "old-done", "completed", 40, CaptureMode::Full).await?;
    let c = capture::derive(CaptureMode::Full, &json!({ "at": "a" }), &json!({}));
    write_success(&app, "old-done", "a", 0, "main", &c).await?;
    seed_run(admin_url, "recent-done", "completed", 1, CaptureMode::Off).await?;
    seed_run(admin_url, "old-running", "running", 40, CaptureMode::Off).await?;

    let prune = ctl_process::run_ops_checked([
        "prune-run-history",
        "--database-url",
        app_url,
        "--schema",
        SCHEMA,
        "--tenant",
        TENANT,
        "--retention-days",
        "30",
    ])
    .await
    .context("prune through wamn-ctl")?;
    let prune_stdout = String::from_utf8(prune.stdout).context("prune output is UTF-8")?;
    let reported_one = prune_stdout.contains("pruned 1 terminal run(s)");

    let old_gone = !run_exists(&app, "old-done").await?;
    let recent_kept = run_exists(&app, "recent-done").await?;
    let running_kept = run_exists(&app, "old-running").await?;
    let cascaded = app
        .query_one(
            "SELECT count(*) FROM node_runs WHERE run_id = 'old-done'",
            &[],
        )
        .await?
        .get::<_, i64>(0)
        == 0;
    let pass = reported_one && old_gone && recent_kept && running_kept && cascaded;
    println!(
        "  reported_one={reported_one} old_gone={old_gone} recent_kept={recent_kept} \
         running_kept={running_kept} node_runs_cascaded={cascaded}"
    );
    println!("PASS(retention: old terminal pruned + cascade, recent/running kept): {pass}");
    Ok(pass)
}

pub async fn run(args: CaptureBenchArgs) -> anyhow::Result<()> {
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "capturebench needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;

    println!("# wamn-host 9.6 capturebench (schema {SCHEMA}, tenant {TENANT})");
    provision(&admin_url)
        .await
        .context("provision ephemeral run-state schema")?;

    let run_all = args.mode == Mode::All;
    let mut pass = true;
    let outcome = async {
        if run_all || args.mode == Mode::Off {
            pass &= off_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::OutputTooLarge {
            pass &= output_too_large_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Redaction {
            pass &= redaction_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Retention {
            pass &= retention_phase(&app_url, &admin_url).await?;
        }
        anyhow::Ok(())
    }
    .await;

    let _ = teardown(&admin_url).await;
    outcome?;

    println!("\ncapturebench complete — overall PASS: {pass}");
    if !pass {
        bail!("one or more 9.6 capture gates failed");
    }
    Ok(())
}
