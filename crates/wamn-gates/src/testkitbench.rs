//! `testkitbench` — the 11.4 assertion-library gate: cases-as-data driven
//! through the pure `wamn-testkit` vocabulary.
//!
//! A checked-in JSON fixture (`--cases`, a `Vec<TestCase>`) proves the
//! cases-as-data path (the 828 lane's catalog-jsonb store reads the identical
//! shape). Each case routes by its ref:
//!
//!   node-level (`node_ref`) — the gate warm-instantiates the node in a REAL
//!     [`ServeNode`] (the f2invoke template) and `.invoke()`s it with the case
//!     input/config, capturing the emission / port / error.
//!   flow-level (`flow_ref`) — the gate drives the flow under the test-double
//!     set (`DoubleSet::virtual_host` + a spying `EgressRecorder` + the 9-arg
//!     [`RunWorker::instantiate`]) exactly as `testhostbench`'s runworker phase
//!     does, then captures the run outcome (from the [`DrainReport`]), the egress
//!     log, and admin-pool DB reads.
//!
//! Every captured fact bundle is folded through [`wamn_testkit::evaluate`] and
//! each [`AssertionResult`](wamn_testkit::AssertionResult) becomes a
//! [`wamn_gate_harness::check`] line — the library decides, the gate only drives.
//!
//! DB-state note: the flow-level DB reads go through the provisioner's SUPERUSER
//! (admin) session (RLS-bypassing), scoped to the runner's tenant + schema. This
//! is the same admin path `testhostbench` uses for its final-DB-state asserts,
//! and it is DISTINCT from the runner's own `wamn_app` (NOSUPERUSER, RLS-forced)
//! pool — a DB-state assert observes the row a superuser sees, not the row the
//! tenant-scoped app role would.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use wash_runtime::host::http::HostHandler;

use wamn_gate_harness::{check, scope_session};
use wamn_host::doubles::{DoubleSet, EgressRecorder, EphemeralSchemaProvisioner};
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_credentials::WamnCredentials;
use wamn_host::plugins::wamn_logging::WamnLogging;
use wamn_host::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_host::serve_node::{self, ServeNode, ServeNodeAuthn};
use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, WireNodeError, WirePayload, WireRunContext,
};
use wamn_run_queue::{enqueue_sql, write_ahead_triggered_run_sql};
use wamn_run_worker::{RunWorker, RunnerIdentity};
use wamn_testkit::{
    Assertion, Captured, DbCapture, NodeErrorKind, Outcome, RunFacts, RunStatus, TestCase, evaluate,
};

/// The virtual-clock epoch + seed the flow-level test host uses (matching the
/// run-worker `--test-doubles` constants).
const TEST_EPOCH_SECS: u64 = 1_700_000_000;
const TEST_SEED: u64 = 0x7492_5EED_5EED_7492;

/// The runner identity the flow-level path drives under. `RW_OWNER` is the store
/// workload id an egress assertion's `flow` key must name — the checked-in
/// fixture's flow-level egress asserts target exactly this string.
const RW_TENANT: &str = "tk-rw-tenant";
const RW_OWNER: &str = "tk-runworker";
const RW_SCHEMA: &str = "tk_runworker";

#[derive(Debug, Args)]
pub struct TestKitBenchArgs {
    /// Path to the checked-in `Vec<TestCase>` JSON fixture.
    #[arg(long, default_value = "/bench/testkit-cases.json")]
    pub cases: PathBuf,

    /// The compiled node the node-level cases invoke (default: the disposition
    /// sample node baked into the gates image).
    #[arg(long, default_value = "/bench/disposition-node.wasm")]
    pub node: PathBuf,

    /// The flowrunner guest the flow-level cases drive.
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// `wamn_app` Postgres URL (overrides DATABASE_URL / WAMN_PG_URL) — required
    /// only when the fixture carries flow-level cases.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL used to provision/drop the ephemeral flow schema AND to run
    /// the DB-state assertion queries (env WAMN_PG_ADMIN_URL).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Pool max size (flow-level path).
    #[arg(long, default_value_t = 8)]
    pub pool_max: usize,
}

pub async fn run(args: TestKitBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates testkitbench — 11.4 assertion library (cases-as-data)");

    let raw = std::fs::read_to_string(&args.cases)
        .with_context(|| format!("read cases fixture {}", args.cases.display()))?;
    let cases: Vec<TestCase> = serde_json::from_str(&raw)
        .with_context(|| format!("parse Vec<TestCase> from {}", args.cases.display()))?;
    println!(
        "loaded {} case(s) from {} (cases-as-data path)",
        cases.len(),
        args.cases.display()
    );

    let node_cases: Vec<&TestCase> = cases.iter().filter(|c| c.node_ref.is_some()).collect();
    let flow_cases: Vec<&TestCase> = cases.iter().filter(|c| c.flow_ref.is_some()).collect();

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    let mut ok = true;
    if !node_cases.is_empty() {
        ok &= node_phase(&engine, &args.node, &node_cases).await?;
    }
    if !flow_cases.is_empty() {
        ok &= flow_phase(&engine, &args, &flow_cases).await?;
    }

    ticker.abort();
    println!("\ntestkitbench complete — overall PASS: {ok}");
    if !ok {
        bail!("testkitbench gate failed");
    }
    Ok(())
}

/// Fold one case's [`Outcome`] into the running pass flag: one self-describing
/// [`check`] line per assertion (the assertion's wire form + the failure detail).
fn fold_outcome(ok: &mut bool, outcome: &Outcome) {
    for r in &outcome.results {
        let desc = serde_json::to_string(&r.assertion).unwrap_or_default();
        let label = match (r.passed, &r.detail) {
            (false, Some(d)) => format!("{} :: {desc} — {d}", outcome.name),
            _ => format!("{} :: {desc}", outcome.name),
        };
        check(ok, &label, r.passed);
    }
}

// ---------------------------------------------------------------------------
// node-level: warm ServeNode + .invoke() (the f2invoke template)
// ---------------------------------------------------------------------------

async fn node_phase(
    engine: &wash_runtime::engine::Engine,
    node_path: &std::path::Path,
    cases: &[&TestCase],
) -> anyhow::Result<bool> {
    println!(
        "\n## node — {} case(s) drive the pure run(ctx,input) handler via a warm ServeNode",
        cases.len()
    );
    let wasm =
        std::fs::read(node_path).with_context(|| format!("read node {}", node_path.display()))?;

    // A world node: empty vault, no signing key (direct .invoke() admitted),
    // deny-all egress — exactly the f2invoke posture.
    let serve = ServeNode::new(
        engine,
        &wasm,
        Arc::new(WamnCredentials::empty()),
        serve_node::DEFAULT_NODE_ID,
        "default",
        Arc::from([]),
        ServeNodeAuthn {
            require_signing_key: false,
            max_signature_age_secs: None,
        },
    )
    .await
    .context("warm-instantiate the node under test")?;

    let mut ok = true;
    for case in cases {
        let resp = serve.invoke(build_node_request(case)).await;
        let captured = capture_node(&resp);
        fold_outcome(&mut ok, &evaluate(case, &captured));
    }
    Ok(ok)
}

/// Build the invocation envelope for a node case: the case ctx (or a default),
/// with `config` overridden by the case config and the input carried inline. No
/// credential grant (the vocabulary v0 targets world/zero-import nodes).
fn build_node_request(case: &TestCase) -> NodeInvokeRequest {
    let mut ctx = case.ctx.clone().unwrap_or_else(|| default_ctx(case));
    if let Some(cfg) = &case.config {
        ctx.config = cfg.to_string();
    }
    NodeInvokeRequest {
        ctx,
        input: WirePayload::Inline(case.input.to_string()),
        grant: Vec::new(),
    }
}

fn default_ctx(case: &TestCase) -> WireRunContext {
    WireRunContext {
        run_id: "testkit".into(),
        flow_id: "testkit".into(),
        flow_version: 1,
        node_id: case
            .node_ref
            .as_ref()
            .and_then(|n| n.node_id.clone())
            .unwrap_or_else(|| "node".into()),
        attempt: 0,
        idempotency_key: format!("testkit:{}", case.name),
        deadline_ms: None,
        traceparent: None,
        tracestate: None,
        config: case
            .config
            .as_ref()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "{}".into()),
    }
}

/// Turn a node response into the captured fact bundle: a success emission fills
/// `node_output` (parsed) + `node_port` (absent → the literal `main`); an error
/// fills `node_error` with the frozen taxonomy kind.
fn capture_node(resp: &NodeInvokeResponse) -> Captured {
    match resp {
        NodeInvokeResponse::Ok(em) => {
            let node_output = match &em.payload {
                WirePayload::Inline(s) => serde_json::from_str(s).ok(),
            };
            Captured {
                node_output,
                node_port: Some(em.port.clone().unwrap_or_else(|| "main".into())),
                ..Default::default()
            }
        }
        NodeInvokeResponse::Err(e) => Captured {
            node_error: Some(wire_error_kind(e)),
            ..Default::default()
        },
    }
}

fn wire_error_kind(e: &WireNodeError) -> NodeErrorKind {
    match e {
        WireNodeError::Retryable(_) => NodeErrorKind::Retryable,
        WireNodeError::RateLimited(_) => NodeErrorKind::RateLimited,
        WireNodeError::Terminal(_) => NodeErrorKind::Terminal,
        WireNodeError::InvalidInput(_) => NodeErrorKind::InvalidInput,
        WireNodeError::Cancelled => NodeErrorKind::Cancelled,
    }
}

// ---------------------------------------------------------------------------
// flow-level: RunWorker under the test-double set (the runworker template)
// ---------------------------------------------------------------------------

async fn flow_phase(
    engine: &wash_runtime::engine::Engine,
    args: &TestKitBenchArgs,
    cases: &[&TestCase],
) -> anyhow::Result<bool> {
    println!(
        "\n## flow — {} case(s) drive poc-s6 under the test-double set (RunWorker + EgressRecorder)",
        cases.len()
    );

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    cfg.pool_max_size = args.pool_max;
    if cfg.database_url.is_none() {
        bail!("flow-level cases need a database url: --database-url or DATABASE_URL / WAMN_PG_URL");
    }
    let admin_url = args
        .admin_database_url
        .clone()
        .context("flow-level cases need --admin-database-url or WAMN_PG_ADMIN_URL")?;
    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read flowrunner {}", args.flowrunner.display()))?;

    // The real egress target for the poc-s6 http-call node (reuse testhostbench's
    // loopback echo — a 200-answering listener).
    let (echo_addr, echo_task) = crate::testhostbench::spawn_echo().await?;
    let echo_authority = format!("127.0.0.1:{}", echo_addr.port());
    let echo_url = format!("http://{echo_authority}/echo");

    // Provision the union schema (flow tables + run_queue) via the drift-guarded
    // runnerbench DDL, seed poc-s6 (delay 0 → drives straight through), and stage
    // a dispatched run + its queue row.
    let provisioner =
        EphemeralSchemaProvisioner::connect(&admin_url, crate::runnerbench::runner_ddl)
            .await
            .context("connect flow provisioner")?;
    provisioner
        .provision_case(RW_SCHEMA)
        .await
        .context("provision flow schema")?;
    let admin = provisioner.admin();
    scope_session(admin, RW_TENANT, RW_SCHEMA).await?;
    let flow_json = crate::flowbench::flow_json_s6(0, &echo_url);
    wamn_gate_harness::seed_flow_version(admin, RW_TENANT, "poc-s6", 1, true, &flow_json, true)
        .await?;
    let run_id = "tk-run-0";
    admin
        .execute(
            &write_ahead_triggered_run_sql(),
            &[&run_id, &"poc-s6", &1i32, &"cron", &"\"receipt\""],
        )
        .await
        .context("seed dispatched runs row")?;
    admin
        .execute(
            &enqueue_sql(),
            &[&run_id, &Option::<&str>::None, &0i32, &0i64],
        )
        .await
        .context("enqueue run_queue row")?;

    // Drive the production runner under the test-double set (virtual clock +
    // seeded random + a spying egress recorder that expects only the echo).
    let plugin = Arc::new(WamnPostgres::new(cfg.clone())?);
    let vault = Arc::new(WamnCredentials::empty());
    let recorder = Arc::new(EgressRecorder::spying());
    recorder.expect(RW_OWNER, [echo_authority.clone()]);
    let (doubles, _clock) = DoubleSet::virtual_host(
        TEST_EPOCH_SECS,
        TEST_SEED,
        recorder.clone() as Arc<dyn HostHandler>,
    );
    let mut worker = RunWorker::instantiate(
        engine,
        &guest,
        plugin.clone(),
        vault,
        Arc::new(WamnLogging::from_env()?),
        RunnerIdentity {
            owner: RW_OWNER,
            tenant: RW_TENANT,
            schema: Some(RW_SCHEMA),
            project: "default",
        },
        Arc::from([]),
        30_000,
        Some(doubles),
    )
    .await
    .context("instantiate RunWorker with the test double set")?;
    let report = worker.drain().await.context("drain run_queue")?;
    println!("flow drain: {report:?}, egress={:?}", recorder.records());

    // Base captured facts shared by every flow case: the run outcome (from the
    // drain report) and the egress audit log.
    let status = if report.failed > 0 {
        RunStatus::Failed
    } else if report.completed > 0 {
        RunStatus::Completed
    } else {
        RunStatus::Running
    };
    let egress = recorder.records();

    // DB-state asserts read through the ADMIN (superuser) session, scoped to the
    // runner's tenant + schema (RLS-bypassing — the documented distinction).
    scope_session(admin, RW_TENANT, RW_SCHEMA).await?;

    let mut ok = true;
    for case in cases {
        let mut captured = Captured {
            run: Some(RunFacts {
                status,
                fail_kind: None,
                fail_node: None,
            }),
            egress: egress.clone(),
            ..Default::default()
        };
        // Run each DB-state assertion's query via the admin pool and capture the
        // rows (each query selects a single json column — one object per row).
        for a in &case.expect {
            if let Assertion::DbState { query, params, .. } = a {
                let rows = admin_query_json(admin, query, params)
                    .await
                    .with_context(|| format!("db-state query for case {}", case.name))?;
                captured.db.push(DbCapture {
                    query: query.clone(),
                    params: params.clone(),
                    rows,
                });
            }
        }
        fold_outcome(&mut ok, &evaluate(case, &captured));
    }

    drop(worker);
    drop(plugin);
    provisioner.drop_case(RW_SCHEMA).await.ok();
    echo_task.abort();
    Ok(ok)
}

/// Run a DB-state query that SELECTs a single json column and collect one
/// [`serde_json::Value`] per row. String params bind as text (the only param
/// kind the v0 fixtures need).
async fn admin_query_json(
    admin: &tokio_postgres::Client,
    query: &str,
    params: &[serde_json::Value],
) -> anyhow::Result<Vec<serde_json::Value>> {
    let owned: Vec<String> = params
        .iter()
        .map(|p| {
            p.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| p.to_string())
        })
        .collect();
    let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = owned
        .iter()
        .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    let rows = admin.query(query, &refs).await?;
    Ok(rows
        .iter()
        .map(|r| r.get::<usize, serde_json::Value>(0))
        .collect())
}
