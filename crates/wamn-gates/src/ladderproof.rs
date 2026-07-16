//! ladderproof — the execution-ladder conformance proof (wamn-ojm).
//!
//! Prove flows execute CORRECTLY on the LIVE runner, OUTSIDE a bench harness.
//! Unlike `runnerbench` (which instantiates the flowrunner IN-PROC via
//! [`wamn_host::run_worker::RunWorker`] and drives the claim loop itself),
//! `ladderproof` is a pure DB CLIENT — the f1proof/apiproof shape: it seeds ONE
//! run the dispatcher way (write-ahead `dispatched` row + queue row) and then
//! WAITS for a SEPARATELY-DEPLOYED `run-worker` service (deploy/runner.yaml) to
//! claim it, drive it, and record the result. It asserts nothing about how the
//! run was driven — only that the deployed runner produced the correct terminal
//! state + per-node execution trace.
//!
//! The proof is rung-parameterised (`--rung`): each rung adds a case over the
//! same seed/poll/assert client, climbing the ladder.
//!   * **Rung 1** (wamn-ojm.1) — `webhook-in -> respond`
//!     (deploy/ladder/rung1.flow.json): a single meaningful node + a terminal,
//!     both passthrough, so the run completes echoing its input.
//!   * **Rung 2** (wamn-ojm.2) — `webhook-in -> transform{upper} ->
//!     transform{reverse} -> respond` (deploy/ladder/rung2.flow.json): a linear
//!     multi-node chain that proves correct SEQUENCING (the `node_runs` seq
//!     order) + payload THREADING (each node's recorded input is the prior
//!     node's recorded output).
//!
//! Each rung is a `manual`-trigger flow: nothing auto-fires it, so the proof
//! seeds the run directly, isolating the RUNNER (the subject) from the trigger
//! machinery (cron/outbox, already gated by the dispatcher).
//!
//! `--setup` provisions a fresh ephemeral schema + registers EVERY rung's flow
//! (the LOCAL self-contained path, run once before the mutation loop) so one
//! schema serves both rungs. Without it, ladderproof is a client against a
//! schema the deploy pipeline already provisioned — so re-runs never drop the
//! schema out from under the live runner.

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};

use wamn_gate_harness::{check, scope_session, seed_flow_version};
use wamn_run_queue::{enqueue_sql, write_ahead_triggered_run_sql};

/// The committed rung fixtures (single source of truth; the drift-guard tests
/// pin that each file parses to the flow the proof asserts against).
const RUNG1_FLOW_JSON: &str = include_str!("../../../deploy/ladder/rung1.flow.json");
const RUNG2_FLOW_JSON: &str = include_str!("../../../deploy/ladder/rung2.flow.json");

/// The rungs registered by `--setup` (so one ephemeral schema serves both).
const ALL_RUNGS: [u8; 2] = [1, 2];

#[derive(Debug, Args)]
pub struct LadderProofArgs {
    /// Which rung to prove (1 = single-node passthrough, 2 = linear transform
    /// chain). The runner + schema are rung-agnostic; only the seeded flow and
    /// the expected chain differ.
    #[arg(long, default_value_t = 1)]
    pub rung: u8,

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

    /// Provision a fresh ephemeral schema (admin) + register EVERY rung's flow
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

/// One rung's fixture + the exact execution trace the deployed runner must
/// produce. `chain` is the ordered list of `(node_id, expected output payload)`
/// in dispatch order — the sequencing spine; `input` is the seeded trigger
/// payload (the entry node's incoming payload). Threading is then structural:
/// node i's recorded input must equal the run input (i == 0) or the prior node's
/// recorded output.
struct RungCase {
    flow_json: &'static str,
    flow_id: &'static str,
    input: Value,
    chain: Vec<(&'static str, Value)>,
}

impl RungCase {
    /// The run's final result is the last node's output.
    fn expected_result(&self) -> &Value {
        &self.chain.last().expect("a rung has >= 1 node").1
    }
}

/// Build the case for a rung. Rung 2's expected outputs are computed with the
/// SAME `upper`/`reverse` the flowrunner's legacy `transform` arm applies over
/// `payload.as_str()` — so the input must be a JSON STRING (an object would
/// stringify to `""`). `"abcDEF"` -> `"ABCDEF"` -> `"FEDCBA"` is visibly
/// distinct at every step (case change, then order flip), so a reordered or
/// dropped node breaks the recorded chain even though `upper`/`reverse` happen
/// to commute on the final result.
fn rung_case(rung: u8) -> anyhow::Result<RungCase> {
    match rung {
        1 => {
            // A nested object proves the whole payload flows through both
            // passthrough nodes and back out as the run result.
            let input = json!({ "msg": "hello-ladder", "nested": { "n": 42 } });
            Ok(RungCase {
                flow_json: RUNG1_FLOW_JSON,
                flow_id: "ladder-rung1",
                chain: vec![("in", input.clone()), ("out", input.clone())],
                input,
            })
        }
        2 => {
            let s = "abcDEF";
            let upper = s.to_uppercase();
            let reversed: String = upper.chars().rev().collect();
            let input = Value::String(s.to_string());
            Ok(RungCase {
                flow_json: RUNG2_FLOW_JSON,
                flow_id: "ladder-rung2",
                chain: vec![
                    ("in", input.clone()),
                    ("t1", Value::String(upper)),
                    ("t2", Value::String(reversed.clone())),
                    ("out", Value::String(reversed)),
                ],
                input,
            })
        }
        other => bail!("unknown rung {other} (supported: {ALL_RUNGS:?})"),
    }
}

fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The flow tables the runner walks + the 5.14 `run_queue` it claims from, in the
/// house tenant floor (the runnerbench shape, minus the pg-write `sink` — the
/// ladder flows have no pg-write node).
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
/// register EVERY rung's flow active (app, under the tenant claim). The LOCAL
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

    // Register every rung's flow via the app role, so the same RLS floor the
    // runner reads under is exercised at registration and one schema serves the
    // whole ladder (flow ids are distinct, so all coexist active).
    let app = connect_app(app_url, schema, tenant).await?;
    for rung in ALL_RUNGS {
        let case = rung_case(rung)?;
        seed_flow_version(&app, tenant, case.flow_id, 1, true, case.flow_json, true)
            .await
            .with_context(|| format!("register {}", case.flow_id))?;
    }
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
async fn seed_run(
    client: &mut Client,
    flow_id: &str,
    run_id: &str,
    input_text: &str,
) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &flow_id, &1i32, &"manual", &input_text],
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
    let case = rung_case(args.rung)?;
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;

    println!(
        "# wamn-gates ladderproof rung {} — deployed-runner conformance (schema {}, tenant {}, flow {})",
        args.rung, args.schema, args.tenant, case.flow_id
    );

    if args.setup {
        let admin_url = args.admin_database_url.clone().context(
            "--setup needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
        )?;
        setup(&admin_url, &app_url, &args.schema, &args.tenant)
            .await
            .context("setup: provision schema + register flows")?;
        println!("## setup — provisioned schema + registered rungs {ALL_RUNGS:?} (active)");
    }

    let input_text = serde_json::to_string(&case.input)?;

    // A unique run id per invocation so re-runs never collide (the mutation loop
    // re-runs the client many times against the same live schema + runner).
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let run_id = format!("ladder-{nanos}");

    let mut client = connect_app(&app_url, &args.schema, &args.tenant).await?;
    seed_run(&mut client, case.flow_id, &run_id, &input_text).await?;
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

    let outcome = assert_run(&client, &run_id, &case, &status).await?;

    if args.teardown
        && let Some(admin_url) = args.admin_database_url.clone()
    {
        let _ = teardown(&admin_url, &args.schema).await;
    }

    println!(
        "\nladderproof rung {} complete — overall PASS: {outcome}",
        args.rung
    );
    if !outcome {
        bail!("ladderproof rung {} failed", args.rung);
    }
    Ok(())
}

/// Assert the deployed runner drove the seeded run correctly: terminal state,
/// the final result, and — the ladder-rung-2 point — correct SEQUENCING (the
/// `node_runs` are exactly `case.chain` in seq order) + payload THREADING (each
/// node's recorded input is the run input at seq 0, else the prior node's
/// recorded output).
async fn assert_run(
    client: &Client,
    run_id: &str,
    case: &RungCase,
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
        "result_json is the final node's output",
        result_val.as_ref() == Some(case.expected_result()),
    );
    check(
        &mut ok,
        "trigger_source recorded as manual",
        trigger.as_deref() == Some("manual"),
    );

    let rows = client
        .query(
            "SELECT node_id, seq, status, output_port, output_json::text, input_json::text \
             FROM node_runs WHERE run_id = $1 ORDER BY seq",
            &[&run_id],
        )
        .await?;
    check(
        &mut ok,
        &format!(
            "node_runs count == chain length ({}) — got {}",
            case.chain.len(),
            rows.len()
        ),
        rows.len() == case.chain.len(),
    );
    // The per-node execution trace: each node succeeded on the main port, in
    // dispatch order, emitting the chain's expected payload, and receiving the
    // prior node's output (the threading proof).
    if rows.len() == case.chain.len() {
        for (i, (row, (want_id, want_output))) in rows.iter().zip(case.chain.iter()).enumerate() {
            let node_id: String = row.get(0);
            let seq: i32 = row.get(1);
            let status: String = row.get(2);
            let port: Option<String> = row.get(3);
            let output = row
                .get::<_, Option<String>>(4)
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let node_input = row
                .get::<_, Option<String>>(5)
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            check(
                &mut ok,
                &format!("node_run {want_id} @ seq {i}: id/seq/status/port"),
                node_id == *want_id
                    && seq == i as i32
                    && status == "success"
                    && port.as_deref() == Some("main"),
            );
            check(
                &mut ok,
                &format!("node_run {want_id}: output matches the chain"),
                output.as_ref() == Some(want_output),
            );
            // Threading: node i's input == the run input (seq 0) else the prior
            // node's output. This is what makes it a MULTI-NODE proof — the
            // payload was threaded through, not recomputed from the trigger.
            let expected_input = if i == 0 {
                &case.input
            } else {
                &case.chain[i - 1].1
            };
            check(
                &mut ok,
                &format!("node_run {want_id}: input == prior node's output (threading)"),
                node_input.as_ref() == Some(expected_input),
            );
        }
    }

    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A committed fixture parses to the flow the proof asserts against, and is
    /// VALID by the same engine the runner compiles it with (`Plan::compile`
    /// validates first) — a single-source drift guard so a fixture change that
    /// breaks the rung breaks the build, not the in-cluster gate.
    fn assert_valid_fixture(json: &str, flow_id: &str) -> Value {
        let v: Value = serde_json::from_str(json).expect("fixture parses");
        assert_eq!(v["flow-id"], json!(flow_id));
        assert_eq!(v["trigger"]["type"], json!("manual"));
        let flow = wamn_flow::Flow::from_json(json).expect("fixture is a wamn-flow");
        flow.validate().expect("fixture validates");
        assert_eq!(flow.flow_id.as_str(), flow_id);
        v
    }

    #[test]
    fn rung1_fixture_is_the_manual_passthrough_flow() {
        let v = assert_valid_fixture(RUNG1_FLOW_JSON, "ladder-rung1");
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
    }

    #[test]
    fn rung2_fixture_is_the_linear_transform_chain() {
        let v = assert_valid_fixture(RUNG2_FLOW_JSON, "ladder-rung2");
        assert_eq!(v["entry"], json!("in"));
        let nodes = v["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 4, "rung 2 is in -> t1 -> t2 -> out");
        // The transform ops are load-bearing: t1 upper then t2 reverse.
        assert_eq!(nodes[1]["id"], json!("t1"));
        assert_eq!(nodes[1]["type"], json!("transform"));
        assert_eq!(nodes[1]["config"]["op"], json!("upper"));
        assert_eq!(nodes[2]["id"], json!("t2"));
        assert_eq!(nodes[2]["type"], json!("transform"));
        assert_eq!(nodes[2]["config"]["op"], json!("reverse"));
        assert_eq!(nodes[3]["type"], json!("respond"));
        // Edges thread in -> t1 -> t2 -> out (the sequencing spine).
        let edges: Vec<(String, String)> = v["edges"]
            .as_array()
            .expect("edges array")
            .iter()
            .map(|e| {
                (
                    e["from"].as_str().unwrap().to_string(),
                    e["to"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            edges,
            vec![
                ("in".into(), "t1".into()),
                ("t1".into(), "t2".into()),
                ("t2".into(), "out".into()),
            ]
        );
    }

    /// The rung-2 case computes the chain the way the flowrunner's legacy
    /// transform arm does: input is a JSON STRING (so `payload.as_str()` sees
    /// it), upper then reverse. This pins the expected trace + the threading
    /// relation the live assert checks.
    #[test]
    fn rung2_case_threads_upper_then_reverse() {
        let case = rung_case(2).expect("rung 2 case");
        assert_eq!(case.flow_id, "ladder-rung2");
        assert_eq!(case.input, json!("abcDEF"));
        let ids: Vec<&str> = case.chain.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec!["in", "t1", "t2", "out"]);
        let outs: Vec<&Value> = case.chain.iter().map(|(_, o)| o).collect();
        assert_eq!(*outs[0], json!("abcDEF")); // in: passthrough
        assert_eq!(*outs[1], json!("ABCDEF")); // t1: upper
        assert_eq!(*outs[2], json!("FEDCBA")); // t2: reverse(upper)
        assert_eq!(*outs[3], json!("FEDCBA")); // out: passthrough
        assert_eq!(case.expected_result(), &json!("FEDCBA"));
    }

    #[test]
    fn unknown_rung_is_rejected() {
        assert!(rung_case(3).is_err());
        assert!(rung_case(0).is_err());
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
