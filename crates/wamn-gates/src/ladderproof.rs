//! ladderproof — the execution-ladder conformance proof (wamn-ojm).
//!
//! Rung 1 (wamn-ojm.1): prove the simplest flow executes CORRECTLY on the LIVE
//! runner, OUTSIDE a bench harness. Unlike `runnerbench` (which instantiates the
//! flowrunner IN-PROC via [`wamn_host::run_worker::RunWorker`] and drives the
//! claim loop itself), `ladderproof` is a pure DB CLIENT — the f1proof/apiproof
//! shape: it seeds ONE run the dispatcher way (write-ahead `dispatched` row +
//! queue row) and then WAITS for a SEPARATELY-DEPLOYED `run-worker` service
//! (deploy/runner.yaml) to claim it, drive it, and record the result. It asserts
//! nothing about how the run was driven — only that the deployed runner produced
//! the correct terminal state.
//!
//! The rung-1 flow is `webhook-in -> respond` (deploy/ladder/rung1.flow.json), a
//! `manual`-trigger passthrough: the deployed runner claims the seeded run, the
//! two nodes dispatch, and the run completes echoing its input. Assertions:
//!   * the run reaches `completed` (the deployed runner drove it to terminal);
//!   * `result_json` echoes the seeded input verbatim (both nodes are passthrough);
//!   * exactly two `node_runs` rows — `in` then `out`, both `success` on the
//!     `main` port, each output echoing the input (the per-node execution trace);
//!   * `trigger_source` is `manual` (the seed, not a dispatcher path).
//!
//! `--setup` provisions a fresh ephemeral schema + registers the flow (the
//! LOCAL self-contained path, run once before the mutation loop). Without it,
//! ladderproof is a client against a schema the deploy pipeline already
//! provisioned — so re-runs never drop the schema out from under the live runner.
//! The proof is parameterised for reuse: ojm.2 (multi-node linear) and ojm.3
//! (branching) add cases over the same seed/poll/assert client.

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};

use wamn_gate_harness::{check, scope_session, seed_flow_version};
use wamn_run_queue::{enqueue_sql, write_ahead_triggered_run_sql};

/// The committed rung-1 flow fixture (single source of truth; the drift-guard
/// test pins that the file parses to this manual passthrough flow).
const FLOW_JSON: &str = include_str!("../../../deploy/ladder/rung1.flow.json");
/// The flow id embedded in the fixture (equals the graph's `flow-id`).
const FLOW_ID: &str = "ladder-rung1";

#[derive(Debug, Args)]
pub struct LadderProofArgs {
    /// App (wamn_app, NOSUPERUSER) Postgres URL — seeds the run + reads the
    /// result. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL — required only for --setup / --teardown (provisioning is a
    /// privileged op; wamn_app is NOSUPERUSER/NOCREATEDB like production).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The demo schema the deployed runner claims from (matches the runner's
    /// --schema).
    #[arg(long, default_value = "wamn_runner_demo")]
    pub schema: String,

    /// The tenant the seeded run + the runner share (matches the runner's
    /// --tenant).
    #[arg(long, default_value = "demo-tenant")]
    pub tenant: String,

    /// Provision a fresh ephemeral schema (admin) + register the manual flow
    /// (app) before seeding — the LOCAL self-contained path. Omit it in-cluster,
    /// where the deploy pipeline provisions the schema and the runner is live.
    #[arg(long)]
    pub setup: bool,

    /// Drop the schema at the end (admin) — LOCAL cleanup only.
    #[arg(long)]
    pub teardown: bool,

    /// How long to wait for the deployed runner to drive the seeded run. Covers
    /// the runner's max idle poll interval (a directly-seeded run gets no
    /// doorbell) plus the drive.
    #[arg(long, default_value_t = 45)]
    pub timeout_secs: u64,
}

/// The seeded trigger input. A nested object proves the whole payload flows
/// through both passthrough nodes and back out as the run result — not just
/// "a row exists".
fn demo_input() -> Value {
    json!({ "msg": "hello-ladder", "nested": { "n": 42 } })
}

fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The flow tables the runner walks + the 5.14 `run_queue` it claims from, in the
/// house tenant floor (the runnerbench shape, minus the pg-write `sink` — the
/// rung-1 flow has no pg-write node).
fn ladder_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.flows (\
            tenant_id text NOT NULL, flow_id text NOT NULL, version int NOT NULL, \
            active boolean NOT NULL DEFAULT false, graph_json jsonb NOT NULL, \
            PRIMARY KEY (tenant_id, flow_id, version));\
         ALTER TABLE {schema}.flows ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.flows FORCE ROW LEVEL SECURITY;\
         CREATE POLICY flows_tenant ON {schema}.flows \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.flows TO wamn_app;\
         CREATE TABLE {schema}.runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, \
            status text NOT NULL DEFAULT 'running' \
              CHECK (status IN ('dispatched','running','completed','failed','cancelled','infrastructure-failure')), \
            trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
            updated_at timestamptz NOT NULL DEFAULT now(), \
            idempotency_key text, replay_of text, root_run_id text, \
            fail_kind text, fail_node text, fail_reason text, \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.node_runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
            occurrence int NOT NULL DEFAULT 0, seq int NOT NULL, attempt int NOT NULL DEFAULT 0, \
            status text NOT NULL, output_port text, output_json jsonb, input_json jsonb, \
            error_kind text, error_detail jsonb, resume_at timestamptz, \
            PRIMARY KEY (tenant_id, run_id, node_id, occurrence), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         ALTER TABLE {schema}.node_runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.node_runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY node_runs_tenant ON {schema}.node_runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.node_runs TO wamn_app;\
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

/// A wamn_app connection pinned to the demo schema + tenant claim — the same RLS
/// floor + search_path the deployed runner's plugin session runs under, so the
/// seeder and the runner see each other's rows.
async fn connect_app(app_url: &str, schema: &str, tenant: &str) -> anyhow::Result<Client> {
    let (client, conn) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    scope_session(&client, tenant, schema)
        .await
        .context("set search_path + tenant claim")?;
    Ok(client)
}

/// Provision a fresh ephemeral schema + the flow tables (superuser), then
/// register the rung-1 flow active (app, under the tenant claim). The LOCAL
/// self-contained bring-up; in-cluster the deploy pipeline provisions instead.
async fn setup(admin_url: &str, app_url: &str, schema: &str, tenant: &str) -> anyhow::Result<()> {
    let (admin, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for --setup")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        // Ensure the non-superuser runtime role exists (as in production).
        admin
            .batch_execute(
                "DO $$ BEGIN \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                     CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
                   END IF; \
                 END $$;",
            )
            .await
            .context("ensure wamn_app role")?;
        admin
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; \
                 CREATE SCHEMA {schema} AUTHORIZATION postgres; \
                 GRANT USAGE ON SCHEMA {schema} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        admin
            .batch_execute(&ladder_ddl(schema))
            .await
            .context("apply ladder DDL")?;
        anyhow::Ok(())
    }
    .await;
    drop(admin);
    let _ = conn_task.await;
    result?;

    // Register the flow via the app role, so the same RLS floor the runner reads
    // under is exercised at registration.
    let app = connect_app(app_url, schema, tenant).await?;
    seed_flow_version(&app, tenant, FLOW_ID, 1, true, FLOW_JSON, true)
        .await
        .context("register rung-1 flow")?;
    Ok(())
}

async fn teardown(admin_url: &str, schema: &str) -> anyhow::Result<()> {
    let (admin, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = admin
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))
        .await
        .map_err(|e| anyhow::anyhow!("drop ephemeral schema: {e}"));
    drop(admin);
    let _ = conn_task.await;
    r.map(|_| ())
}

/// Seed ONE run the way the dispatcher does — the write-ahead `dispatched` row +
/// the queue row, co-transacted (the exact producer state the runner claims).
async fn seed_run(client: &mut Client, run_id: &str, input_text: &str) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &FLOW_ID, &1i32, &"manual", &input_text],
    )
    .await
    .context("write-ahead run")?;
    tx.execute(
        &enqueue_sql(),
        &[&run_id, &Option::<&str>::None, &0i32, &0i64],
    )
    .await
    .context("enqueue run")?;
    tx.commit().await?;
    Ok(())
}

fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "infrastructure-failure"
    )
}

pub async fn run(args: LadderProofArgs) -> anyhow::Result<()> {
    if !valid_ident(&args.schema) {
        bail!("invalid schema {:?}", args.schema);
    }
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;

    println!(
        "# wamn-gates ladderproof rung 1 — deployed-runner conformance (schema {}, tenant {}, flow {FLOW_ID})",
        args.schema, args.tenant
    );

    if args.setup {
        let admin_url = args.admin_database_url.clone().context(
            "--setup needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
        )?;
        setup(&admin_url, &app_url, &args.schema, &args.tenant)
            .await
            .context("setup: provision schema + register flow")?;
        println!("## setup — provisioned schema + registered {FLOW_ID} (active)");
    }

    let input = demo_input();
    let input_text = serde_json::to_string(&input)?;

    // A unique run id per invocation so re-runs never collide (the mutation loop
    // re-runs the client many times against the same live schema + runner).
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let run_id = format!("ladder-{nanos}");

    let mut client = connect_app(&app_url, &args.schema, &args.tenant).await?;
    seed_run(&mut client, &run_id, &input_text).await?;
    println!(
        "## seed — one manual run {run_id} written-ahead + enqueued; awaiting the deployed runner"
    );

    // Poll the run to terminal — the deployed runner claims + drives it. A
    // directly-seeded run gets no doorbell, so the wait covers the runner's idle
    // poll interval.
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let mut status = "dispatched".to_string();
    loop {
        let row = client
            .query_opt("SELECT status FROM runs WHERE run_id = $1", &[&run_id])
            .await?;
        if let Some(row) = row {
            status = row.get(0);
            if is_terminal(&status) {
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let outcome = assert_run(&client, &run_id, &input, &status).await?;

    if args.teardown
        && let Some(admin_url) = args.admin_database_url.clone()
    {
        let _ = teardown(&admin_url, &args.schema).await;
    }

    println!("\nladderproof complete — overall PASS: {outcome}");
    if !outcome {
        bail!("ladderproof rung 1 failed");
    }
    Ok(())
}

/// Assert the deployed runner drove the seeded run correctly: terminal state,
/// echoed result, and the two-node execution trace.
async fn assert_run(
    client: &Client,
    run_id: &str,
    input: &Value,
    final_status: &str,
) -> anyhow::Result<bool> {
    println!("## assert — the deployed runner drove the run correctly");
    let mut ok = true;

    check(
        &mut ok,
        &format!("run reached completed (status = {final_status})"),
        final_status == "completed",
    );

    let run = client
        .query_one(
            "SELECT result_json::text, trigger_source FROM runs WHERE run_id = $1",
            &[&run_id],
        )
        .await?;
    let result_text: Option<String> = run.get(0);
    let trigger: Option<String> = run.get(1);
    let result_val = result_text
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    check(
        &mut ok,
        "result_json echoes the seeded input verbatim",
        result_val.as_ref() == Some(input),
    );
    check(
        &mut ok,
        "trigger_source recorded as manual",
        trigger.as_deref() == Some("manual"),
    );

    let rows = client
        .query(
            "SELECT node_id, seq, status, output_port, output_json::text \
             FROM node_runs WHERE run_id = $1 ORDER BY seq",
            &[&run_id],
        )
        .await?;
    check(
        &mut ok,
        &format!("two node_runs recorded (in, out) — got {}", rows.len()),
        rows.len() == 2,
    );
    // The per-node execution trace: the single meaningful node (in) then the
    // terminal (out), both succeeded on the main port echoing the input.
    let expect = [("in", 0i32), ("out", 1i32)];
    if rows.len() == 2 {
        for (row, (want_id, want_seq)) in rows.iter().zip(expect) {
            let node_id: String = row.get(0);
            let seq: i32 = row.get(1);
            let status: String = row.get(2);
            let port: Option<String> = row.get(3);
            let output = row
                .get::<_, Option<String>>(4)
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            check(
                &mut ok,
                &format!("node_run {want_id}: id/seq/status/port"),
                node_id == want_id
                    && seq == want_seq
                    && status == "success"
                    && port.as_deref() == Some("main"),
            );
            check(
                &mut ok,
                &format!("node_run {want_id}: output echoes the input"),
                output.as_ref() == Some(input),
            );
        }
    }

    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed fixture parses to the manual passthrough flow the proof
    /// asserts against — a single-source drift guard (the runner loads THIS
    /// graph from `flows`, so a fixture change that breaks the rung must break
    /// the build, not the in-cluster gate).
    #[test]
    fn rung1_fixture_is_the_manual_passthrough_flow() {
        let v: Value = serde_json::from_str(FLOW_JSON).expect("fixture parses");
        assert_eq!(v["flow-id"], json!(FLOW_ID));
        assert_eq!(v["trigger"]["type"], json!("manual"));
        assert_eq!(v["entry"], json!("in"));
        let nodes = v["nodes"].as_array().expect("nodes array");
        assert_eq!(
            nodes.len(),
            2,
            "rung 1 is a single meaningful node + respond"
        );
        assert_eq!(nodes[0]["id"], json!("in"));
        assert_eq!(nodes[0]["type"], json!("webhook-in"));
        assert_eq!(nodes[1]["id"], json!("out"));
        assert_eq!(nodes[1]["type"], json!("respond"));
        let edges = v["edges"].as_array().expect("edges array");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["from"], json!("in"));
        assert_eq!(edges[0]["to"], json!("out"));

        // And it is a VALID flow by the same engine the runner compiles it with
        // (Plan::compile validates first) — an invalid fixture would only fail
        // at runtime inside the deployed runner, not at build time.
        let flow = wamn_flow::Flow::from_json(FLOW_JSON).expect("fixture is a wamn-flow");
        flow.validate().expect("fixture validates");
        assert_eq!(flow.flow_id.as_str(), FLOW_ID);
    }

    #[test]
    fn schema_identifier_is_validated() {
        assert!(valid_ident("wamn_runner_demo"));
        assert!(!valid_ident("bad-schema"));
        assert!(!valid_ident("1leading"));
        assert!(!valid_ident("drop;table"));
    }

    #[test]
    fn terminal_states_stop_the_poll() {
        for s in ["completed", "failed", "cancelled", "infrastructure-failure"] {
            assert!(is_terminal(s), "{s}");
        }
        for s in ["dispatched", "running"] {
            assert!(!is_terminal(s), "{s}");
        }
    }
}
