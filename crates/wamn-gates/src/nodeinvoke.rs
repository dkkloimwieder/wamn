//! `nodeinvoke` — the production custom-node invocation gate (5.6 / wamn-bd5).
//!
//! Proves the WHOLE v0 path end-to-end, locally and repeatably: the REAL runner
//! (the production [`RunWorker`] driving `flowrunner.wasm`) executes a flow whose
//! step is a CUSTOM node, which dispatches as an in-cluster HTTP hop to a REAL
//! [`ServeNode`] host serving `node-cred.wasm` under the real `wamn:node` world.
//! Both wasmtime stores run concurrently on ONE task via `select!` (no cross-
//! thread store), so the flowrunner's `wasi:http` POST reaches the serve-node's
//! `/run` and the reply folds back into the walk.
//!
//! Assertions (each named):
//!   * DELIVERY — every seeded run completes; the custom node's `node_runs`
//!     output round-trips the input payload (payload in -> node output back);
//!   * GRANT — the node reads its DECLARED credential (`ok:<secret>`): the
//!     runner declared exactly it in the envelope, the serve-node host installed
//!     it as the per-invocation grant;
//!   * NOT-GRANTED — the node's probe of an UNDECLARED (sibling) credential is
//!     `not-granted` at the real WIT boundary (the credprobe negative, now live);
//!   * MEMOIZED — across N runs sharing one custom-node config, the serve-node
//!     parsed that config exactly ONCE (design-note 9b).
//!
//! The runner never gets the trusted grant channel FOR THE NODE — the node is a
//! separate component the serve-node host grants get-only. A forged-wider grant
//! is the mutation (a) target, killed by NOT-GRANTED.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use wamn_run_queue::{enqueue_sql, write_ahead_triggered_run_sql};

use wamn_gate_harness::check;
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_credentials::WamnCredentials;
use wamn_host::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_host::serve_node::{self, ServeNode};
use wamn_run_worker::{RunWorker, RunnerIdentity};
use wash_runtime::host::allowed_hosts::AllowedHost;

const SCHEMA: &str = "wamn_nodeinvoke_bench";
const TENANT: &str = "nodeinvoke-tenant";
const OWNER: &str = "nodeinvoke-bench";
const FLOW_ID: &str = "node-invoke";
const PROJECT: &str = "default";
/// Distinctive secrets so `ok:<secret>` / the leak are unambiguous.
const SECRET: &str = "node-secret-7c1f2a";
const SIBLING_SECRET: &str = "sibling-secret-do-not-leak";

#[derive(Debug, Args)]
pub struct NodeInvokeArgs {
    /// The flowrunner guest (`flowrunner.wasm`) the runner drives.
    #[arg(long)]
    pub flowrunner: PathBuf,

    /// The credential-reading custom node (`node_cred.wasm`) the serve-node host
    /// serves under the real wamn:node world.
    #[arg(long)]
    pub node_cred: PathBuf,

    /// App (runner) Postgres URL — the NOSUPERUSER wamn_app role.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Loopback port the serve-node HTTP server binds (the runner->node hop
    /// target). The flow's allowed-hosts + the runner host allowlist both admit
    /// `127.0.0.1:<port>`.
    #[arg(long, default_value_t = 8091)]
    pub node_port: u16,

    /// Runs seeded (each drives the same custom-node config, so memoization is
    /// observable — N runs, one config parse).
    #[arg(long, default_value_t = 12)]
    pub iters: usize,
}

/// The custom-node flow: `in -> call(custom) -> done(respond)`. The `call` step
/// declares credential `granted`, points at the loopback serve-node, and probes
/// `granted` (declared -> readable) + `sibling` (undeclared -> not-granted).
fn flow_json(port: u16) -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FLOW_ID}","version":1,
            "trigger":{{"type":"manual"}},"entry":"in",
            "credentials":[{{"name":"granted"}}],
            "allowed-hosts":["127.0.0.1:{port}"],
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"call","type":"custom","credential":"granted",
                "config":{{"endpoint":"http://127.0.0.1:{port}","probe":"granted","forbidden":"sibling"}}}},
              {{"id":"done","type":"respond"}}
            ],
            "edges":[{{"from":"in","to":"call"}},{{"from":"call","to":"done"}}]}}"#
    )
}

// --- ephemeral schema (the flowrunner flow tables + the run_queue) -----------
fn runner_ddl(schema: &str) -> String {
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
         CREATE TABLE {schema}.sink (\
            tenant_id text NOT NULL, run_id text NOT NULL, step int NOT NULL, \
            payload text NOT NULL, \
            CONSTRAINT sink_idem UNIQUE (tenant_id, run_id, step));\
         ALTER TABLE {schema}.sink ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.sink FORCE ROW LEVEL SECURITY;\
         CREATE POLICY sink_tenant ON {schema}.sink \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.sink TO wamn_app;\
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
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.partition_owner TO wamn_app;"
    )
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; CREATE SCHEMA {SCHEMA} AUTHORIZATION postgres; GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        client
            .batch_execute(&runner_ddl(SCHEMA))
            .await
            .context("apply runner DDL")?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn teardown(admin_url: &str) {
    if let Ok((client, conn)) = tokio_postgres::connect(admin_url, NoTls).await {
        let conn_task = tokio::spawn(conn);
        let _ = client
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"))
            .await;
        drop(client);
        let _ = conn_task.await;
    }
}

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

/// Seed a run the way the dispatcher does: the write-ahead `dispatched` row +
/// the queue row, co-transacted. The trigger input is a JSON string the
/// custom node echoes back.
async fn seed_run(client: &mut Client, run_id: &str, input_json: &str) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &FLOW_ID, &1i32, &"manual", &input_json],
    )
    .await?;
    tx.execute(
        &enqueue_sql(),
        &[&run_id, &Option::<&str>::None, &0i32, &0i64],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn count(client: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}

pub async fn run(args: NodeInvokeArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let flowrunner = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read flowrunner {}", args.flowrunner.display()))?;
    let node_wasm = std::fs::read(&args.node_cred)
        .with_context(|| format!("read node-cred {}", args.node_cred.display()))?;
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("nodeinvoke needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL")?;
    let port = args.node_port;
    let n = args.iters;

    println!(
        "# wamn-gates nodeinvoke — v0 custom-node HTTP invocation (schema {SCHEMA}, node port {port})"
    );
    provision(&admin_url).await.context("provision schema")?;

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    // The serve-node host: a warm node-cred instance whose vault has the granted
    // secret AND an ungranted sibling in the same project. The runner->node hop
    // is loopback; the node's OWN egress is deny-all (it makes none).
    let node_vault = Arc::new(WamnCredentials::from_projects(std::collections::HashMap::from([(
        PROJECT.to_string(),
        std::collections::HashMap::from([
            ("granted".to_string(), SECRET.to_string()),
            ("sibling".to_string(), SIBLING_SECRET.to_string()),
        ]),
    )])));
    let serve = Arc::new(
        ServeNode::new(
            &engine,
            &node_wasm,
            node_vault,
            serve_node::DEFAULT_NODE_ID,
            PROJECT,
            Arc::from([]),
        )
        .await
        .context("build serve-node")?,
    );

    // Drive the gate while the serve-node accept loop runs concurrently on the
    // SAME task (select!): when the gate logic returns, the server future drops.
    let serve_loop = serve_node::serve(serve.clone(), port);
    let gate = gate_body(&engine, &flowrunner, &app_url, serve.clone(), port, n);

    let outcome = tokio::select! {
        r = serve_loop => r.map(|_| false), // the server only ends on error
        r = gate => r,
    };

    ticker.abort();
    teardown(&admin_url).await;
    let pass = outcome?;

    println!("\nnodeinvoke complete — overall PASS: {pass}");
    if !pass {
        bail!("nodeinvoke gate failed");
    }
    Ok(())
}

async fn gate_body(
    engine: &wash_runtime::engine::Engine,
    flowrunner: &[u8],
    app_url: &str,
    serve: Arc<ServeNode>,
    port: u16,
    n: usize,
) -> anyhow::Result<bool> {
    let (mut seed_conn, _h) = connect_app(app_url).await?;
    wamn_gate_harness::seed_flow_version(
        &seed_conn, TENANT, FLOW_ID, 1, true, &flow_json(port), true,
    )
    .await?;

    // The production runner. Its OWN vault is empty — the custom node's
    // credentials resolve at the serve-node's vault, not here — but the flow
    // declares `granted`, so the runner's per-run grant channel names it (for
    // any standard node; the custom hop carries its own grant in the envelope).
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.to_string());
    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    let runner_vault = Arc::new(WamnCredentials::empty());
    // The runner host allowlist admits the loopback serve-node (the outer bound;
    // the flow's declared allowed-hosts is the inner — both must pass).
    let allowed: Arc<[AllowedHost]> = vec![format!("127.0.0.1:{port}").parse::<AllowedHost>()?].into();

    let mut worker = RunWorker::instantiate(
        engine,
        flowrunner,
        plugin,
        runner_vault,
        RunnerIdentity {
            owner: OWNER,
            tenant: TENANT,
            schema: Some(SCHEMA),
            project: PROJECT,
        },
        allowed,
        30_000,
    )
    .await?;

    // Seed N runs of the custom-node flow, each echoing the same input.
    for i in 0..n {
        seed_run(&mut seed_conn, &format!("ni-{i}"), "\"hello\"").await?;
    }
    let report = worker.drain().await?;

    let queued = count(&seed_conn, &format!("SELECT count(*) FROM {SCHEMA}.run_queue")).await?;
    let completed = count(
        &seed_conn,
        &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE status = 'completed'"),
    )
    .await?;

    let mut ok = true;
    check(
        &mut ok,
        "DELIVERY: every seeded run drained to completed",
        report.claimed == n && report.completed == n && queued == 0 && completed as usize == n,
    );
    // Diagnostics: the drain tally + any failure verdict + the recorded node
    // trail, so a broken hop is legible in the gate output (not a silent fail).
    println!(
        "  drain: claimed={} completed={} parked={} failed={} (queue rows left={queued})",
        report.claimed, report.completed, report.parked, report.failed
    );
    for row in seed_conn
        .query(
            &format!(
                "SELECT run_id, status, fail_kind, fail_node, fail_reason FROM {SCHEMA}.runs ORDER BY run_id LIMIT 3"
            ),
            &[],
        )
        .await?
    {
        let rid: String = row.get(0);
        let status: String = row.get(1);
        let fk: Option<String> = row.get(2);
        let fnode: Option<String> = row.get(3);
        let fr: Option<String> = row.get(4);
        println!(
            "  run {rid}: status={status} fail_kind={:?} fail_node={:?} fail_reason={:?}",
            fk, fnode, fr
        );
    }
    for row in seed_conn
        .query(
            &format!("SELECT node_id, status, error_kind FROM {SCHEMA}.node_runs WHERE run_id = 'ni-0' ORDER BY seq"),
            &[],
        )
        .await?
    {
        let nid: String = row.get(0);
        let st: String = row.get(1);
        let ek: Option<String> = row.get(2);
        println!("  ni-0 node_run: {nid} status={st} error_kind={:?}", ek);
    }

    // Inspect the custom node's recorded output on one run (payload round-trip +
    // the credential probes at the real WIT boundary).
    let out_row = seed_conn
        .query_one(
            &format!(
                "SELECT output_json::text FROM {SCHEMA}.node_runs WHERE run_id = 'ni-0' AND node_id = 'call'"
            ),
            &[],
        )
        .await
        .context("custom node produced no node_runs row")?;
    let out_text: String = out_row.get(0);
    let out: serde_json::Value = serde_json::from_str(&out_text).context("node output not JSON")?;

    check(
        &mut ok,
        "DELIVERY: input payload round-tripped through the node (echo == input)",
        out.get("echo").and_then(|v| v.as_str()) == Some("hello"),
    );
    check(
        &mut ok,
        "GRANT: the DECLARED credential is readable inside the node (ok:<secret>)",
        out.get("probe").and_then(|v| v.as_str()) == Some(&format!("ok:{SECRET}")[..]),
    );
    check(
        &mut ok,
        "NOT-GRANTED: an UNDECLARED (sibling) credential is not-granted at the boundary",
        out.get("forbidden").and_then(|v| v.as_str()) == Some("err:not-granted"),
    );
    // Belt and braces: the leaked secret text never appears in the recorded output.
    check(
        &mut ok,
        "NOT-GRANTED: the ungranted sibling secret never leaks into run history",
        !out_text.contains(SIBLING_SECRET),
    );

    // Design-note 9b: N runs share ONE custom-node config identity, so the warm
    // serve-node parsed that config exactly once.
    let parses = serve.config_parse_count().await;
    check(
        &mut ok,
        "MEMOIZED: N runs of one config parsed exactly once on the serve-node (9b)",
        parses == 1,
    );
    println!(
        "  (config parses on the warm serve-node = {parses} across {n} invocations; drained {}/{n})",
        report.completed
    );

    Ok(ok)
}
