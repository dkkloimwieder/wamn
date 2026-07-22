//! The `capturebench` subcommand: the 9.6 node-level I/O capture gates (wamn-srb).
//!
//! Pure host-side like dispatchbench (no wasm guest): it applies the REAL
//! `deploy/sql/run-state.sql` into a throwaway ephemeral schema, then exercises
//! the SAME pure capture logic (`wamn_run_store::capture`) and the SAME `node_runs`
//! insert builders the flowrunner guest binds — so the columns a capture policy
//! produces, the reconstruction verdict, the secret-containment property, and the
//! retention verb all run against real Postgres over the real prepared statements
//! (SR12b), without standing up the wasm runtime.
//!
//! Modes:
//!   toggle    — an `off`/`preview` policy writes a row with NULL payloads and the
//!               right `capture_mode`, and the run reconstructs to CaptureOff
//!               (non-replayable) — the capture-off seam end to end.
//!   truncate  — a payload over the size threshold is stored PREVIEW-ONLY (payload
//!               NULL) with the correct head / full size / content hash, in a mode
//!               (`full`) that would otherwise store it faithfully.
//!   scrub     — a payload carrying a KNOWN secret is written through a `scrubbed`
//!               flow (success + error rows); a containment scan asserts the raw
//!               secret appears NOWHERE in `node_runs` and `redacted` is set.
//!   retention — old + recent terminal runs (plus a non-terminal run and a
//!               `cron_anchor` row) are seeded; the REAL `prune-run-history` verb
//!               logic prunes the old terminal run (cascading its node_runs), keeps
//!               the recent one and the non-terminal one, and leaves cron_anchor
//!               untouched (so a pruned cron tick cannot re-fire — wamn-fqg.6).
//!   all       — every mode in sequence.

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};

use wamn_flow::{Capture, CaptureMode, Flow};
use wamn_run_store::{NodeRunRecord, ReconstructError, RunRecord, capture, reconstruct};
use wamn_runner::Plan;

const SCHEMA: &str = "wamn_capture";
const TENANT: &str = "capture-t";
/// The known secret seeded through the scrub gate — asserted to appear NOWHERE.
const SECRET: &str = "hunter2-TOPSECRET-9f3c";

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Toggle,
    Truncate,
    Scrub,
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
    include_str!("../../../deploy/sql/run-state.sql").replace("wamn_run", SCHEMA)
}

async fn admin_exec(admin_url: &str, sql: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let r = client
        .batch_execute(sql)
        .await
        .map_err(|e| anyhow::anyhow!("admin exec: {e}"));
    drop(client);
    let _ = conn_task.await;
    r
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"),
    )
    .await?;
    admin_exec(admin_url, &run_state_ddl()).await
}

async fn teardown(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"),
    )
    .await
}

async fn reset(admin_url: &str) -> anyhow::Result<()> {
    admin_exec(
        admin_url,
        &format!("TRUNCATE {SCHEMA}.runs CASCADE; TRUNCATE {SCHEMA}.cron_anchor;"),
    )
    .await
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
    client: &Client,
    run_id: &str,
    status: &str,
    age_days: i64,
) -> anyhow::Result<()> {
    client
        .execute(
            "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status, created_at) \
             VALUES (current_setting('app.tenant', true), $1, 'f', 1, $2, \
                     now() - ($3::bigint * interval '1 day'))",
            &[&run_id, &status, &age_days],
        )
        .await
        .context("seed run")?;
    Ok(())
}

fn to_jsonb(s: &Option<String>) -> Option<Value> {
    s.as_deref()
        .map(|t| serde_json::from_str(t).expect("captured json re-parses"))
}

/// Write a completed `success` node-run via `insert_node_run_success_sql` with the
/// capture columns `capture::derive` produced — the exact 12-param bind the guest
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
            &wamn_run_store::sql::insert_node_run_success_sql(),
            &[
                &run_id,
                &node_id,
                &occ,
                &seq,
                &port,
                &out_j,
                &in_j,
                &c.preview_head,
                &c.payload_size,
                &c.payload_hash,
                &c.capture_mode,
                &c.redacted,
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
    if c.redacted {
        capture::scrub(&mut detail);
    }
    let out_j = to_jsonb(&c.output_json);
    let in_j = to_jsonb(&c.input_json);
    let occ: i32 = 0;
    client
        .execute(
            &wamn_run_store::sql::insert_node_run_error_sql(),
            &[
                &run_id,
                &node_id,
                &occ,
                &seq,
                &out_j,
                &in_j,
                &kind,
                &detail,
                &c.preview_head,
                &c.payload_size,
                &c.payload_hash,
                &c.capture_mode,
                &c.redacted,
            ],
        )
        .await
        .context("write error node_run")?;
    Ok(())
}

/// A minimal linear flow `a -> b` with the given capture policy — the reconstruct
/// source for the toggle/truncate phases.
fn linear_flow(capture: Value) -> Flow {
    let mut graph = json!({
        "schema-version": "0.1", "flow-id": "cap", "version": 1,
        "trigger": {"type": "manual"}, "entry": "a",
        "nodes": [{"id": "a", "type": "echo"}, {"id": "b", "type": "echo"}],
        "edges": [{"from": "a", "to": "b"}],
    });
    if !capture.is_null() {
        graph["capture"] = capture;
    }
    Flow::from_json(&graph.to_string()).expect("capture fixture flow parses")
}

/// Read a run's completed node-runs back and fold them through reconstruction —
/// the driver's exact resume path, so a NULL `output_json` (capture off) surfaces
/// as CaptureOff.
fn load_node_runs(rows: &[tokio_postgres::Row], run_id: &str) -> Vec<NodeRunRecord> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let node_id: String = row.get(0);
        let occurrence: i32 = row.get(1);
        let seq: i32 = row.get(2);
        let port: Option<String> = row.get(3);
        let output_text: Option<String> = row.get(4);
        // SQL NULL output => None => CaptureOff, exactly as the guest maps it.
        let output = output_text.map(|s| serde_json::from_str::<Value>(&s).expect("output json"));
        let mut rec = NodeRunRecord::success(
            run_id,
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
            &wamn_run_store::sql::select_completed_node_runs_sql(),
            &[&run_id],
        )
        .await
        .context("read completed node_runs")?;
    let node_runs = load_node_runs(&rows, run_id);
    let plan = Plan::compile(flow).map_err(|e| anyhow::anyhow!("compile: {e}"))?;
    let run = RunRecord::new(run_id, "cap", 1, json!({ "trig": 1 }));
    Ok(reconstruct(&plan, &run, &node_runs).map(|_| ()))
}

// ---------------------------------------------------------------------------
// toggle: off/preview => NULL payloads + capture_mode + CaptureOff replay
// ---------------------------------------------------------------------------

async fn toggle_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!("\n## toggle — off/preview capture writes NULL payloads and reconstructs CaptureOff");
    reset(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    let mut pass = true;
    for (mode, run_id) in [
        (CaptureMode::Off, "cap-off"),
        (CaptureMode::Preview, "cap-prev"),
    ] {
        seed_run(&app, run_id, "running", 0).await?;
        let policy = Capture {
            mode,
            ..Capture::default()
        };
        let c = capture::derive(&policy, &json!({ "at": "a" }), &json!({ "at": "a" }));
        write_success(&app, run_id, "a", 0, "main", &c).await?;

        // The stored row: output_json NULL, capture_mode the effective literal.
        let row = app
            .query_one(
                "SELECT output_json IS NULL, capture_mode FROM node_runs \
                 WHERE run_id = $1 AND node_id = 'a'",
                &[&run_id],
            )
            .await?;
        let out_null: bool = row.get(0);
        let recorded_mode: Option<String> = row.get(1);
        let mode_ok = recorded_mode.as_deref() == Some(mode.as_str());

        // Reconstruction: a completed row with no captured output => CaptureOff.
        let flow = linear_flow(json!({ "mode": mode.as_str() }));
        let verdict = reconstruct_verdict(&app, &flow, run_id).await?;
        let capture_off = matches!(verdict, Err(ReconstructError::CaptureOff { .. }));

        let ok = out_null && mode_ok && capture_off;
        pass &= ok;
        println!(
            "  {}: output_null={out_null} capture_mode={recorded_mode:?} CaptureOff={capture_off} -> {ok}",
            mode.as_str()
        );
    }
    println!("PASS(toggle: NULL payloads + capture_mode + CaptureOff replay): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// truncate: an oversized payload is stored preview-only (head/size/hash)
// ---------------------------------------------------------------------------

async fn truncate_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!("\n## truncate — a payload over the size threshold is stored preview-only");
    reset(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    // A `full` policy with a tiny threshold: the big payload would store faithfully
    // but for its size, so this isolates the truncation.
    let big = "x".repeat(4096);
    let output = json!({ "blob": big });
    let raw_len = output.to_string().len() as i64;
    let expected_hash = format!("{:016x}", capture::fnv1a64(output.to_string().as_bytes()));
    let policy = Capture {
        mode: CaptureMode::Full,
        max_bytes: 64,
    };
    let c = capture::derive(&policy, &output, &json!({ "in": 1 }));

    seed_run(&app, "cap-big", "running", 0).await?;
    write_success(&app, "cap-big", "a", 0, "main", &c).await?;

    let row = app
        .query_one(
            "SELECT output_json IS NULL, input_json IS NULL, preview_head, payload_size, \
                    payload_hash, capture_mode, redacted \
               FROM node_runs WHERE run_id = 'cap-big' AND node_id = 'a'",
            &[],
        )
        .await?;
    let out_null: bool = row.get(0);
    let in_null: bool = row.get(1);
    let preview: Option<String> = row.get(2);
    let size: Option<i64> = row.get(3);
    let hash: Option<String> = row.get(4);
    let mode: Option<String> = row.get(5);
    let redacted: bool = row.get(6);

    let preview_present = preview.as_deref().is_some_and(|p| !p.is_empty());
    let size_ok = size == Some(raw_len);
    let hash_ok = hash.as_deref() == Some(expected_hash.as_str());
    let mode_ok = mode.as_deref() == Some("preview");

    let pass = out_null && in_null && preview_present && size_ok && hash_ok && mode_ok && !redacted;
    println!(
        "  output_null={out_null} input_null={in_null} preview={preview_present} \
         size={size:?}=={raw_len} hash_ok={hash_ok} mode={mode:?} redacted={redacted}"
    );
    println!("PASS(truncate: oversized payload preview-only with head/size/hash): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// scrub: a known secret through a `scrubbed` flow appears NOWHERE in node_runs
// ---------------------------------------------------------------------------

async fn scrub_phase(app_url: &str, admin_url: &str) -> anyhow::Result<bool> {
    println!(
        "\n## scrub — a known secret through a `scrubbed` flow appears NOWHERE in node_runs \
         (f3proof-style containment)"
    );
    reset(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    let policy = Capture {
        mode: CaptureMode::Scrubbed,
        ..Capture::default()
    };
    seed_run(&app, "cap-scrub", "running", 0).await?;

    // The secret rides ONLY positions the v0 scrubber is designed to catch: a
    // secret-KEY value (`token`/`api_key`), a nested secret key, and a value-shape
    // (`Bearer `) prefix. (A secret buried in free text under an innocent key is a
    // known v0 gap — no content scanning — so seeding it there would honestly
    // fail; the gate proves the CATCHABLE cases are airtight everywhere.)
    let output = json!({ "token": SECRET, "auth": format!("Bearer {SECRET}") });
    let input = json!({ "api_key": SECRET, "nested": { "private_key": SECRET } });
    let cs = capture::derive(&policy, &output, &input);
    write_success(&app, "cap-scrub", "a", 0, "main", &cs).await?;

    // An error row: the secret rides the error payload AND the taxonomy detail
    // under secret keys, exercising the guest's error-path detail scrub.
    let err_payload = json!({ "error": { "token": SECRET, "code": "x" } });
    let err_detail = json!({ "message": "node failed", "code": "x", "data": { "secret": SECRET } });
    let ce = capture::derive(&policy, &err_payload, &input);
    write_error(&app, "cap-scrub", "b", 1, "terminal", err_detail, &ce).await?;

    // Containment scan (f3proof shape): concatenate every text-bearing column of
    // every node_runs row for the run and assert the raw secret is absent.
    let rows = app
        .query(
            "SELECT coalesce(output_json::text, '') || coalesce(input_json::text, '') || \
                    coalesce(preview_head, '') || coalesce(payload_hash, '') || \
                    coalesce(error_detail::text, '') AS blob, redacted \
               FROM node_runs WHERE run_id = 'cap-scrub'",
            &[],
        )
        .await?;
    let mut leaked = false;
    let mut all_redacted = !rows.is_empty();
    let mut placeholder_seen = false;
    for row in &rows {
        let blob: String = row.get(0);
        let redacted: bool = row.get(1);
        if blob.contains(SECRET) {
            leaked = true;
        }
        if blob.contains(capture::REDACTED) {
            placeholder_seen = true;
        }
        all_redacted &= redacted;
    }

    let pass = !leaked && all_redacted && placeholder_seen && rows.len() == 2;
    println!(
        "  rows={} leaked={leaked} all_redacted={all_redacted} placeholder_seen={placeholder_seen}",
        rows.len()
    );
    println!("PASS(scrub: raw secret nowhere in node_runs + redacted set): {pass}");
    Ok(pass)
}

// ---------------------------------------------------------------------------
// retention: the real prune verb removes old terminal runs, keeps the rest,
// leaves cron_anchor untouched
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
         (cascading node_runs), keeps recent + non-terminal, leaves cron_anchor"
    );
    reset(admin_url).await?;
    let (app, _h) = connect_app(app_url).await?;

    // Seed: an old completed run (with a node_run, to prove the cascade), a recent
    // completed run, and an OLD but RUNNING run (terminal-only guard).
    seed_run(&app, "old-done", "completed", 40).await?;
    let c = capture::derive(&Capture::default(), &json!({ "at": "a" }), &json!({}));
    write_success(&app, "old-done", "a", 0, "main", &c).await?;
    seed_run(&app, "recent-done", "completed", 1).await?;
    seed_run(&app, "old-running", "running", 40).await?;

    // A durable cron anchor — pruning runs must NOT touch it (wamn-fqg.6).
    app.execute(
        "INSERT INTO cron_anchor (tenant_id, flow_id, last_tick) \
         VALUES (current_setting('app.tenant', true), 'f', 123456)",
        &[],
    )
    .await?;

    // Run the REAL verb logic (a fresh app connection, since prune re-pins the
    // session GUCs itself and needs &mut).
    let (mut prune_client, conn) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("prune app connect")?;
    let conn_task = tokio::spawn(conn);
    let pruned =
        wamn_ctl::prune_run_history::prune(&mut prune_client, SCHEMA, TENANT, 30, true).await;
    drop(prune_client);
    let _ = conn_task.await;
    let pruned = pruned?;

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
    let anchor_survived = app
        .query_one("SELECT count(*) FROM cron_anchor WHERE flow_id = 'f'", &[])
        .await?
        .get::<_, i64>(0)
        == 1;

    let pass =
        pruned == 1 && old_gone && recent_kept && running_kept && cascaded && anchor_survived;
    println!(
        "  pruned={pruned} old_gone={old_gone} recent_kept={recent_kept} \
         running_kept={running_kept} node_runs_cascaded={cascaded} anchor_survived={anchor_survived}"
    );
    println!("PASS(retention: old terminal pruned + cascade, recent/running/anchor kept): {pass}");
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
        if run_all || args.mode == Mode::Toggle {
            pass &= toggle_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Truncate {
            pass &= truncate_phase(&app_url, &admin_url).await?;
        }
        if run_all || args.mode == Mode::Scrub {
            pass &= scrub_phase(&app_url, &admin_url).await?;
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
