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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_postgres::{Client, NoTls};
use wamn_node_invoke::{
    NodeInvokeRequest, SIGNATURE_HEADER, SIGNING_KEY_CREDENTIAL, SIGNING_KEY_CREDENTIAL_PREVIOUS,
    SignatureError, WirePayload, WireRunContext, granted_credentials, sign_envelope,
    sign_envelope_with_timestamp,
};
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
/// The per-project-env HMAC signing key (wamn-fqg.22), banked in BOTH the
/// runner's vault (so the flowrunner guest signs) and the serve-node's vault (so
/// it verifies) under the reserved `SIGNING_KEY_CREDENTIAL` name — the shared
/// runner-credentials Secret in production. A wrong key the negatives forge with.
const SIGNING_KEY: &str = "fqg22-per-project-env-hmac-0a1b2c3d4e5f";
const WRONG_KEY: &str = "attacker-guessed-the-wrong-key";
/// The PREVIOUS per-project-env key for the wamn-fqg.30 rotation-window assert.
const PREV_KEY: &str = "fqg30-previous-rotation-key-9f8e7d6c";

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
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.partition_owner TO wamn_app;\
         CREATE TABLE {schema}.run_dead_letters (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text NOT NULL, \
            flow_id text NOT NULL, reason text NOT NULL, \
            failed_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         ALTER TABLE {schema}.run_dead_letters ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_dead_letters FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_dead_letters_tenant ON {schema}.run_dead_letters \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT ON {schema}.run_dead_letters TO wamn_app;"
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
    let admin_url = args.admin_database_url.clone().context(
        "nodeinvoke needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;
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
    let node_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([
                ("granted".to_string(), SECRET.to_string()),
                ("sibling".to_string(), SIBLING_SECRET.to_string()),
                // wamn-fqg.22: the serve-node reads its per-project-env signing key
                // from THIS vault (the shared runner-credentials Secret in prod) and
                // enforces verify-before-grant.
                (SIGNING_KEY_CREDENTIAL.to_string(), SIGNING_KEY.to_string()),
            ]),
        )]),
    ));
    let serve = Arc::new(
        ServeNode::new(
            &engine,
            &node_wasm,
            node_vault,
            serve_node::DEFAULT_NODE_ID,
            PROJECT,
            Arc::from([]),
            false, // authn keyed below; not fail-closed (a key is present)
            None,  // wamn-fqg.32: replay-freshness OFF (default) for the E2E drain
        )
        .await
        .context("build serve-node")?,
    );

    // Drive the gate while the serve-node accept loop runs concurrently on the
    // SAME task (select!): when the gate logic returns, the server future drops.
    let serve_loop = serve_node::serve(serve.clone(), port);
    let gate = gate_body(&engine, &flowrunner, &node_wasm, &app_url, serve.clone(), port, n);

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
    node_wasm: &[u8],
    app_url: &str,
    serve: Arc<ServeNode>,
    port: u16,
    n: usize,
) -> anyhow::Result<bool> {
    let (mut seed_conn, _h) = connect_app(app_url).await?;
    wamn_gate_harness::seed_flow_version(
        &seed_conn,
        TENANT,
        FLOW_ID,
        1,
        true,
        &flow_json(port),
        true,
    )
    .await?;

    // The production runner. Its OWN vault is empty — the custom node's
    // credentials resolve at the serve-node's vault, not here — but the flow
    // declares `granted`, so the runner's per-run grant channel names it (for
    // any standard node; the custom hop carries its own grant in the envelope).
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.to_string());
    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    // wamn-fqg.22: the runner's vault carries the SAME per-project-env signing
    // key (the shared runner-credentials Secret in prod) so the flowrunner guest
    // resolves it via `wamn:node/credentials.get` and signs the hop envelope. The
    // node's own credentials still resolve at the serve-node's vault, not here.
    let runner_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([(
                SIGNING_KEY_CREDENTIAL.to_string(),
                SIGNING_KEY.to_string(),
            )]),
        )]),
    ));
    // The runner host allowlist admits the loopback serve-node (the outer bound;
    // the flow's declared allowed-hosts is the inner — both must pass).
    let allowed: Arc<[AllowedHost]> =
        vec![format!("127.0.0.1:{port}").parse::<AllowedHost>()?].into();

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
        allowed.clone(),
        30_000,
    )
    .await?;

    // Seed N runs of the custom-node flow, each echoing the same input.
    for i in 0..n {
        seed_run(&mut seed_conn, &format!("ni-{i}"), "\"hello\"").await?;
    }
    let report = worker.drain().await?;

    let queued = count(
        &seed_conn,
        &format!("SELECT count(*) FROM {SCHEMA}.run_queue"),
    )
    .await?;
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

    // -------------------------------------------------------------------------
    // wamn-fqg.22 — runner→node authn (signed invocation envelope).
    // -------------------------------------------------------------------------
    // The drain above ALREADY proves the signed positive end-to-end: the
    // serve-node holds a key, so it REQUIRES a valid signature; every run
    // completing means the REAL flowrunner signed the exact body correctly (an
    // unsigned or forged runner would 401 → DELIVERY would have failed).
    check(
        &mut ok,
        "AUTHN-POSITIVE: the signed hop drained N runs (a keyed serve-node accepted the flowrunner's real signature)",
        report.completed == n,
    );
    let grants_after_positive = serve.grant_install_count();
    check(
        &mut ok,
        "AUTHN-POSITIVE: each accepted invocation installed its grant (grant_install_count advanced by ≥N)",
        grants_after_positive >= n as u64,
    );

    // Drive the serve-node directly over raw HTTP to exercise the refusal arms
    // the happy path cannot forge — the exact envelope the flowrunner sends.
    let body = canonical_request().to_json();
    // The host clock (unix seconds) the fqg.32 freshness asserts compare against.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let good_sig = sign_envelope(SIGNING_KEY.as_bytes(), body.as_bytes());
    let wrong_sig = sign_envelope(WRONG_KEY.as_bytes(), body.as_bytes());
    let grants_before_negatives = serve.grant_install_count();

    // (1) UNSIGNED — no x-wamn-signature header at all.
    let (status, rbody) = raw_post(port, &body, None).await?;
    check(
        &mut ok,
        "AUTHN-UNSIGNED: an unsigned envelope is refused 401 (missing-signature)",
        status == 401 && rbody.contains("missing-signature"),
    );

    // (2) TAMPERED — a valid signature over the ORIGINAL body, but a MUTATED body
    // (attacker-chosen input) posted under it.
    let tampered = tampered_request().to_json();
    assert_ne!(tampered, body, "the tamper must actually change the body");
    let (status, rbody) = raw_post(port, &tampered, Some(&good_sig)).await?;
    check(
        &mut ok,
        "AUTHN-TAMPERED: a body that does not match its signature is refused 401 (bad-signature)",
        status == 401 && rbody.contains("bad-signature"),
    );

    // (3) WRONG-KEY — a well-formed signature under a key the attacker does not
    // share with the project-env.
    let (status, rbody) = raw_post(port, &body, Some(&wrong_sig)).await?;
    check(
        &mut ok,
        "AUTHN-WRONG-KEY: a signature under the wrong key is refused 401 (bad-signature)",
        status == 401 && rbody.contains("bad-signature"),
    );
    check(
        &mut ok,
        "AUTHN-NO-ORACLE: a refusal body never carries the expected MAC",
        !rbody.contains(&good_sig),
    );

    // VERIFY-BEFORE-GRANT (the load-bearing property, wamn-fqg.22): none of the
    // three refusals reached `invoke`, so NOT ONE installed a grant. A mutant
    // that removes/moves the verification lets a refused request install its
    // grant here → this named check kills it.
    let grants_after_negatives = serve.grant_install_count();
    check(
        &mut ok,
        "VERIFY-BEFORE-GRANT: not one refused request installed a grant (verify precedes grant install)",
        grants_after_negatives == grants_before_negatives,
    );

    // (4) RAW-SIGNED positive — a correctly-signed raw POST IS accepted (200) and
    // DOES install its grant, so the refusals above are a real contrast (the
    // check is not vacuously passing on a serve-node that refuses everything).
    let (status, _rbody) = raw_post(port, &body, Some(&good_sig)).await?;
    check(
        &mut ok,
        "AUTHN-SIGNED: a correctly-signed raw envelope is accepted (200) and installs exactly one grant",
        status == 200 && serve.grant_install_count() == grants_after_negatives + 1,
    );
    println!(
        "  authn: grants(after positive drain)={grants_after_positive}; refusals installed 0 grants (before={grants_before_negatives} after={grants_after_negatives}); raw-signed accepted"
    );

    // -------------------------------------------------------------------------
    // wamn-fqg.31 — fail-closed toggle (BOTH postures). verify_signature is the
    // pure verify-before-grant decision the accept loop makes; drive it directly
    // on two KEYLESS serve-nodes (an empty vault, no reserved signing key):
    //   * --require-signing-key  ⇒ REFUSE ALL (Unconfigured / signing-key-required),
    //     signed or unsigned — no silent revert to network trust;
    //   * default                ⇒ ADMIT unsigned (legacy network-trust, warned).
    // A mutant that drops the fail-closed arm admits the unsigned POST → the
    // FAIL-CLOSED check flips.
    // -------------------------------------------------------------------------
    let keyless_failclosed = ServeNode::new(
        engine,
        node_wasm,
        Arc::new(WamnCredentials::empty()),
        serve_node::DEFAULT_NODE_ID,
        PROJECT,
        Arc::from([]),
        true, // fail-closed
        None,
    )
    .await
    .context("build keyless fail-closed serve-node")?;
    check(
        &mut ok,
        "FAIL-CLOSED (fqg.31): a keyless require-signing-key host REFUSES an unsigned invocation (signing-key-required)",
        keyless_failclosed.verify_signature(body.as_bytes(), None, None, now)
            == Err(SignatureError::Unconfigured),
    );
    check(
        &mut ok,
        "FAIL-CLOSED (fqg.31): it also refuses a SIGNED invocation — no key to verify, so refuse ALL",
        keyless_failclosed.verify_signature(body.as_bytes(), Some(&good_sig), None, now)
            == Err(SignatureError::Unconfigured),
    );
    let keyless_default = ServeNode::new(
        engine,
        node_wasm,
        Arc::new(WamnCredentials::empty()),
        serve_node::DEFAULT_NODE_ID,
        PROJECT,
        Arc::from([]),
        false, // default: legacy network-trust
        None,
    )
    .await
    .context("build keyless default serve-node")?;
    check(
        &mut ok,
        "NETWORK-TRUST (fqg.31): the DEFAULT keyless host admits an unsigned invocation (backward-compatible)",
        keyless_default
            .verify_signature(body.as_bytes(), None, None, now)
            .is_ok(),
    );

    // -------------------------------------------------------------------------
    // wamn-fqg.30 — dual-key acceptance (rotation window). A serve-node holding
    // the CURRENT + PREVIOUS reserved keys accepts an envelope signed with EITHER
    // (the flowrunner always signs with the current key; the previous key covers
    // the window while runners pick up the new one). Garbage still 401s. A mutant
    // that only ever checks the current key rejects the previous-key signature →
    // the first check flips.
    // -------------------------------------------------------------------------
    let dual_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([
                (SIGNING_KEY_CREDENTIAL.to_string(), SIGNING_KEY.to_string()),
                (
                    SIGNING_KEY_CREDENTIAL_PREVIOUS.to_string(),
                    PREV_KEY.to_string(),
                ),
            ]),
        )]),
    ));
    let dual = ServeNode::new(
        engine,
        node_wasm,
        dual_vault,
        serve_node::DEFAULT_NODE_ID,
        PROJECT,
        Arc::from([]),
        false,
        None,
    )
    .await
    .context("build dual-key serve-node")?;
    let prev_sig = sign_envelope(PREV_KEY.as_bytes(), body.as_bytes());
    check(
        &mut ok,
        "DUAL-KEY (fqg.30): an envelope signed with the PREVIOUS key verifies during the rotation window",
        dual.verify_signature(body.as_bytes(), Some(&prev_sig), None, now)
            .is_ok(),
    );
    check(
        &mut ok,
        "DUAL-KEY (fqg.30): the CURRENT key still verifies alongside the previous",
        dual.verify_signature(body.as_bytes(), Some(&good_sig), None, now)
            .is_ok(),
    );
    check(
        &mut ok,
        "DUAL-KEY (fqg.30): a signature under NEITHER key is still refused (bad-signature)",
        dual.verify_signature(body.as_bytes(), Some(&wrong_sig), None, now)
            == Err(SignatureError::Mismatch),
    );

    // -------------------------------------------------------------------------
    // wamn-fqg.32 — replay freshness (timestamp, OFF by default). A serve-node
    // with a max-age configured requires a SIGNED, in-window timestamp; a stale
    // one is refused (stale-timestamp), a fresh one accepted. With freshness OFF
    // (the main keyed `serve` above), a LEGACY timestamp-less envelope still
    // verifies. A mutant that drops the age check accepts the stale envelope →
    // FRESHNESS-STALE flips; one that always checks freshness rejects the legacy
    // envelope → FRESHNESS-LEGACY flips.
    // -------------------------------------------------------------------------
    let fresh_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([(
                SIGNING_KEY_CREDENTIAL.to_string(),
                SIGNING_KEY.to_string(),
            )]),
        )]),
    ));
    let fresh = ServeNode::new(
        engine,
        node_wasm,
        fresh_vault,
        serve_node::DEFAULT_NODE_ID,
        PROJECT,
        Arc::from([]),
        false,
        Some(60), // enforce a 60s freshness window
    )
    .await
    .context("build freshness-enforcing serve-node")?;
    let fresh_ts = now.to_string();
    let fresh_sig = sign_envelope_with_timestamp(SIGNING_KEY.as_bytes(), body.as_bytes(), Some(&fresh_ts));
    check(
        &mut ok,
        "FRESHNESS-FRESH (fqg.32): a fresh timestamped envelope is accepted when max-age is enforced",
        fresh
            .verify_signature(body.as_bytes(), Some(&fresh_sig), Some(&fresh_ts), now)
            .is_ok(),
    );
    let stale_ts = now.saturating_sub(3600).to_string();
    let stale_sig =
        sign_envelope_with_timestamp(SIGNING_KEY.as_bytes(), body.as_bytes(), Some(&stale_ts));
    check(
        &mut ok,
        "FRESHNESS-STALE (fqg.32): a correctly-signed but STALE envelope is refused (stale-timestamp)",
        fresh.verify_signature(body.as_bytes(), Some(&stale_sig), Some(&stale_ts), now)
            == Err(SignatureError::Stale),
    );
    check(
        &mut ok,
        "FRESHNESS-LEGACY (fqg.32): a legacy timestamp-less envelope still verifies when freshness is OFF",
        serve
            .verify_signature(body.as_bytes(), Some(&good_sig), None, now)
            .is_ok(),
    );

    // -------------------------------------------------------------------------
    // wamn-fqg.29 — a persistent key mismatch fails the run TERMINALLY (no retry
    // budget burn). A runner whose vault holds the WRONG signing key signs every
    // custom-node POST wrong, so the keyed serve-node 401s each one identically.
    // The flowrunner maps that `invocation-unauthorized` refusal to a TERMINAL
    // node failure, so the engine fails the run on the FIRST attempt instead of
    // scheduling its full retry budget of transport retries against a refusal
    // that can never succeed. A mutant reverting the mapping to `Retryable` parks
    // the run for a backoff retry (failed=0, parked=1) → this check kills it.
    // -------------------------------------------------------------------------
    seed_run(&mut seed_conn, "ni-mismatch", "\"hello\"").await?;
    let mismatch_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([(
                SIGNING_KEY_CREDENTIAL.to_string(),
                WRONG_KEY.to_string(),
            )]),
        )]),
    ));
    let mut mismatch_cfg = WamnPostgresConfig::from_env();
    mismatch_cfg.database_url = Some(app_url.to_string());
    let mismatch_plugin = Arc::new(WamnPostgres::new(mismatch_cfg)?);
    let mut mismatch_worker = RunWorker::instantiate(
        engine,
        flowrunner,
        mismatch_plugin,
        mismatch_vault,
        RunnerIdentity {
            owner: "nodeinvoke-mismatch",
            tenant: TENANT,
            schema: Some(SCHEMA),
            project: PROJECT,
        },
        allowed.clone(),
        30_000,
    )
    .await?;
    let mreport = mismatch_worker.drain().await?;
    check(
        &mut ok,
        "AUTHN-MISMATCH-TERMINAL (fqg.29): a wrong-key run fails TERMINALLY in one claim (no park / retry-budget burn)",
        mreport.claimed == 1 && mreport.failed == 1 && mreport.parked == 0 && mreport.completed == 0,
    );
    let mrow = seed_conn
        .query_one(
            &format!(
                "SELECT status, fail_kind, fail_node FROM {SCHEMA}.runs WHERE run_id = 'ni-mismatch'"
            ),
            &[],
        )
        .await?;
    let mstatus: String = mrow.get(0);
    let mkind: Option<String> = mrow.get(1);
    let mnode: Option<String> = mrow.get(2);
    check(
        &mut ok,
        "AUTHN-MISMATCH-TERMINAL (fqg.29): run recorded failed/terminal on the custom-node step",
        mstatus == "failed" && mkind.as_deref() == Some("terminal") && mnode.as_deref() == Some("call"),
    );
    // The queue row is GONE (dequeued on a terminal outcome, never parked for a
    // retry): the retry budget was never engaged.
    let mismatch_q = count(
        &seed_conn,
        &format!("SELECT count(*) FROM {SCHEMA}.run_queue WHERE run_id = 'ni-mismatch'"),
    )
    .await?;
    check(
        &mut ok,
        "AUTHN-MISMATCH-TERMINAL (fqg.29): the failed run's queue row was dequeued (not parked for retry)",
        mismatch_q == 0,
    );
    println!(
        "  fqg.29 mismatch: claimed={} failed={} parked={} status={mstatus} fail_kind={:?} fail_node={:?}",
        mreport.claimed, mreport.failed, mreport.parked, mkind, mnode
    );

    Ok(ok)
}

/// The exact envelope the flowrunner's custom-node hop POSTs for this flow — the
/// substrate the raw authn checks sign, tamper, and mis-key.
fn canonical_request() -> NodeInvokeRequest {
    NodeInvokeRequest {
        ctx: WireRunContext {
            run_id: "authn-raw".into(),
            flow_id: FLOW_ID.into(),
            flow_version: 1,
            node_id: "call".into(),
            attempt: 0,
            idempotency_key: "authn-raw:call".into(),
            deadline_ms: Some(30_000),
            traceparent: None,
            tracestate: None,
            config: r#"{"endpoint":"http://127.0.0.1","probe":"granted","forbidden":"sibling"}"#
                .into(),
        },
        input: WirePayload::Inline("\"hello\"".into()),
        grant: granted_credentials(Some("granted")),
    }
}

/// The canonical envelope with an attacker-chosen input — the "forged input"
/// tamper the signature must catch.
fn tampered_request() -> NodeInvokeRequest {
    let mut r = canonical_request();
    r.input = WirePayload::Inline("\"attacker-chosen-input\"".into());
    r
}

/// POST a raw `/run` body to the loopback serve-node with an OPTIONAL
/// `x-wamn-signature`, returning (status-code, full-response-text). Half-closes
/// the write side so the server's keep-alive read EOFs and the response drains.
async fn raw_post(port: u16, body: &str, signature: Option<&str>) -> anyhow::Result<(u16, String)> {
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let mut req = format!(
        "POST /run HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(sig) = signature {
        req.push_str(&format!("{SIGNATURE_HEADER}: {sig}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    sock.write_all(req.as_bytes()).await?;
    sock.flush().await?;
    sock.shutdown().await?; // half-close: the server's next read EOFs cleanly
    let mut resp = Vec::new();
    sock.read_to_end(&mut resp).await?;
    let text = String::from_utf8_lossy(&resp).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    Ok((status, text))
}
