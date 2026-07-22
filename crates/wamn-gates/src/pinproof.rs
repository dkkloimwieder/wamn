//! pinproof — the 11.3 "record-and-replay fixtures" gate (wamn-htn).
//!
//! The in-cluster gate-of-record candidate for pinning a recorded run as a test
//! case. It runs the whole arc against a throwaway Postgres (`WAMN_PG_ADMIN_URL`
//! superuser to provision, `WAMN_PG_URL` app role for the RLS-scoped reads/writes)
//! in an EPHEMERAL schema it owns end to end:
//!
//!   1. provision the run-plane + `flow-tests` tables through the SAME `ensure_*`
//!      path production provisioning uses; register a flow v1;
//!   2. seed a real terminal run under a `full` capture policy whose payloads
//!      carry a raw SECRET (a `token` key + a `Bearer ` value) AND volatile fields
//!      (a UUID + an RFC-3339 timestamp) — the exact shape 9.6 stores;
//!   3. pin it via the REAL `wamn_ctl::pin_run::pin(...)` core, then assert the
//!      stored `test_cases.case_body`: (a) contains NO secret substring (scrubbed
//!      on pin even from a FULL run), (b) parses as a `wamn_testkit::TestCase`,
//!      (c) carries `normalize` with `canonicalize` on;
//!   4. REPLAY round-trip: rebuild a `Captured` from the recorded facts →
//!      `evaluate` PASSES;
//!   5. mutate a VOLATILE field in the replay `Captured` → still PASSES;
//!   6. mutate a REAL field → FAILS;
//!   7. REFUSAL: a `preview`/`off` run (NULL terminal output) → `pin(...)` returns
//!      the typed `NotCaptured` error and writes nothing.
//!
//! Self-contained: it provisions a fresh schema and drops it at the end.

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};

use wamn_ctl::pin_run::{self, PinResult};
use wamn_ctl::publish_catalog::{ensure_flow_registry, ensure_flow_tests, ensure_runstate};
use wamn_flow::{Capture, CaptureMode};
use wamn_gate_harness::{check, scope_session, seed_flow_version};
use wamn_run_store::capture;
use wamn_testkit::{Assertion, Captured, PinError, RunFacts, RunStatus, TestCase, evaluate};

const FLOW_ID: &str = "pinned-flow";
/// The raw secret seeded through the FULL-capture run — asserted absent from the
/// pinned case (scrubbed on pin).
const SECRET: &str = "hunter2-TOPSECRET-9f3c";
/// A server-minted id + timestamp in the node output — volatile fields the pin
/// canonicalizes so replay tolerates a fresh value.
const RUN_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
const RUN_AT: &str = "2026-07-22T06:59:00Z";

#[derive(Debug, Args)]
pub struct PinProofArgs {
    /// App (wamn_app, NOSUPERUSER) Postgres URL. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL — provisions the ephemeral schema + run-plane/test tables.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The ephemeral schema this gate owns (dropped at the end).
    #[arg(long, default_value = "wamn_pinproof")]
    pub schema: String,

    /// The owning tenant the run + suite are seeded under.
    #[arg(long, default_value = "pinproof-tenant")]
    pub tenant: String,

    /// Keep the schema at the end (default drops it).
    #[arg(long)]
    pub keep: bool,
}

/// The node output the FULL-capture run records: a real field, a secret (key +
/// value shape), and two volatile fields.
fn seeded_output() -> Value {
    json!({
        "result": "accepted",
        "token": SECRET,
        "auth": format!("Bearer {SECRET}"),
        "run_uuid": RUN_UUID,
        "at": RUN_AT,
    })
}

/// The trigger input the run records — carries a secret under a secret key, to
/// prove the trigger is scrubbed at pin too.
fn seeded_input() -> Value {
    json!({ "trigger": "go", "api_key": SECRET })
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .context("postgres connect")?;
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok((client, task))
}

pub async fn run(args: PinProofArgs) -> anyhow::Result<()> {
    if !is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    println!(
        "# wamn-gates pinproof — 11.3 record-and-replay fixtures (schema {}, tenant {})",
        args.schema, args.tenant
    );

    // --- provision (superuser) through the production ensure_* path ---
    let (admin, admin_task) = connect(&admin_url).await?;
    provision(&admin, &args.schema).await?;

    let mut ok = true;

    // --- seed flow v1 + a FULL-capture terminal run (app role) ---
    let (app, app_task) = connect(&app_url).await?;
    scope_session(&app, &args.tenant, &args.schema).await?;
    seed_flow_version(&app, &args.tenant, FLOW_ID, 1, true, "{}", true)
        .await
        .context("register flow v1")?;

    seed_completed_run(&app, "pin-full", &seeded_input()).await?;
    let full = capture::derive(&full_policy(), &seeded_output(), &seeded_input());
    write_success(&app, "pin-full", "final", 0, "main", &full).await?;
    // The stored node_runs row DOES hold the raw secret (a full-capture run) — the
    // pin must scrub it, not rely on the store having done so.
    let stored_has_secret: bool = app
        .query_one(
            "SELECT output_json::text LIKE '%' || $1 || '%' FROM node_runs \
             WHERE run_id = 'pin-full' AND node_id = 'final'",
            &[&SECRET],
        )
        .await?
        .get(0);
    check(
        &mut ok,
        "SEED: the full-capture node_runs row holds the raw secret (pin must scrub)",
        stored_has_secret,
    );

    // --- pin via the REAL ctl core; assert Pinned ---
    let result = pin_run::pin(
        &app,
        &args.schema,
        &args.tenant,
        "pin-full",
        "pinned",
        "from-run",
        0,
        vec![],
    )
    .await
    .context("pin the full-capture run")?;
    check(
        &mut ok,
        "PIN: pin(...) pinned the full-capture run",
        matches!(result, PinResult::Pinned { .. }),
    );

    // --- assert the stored case body ---
    let stored: Value = app
        .query_one(
            "SELECT case_body FROM test_cases WHERE case_id = 'from-run'",
            &[],
        )
        .await
        .context("read pinned case body")?
        .get(0);
    let body_text = stored.to_string();
    check(
        &mut ok,
        "SCRUB: the pinned case body contains NO raw secret (scrubbed on pin from a FULL run)",
        !body_text.contains(SECRET),
    );
    let case: Result<TestCase, _> = serde_json::from_value(stored.clone());
    check(
        &mut ok,
        "STORE: the pinned case body parses as a wamn-testkit TestCase",
        case.is_ok(),
    );
    let case = case.context("pinned case must parse")?;
    check(
        &mut ok,
        "NORMALIZE: the pinned case carries normalize with canonicalize on",
        case.normalize.as_ref().is_some_and(|n| n.canonicalize),
    );

    // The reconstruction-relevant node output the case pinned (scrubbed) — the
    // seed of the replay Captured.
    let base_output = case
        .expect
        .iter()
        .find_map(|a| match a {
            Assertion::Equals(v) => Some(v.clone()),
            _ => None,
        })
        .context("pinned case must carry an Equals over the terminal node output")?;
    check(
        &mut ok,
        "SCRUB: the pinned Equals redacts the secret token but keeps the real field",
        base_output["token"] == json!("[redacted]") && base_output["result"] == json!("accepted"),
    );

    // --- REPLAY round-trip: rebuild Captured, evaluate PASSES ---
    let faithful = evaluate(&case, &replay_captured(base_output.clone()));
    check(
        &mut ok,
        "REPLAY: the pinned case passes against the recorded facts (pure round-trip)",
        faithful.passed(),
    );

    // --- mutate a VOLATILE field: still PASSES ---
    let mut volatile = base_output.clone();
    volatile["run_uuid"] = json!("11111111-2222-3333-4444-555555555555");
    volatile["at"] = json!("2020-01-01T00:00:00Z");
    let vol = evaluate(&case, &replay_captured(volatile));
    check(
        &mut ok,
        "REPLAY: a mutated VOLATILE field (uuid/timestamp) still passes (canonicalized)",
        vol.passed(),
    );

    // --- mutate a REAL field: FAILS ---
    let mut real = base_output;
    real["result"] = json!("rejected");
    let re = evaluate(&case, &replay_captured(real));
    check(
        &mut ok,
        "REPLAY: a mutated REAL field fails (the regression is caught)",
        !re.passed(),
    );

    // --- REFUSAL: a preview/off run is not pinnable, writes nothing ---
    seed_completed_run(&app, "pin-preview", &json!({ "trigger": "go" })).await?;
    let preview = capture::derive(
        &Capture {
            mode: CaptureMode::Preview,
            ..Capture::default()
        },
        &seeded_output(),
        &json!({ "in": 1 }),
    );
    write_success(&app, "pin-preview", "final", 0, "main", &preview).await?;
    let refused = pin_run::pin(
        &app,
        &args.schema,
        &args.tenant,
        "pin-preview",
        "pinned",
        "from-preview",
        1,
        vec![],
    )
    .await
    .context("pin the preview run")?;
    check(
        &mut ok,
        "REFUSE: pinning a preview/off run returns the typed NotCaptured error",
        matches!(refused, PinResult::Refused(PinError::NotCaptured { .. })),
    );
    let leaked: i64 = app
        .query_one(
            "SELECT count(*) FROM test_cases WHERE case_id = 'from-preview'",
            &[],
        )
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("REFUSE: nothing was written for the refused run (got {leaked} case rows)"),
        leaked == 0,
    );

    drop(app);
    let _ = app_task.await;

    // --- teardown ---
    if !args.keep {
        admin
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", args.schema))
            .await
            .context("drop ephemeral schema")?;
    }
    drop(admin);
    let _ = admin_task.await;

    println!("\npinproof complete — overall PASS: {ok}");
    if !ok {
        bail!("pinproof failed");
    }
    Ok(())
}

/// The default `full` capture policy (faithful, replayable stored payloads).
fn full_policy() -> Capture {
    Capture {
        mode: CaptureMode::Full,
        ..Capture::default()
    }
}

/// Rebuild the fact bundle a completed run replays to: the terminal node output
/// plus the run's terminal outcome. This is the PURE round-trip — no host, no
/// wasm (11.3 replay is a fixture round-trip, not the parked suite runner).
fn replay_captured(node_output: Value) -> Captured {
    Captured {
        run: Some(RunFacts {
            status: RunStatus::Completed,
            fail_kind: None,
            fail_node: None,
        }),
        node_output: Some(node_output),
        ..Default::default()
    }
}

/// Fresh ephemeral schema + the run-plane / flow-test tables via the SAME
/// `ensure_*` functions `publish-catalog --runstate` uses (production path).
async fn provision(admin: &Client, schema: &str) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
             END IF; END $$;"
        ))
        .await
        .context("reset schema + ensure wamn_app role")?;
    // run-state creates the schema; flow-tests FKs into flows, so ORDER matters.
    ensure_runstate(admin, schema)
        .await
        .context("ensure run-state")?;
    ensure_flow_registry(admin, schema)
        .await
        .context("ensure flow registry")?;
    ensure_flow_tests(admin, schema)
        .await
        .context("ensure flow-test tables")?;
    println!("## provisioned schema {schema} (run-state + flows + test_suites/test_cases)");
    Ok(())
}

/// Insert a completed `runs` row carrying `input` as its trigger. jsonb via
/// `::text::jsonb` (the app-role fixture convention).
async fn seed_completed_run(app: &Client, run_id: &str, input: &Value) -> anyhow::Result<()> {
    app.execute(
        "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status, input_json) \
         VALUES (current_setting('app.tenant', true), $1, $2, 1, 'completed', $3::text::jsonb)",
        &[&run_id, &FLOW_ID, &input.to_string()],
    )
    .await
    .context("seed completed run")?;
    Ok(())
}

fn to_jsonb(s: &Option<String>) -> Option<Value> {
    s.as_deref()
        .map(|t| serde_json::from_str(t).expect("captured json re-parses"))
}

/// Write a completed `success` node-run via `insert_node_run_success_sql` with the
/// capture columns `capture::derive` produced — the exact 12-param bind the guest
/// makes (mirrors capturebench).
async fn write_success(
    app: &Client,
    run_id: &str,
    node_id: &str,
    seq: i32,
    port: &str,
    c: &capture::Captured,
) -> anyhow::Result<()> {
    let out_j = to_jsonb(&c.output_json);
    let in_j = to_jsonb(&c.input_json);
    let occ: i32 = 0;
    app.execute(
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

/// A bare lowercase SQL identifier (the ephemeral schema is interpolated).
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_ident_rejects_injection() {
        assert!(is_bare_ident("wamn_pinproof"));
        assert!(!is_bare_ident("a; DROP"));
        assert!(!is_bare_ident("Cap"));
    }

    /// The gate's seeded output carries a secret AND volatile fields — a broken
    /// fixture fails here, not only against a live Postgres.
    #[test]
    fn seeded_fixtures_carry_secret_and_volatile_fields() {
        let out = seeded_output();
        assert_eq!(out["token"], json!(SECRET));
        assert_eq!(out["run_uuid"], json!(RUN_UUID));
        assert_eq!(out["result"], json!("accepted"));
        assert_eq!(seeded_input()["api_key"], json!(SECRET));
    }
}
