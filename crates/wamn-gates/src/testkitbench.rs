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
use serde::Deserialize;
use wash_runtime::host::http::HostHandler;

use wamn_ctl::publish_catalog::{ensure_flow_registry, ensure_flow_tests, ensure_runstate};
use wamn_gate_harness::{check, scope_session, seed_flow_version, seed_test_case, seed_test_suite};
use wamn_host::doubles::{DoubleSet, EgressRecorder, EphemeralSchemaProvisioner, case_pool};
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
    Assertion, Captured, DbCapture, EgressAssertion, NodeErrorKind, Outcome, RunFacts, RunStatus,
    TestCase, evaluate,
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
    /// Path to the checked-in `Vec<TestCase>` JSON fixture (the 11.4 file-cases
    /// path). Optional: a `--suite`/`--impact-report` run needs no cases file;
    /// with NO selection source at all this falls back to the baked default.
    #[arg(long)]
    pub cases: Option<PathBuf>,

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

    // ---- 11.2-exec: stored-suite EXECUTOR selection (wamn-0lfu) ----
    /// Stored-suite selector `<flow_id>@<version>` (repeatable). Loads EVERY
    /// suite of that flow version from `--source-schema` and executes each stored
    /// case as its own run. Requires `--tenant`. Mutually exclusive with
    /// `--cases` / `--impact-report`.
    #[arg(long = "suite")]
    pub suites: Vec<String>,

    /// The tenant for `--suite` selection (the single-tenant stored path).
    #[arg(long)]
    pub tenant: Option<String>,

    /// The alternative stored-suite selection: a JSON array of `SuiteSelector`
    /// `{tenant, flow_id, flow_version, suite_id}` — the flattened
    /// `wamn_impact::ImpactReport` suite tuples (the 12g auto-run input
    /// contract). Mutually exclusive with `--cases` / `--suite`.
    #[arg(long)]
    pub impact_report: Option<PathBuf>,

    /// The schema holding `flows` + `test_suites` + `test_cases` to READ in the
    /// stored path (the in-cluster composition uses `poc_f1`). Read-only — never
    /// mutated; execution happens in a separate ephemeral schema.
    #[arg(long, default_value = "wamn_run")]
    pub source_schema: String,

    /// Base name of the ephemeral EXECUTION schema provisioned per case (a
    /// `_<n>` case suffix is appended). Dropped at case teardown.
    #[arg(long, default_value = "tk_suiteexec")]
    pub exec_schema: String,

    /// Hermetic gate-of-record: self-seed `--source-schema` (via the production
    /// `ensure_*` path) with a drivable no-egress demo flow + suite, then run the
    /// stored path against it. The `suiteexec-job.yaml` "SQL preamble" — no
    /// external fixture data. Requires `--tenant`.
    #[arg(long)]
    pub seed_demo: bool,

    /// Keep the `--seed-demo` source schema at the end (default drops it) — for
    /// running a follow-on `--impact-report` / `--suite` against the same seeded
    /// data during local iteration.
    #[arg(long)]
    pub keep: bool,
}

/// The stored-suite executor's `--impact-report` input row — a field-for-field
/// mirror of `wamn_impact::SuiteEdge` (`{tenant, flow_id, flow_version: i32,
/// suite_id}`). This is the well-defined SUBSET of an `ImpactReport` (its
/// flattened `entities[].suites[]` tuples) the 12g migrate-catalog auto-run seam
/// will emit. Kept a LOCAL deserialize type (wamn-impact has no serde derives
/// today and is out of this bead's scope) whose field names/types are pinned to
/// the `SuiteEdge` shape by `suite_selector_matches_the_suite_edge_shape`.
/// `flow_version` is `i32` (the SQL `int` column / `SuiteEdge`); the executor
/// casts it to the `u32` the `wamn-testkit` `FlowRef` uses at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteSelector {
    pub tenant: String,
    pub flow_id: String,
    pub flow_version: i32,
    pub suite_id: String,
}

/// The default file-cases fixture used when NO selection source is given (the
/// bare `testkitbench` invocation, preserved from the 11.4 behavior).
const DEFAULT_CASES: &str = "/bench/testkit-cases.json";

pub async fn run(args: TestKitBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    // Exactly one selection source among --cases / --suite / --impact-report.
    let selection_sources = [
        args.cases.is_some(),
        !args.suites.is_empty(),
        args.impact_report.is_some(),
    ];
    if selection_sources.iter().filter(|b| **b).count() > 1 {
        bail!(
            "exactly one selection source: --cases <file> | --suite <flow@version> | --impact-report <file>"
        );
    }
    if args.seed_demo
        && (args.cases.is_some() || args.impact_report.is_some() || !args.suites.is_empty())
    {
        bail!(
            "--seed-demo is standalone (it seeds + runs its own demo suite); do not combine with --cases / --suite / --impact-report"
        );
    }

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    let stored_path = args.seed_demo || !args.suites.is_empty() || args.impact_report.is_some();
    let result = if stored_path {
        run_stored_suites(&engine, &args).await
    } else {
        run_file_cases(&engine, &args).await
    };

    ticker.abort();
    let ok = result?;
    println!("\ntestkitbench complete — overall PASS: {ok}");
    if !ok {
        bail!("testkitbench gate failed");
    }
    Ok(())
}

/// The 11.4 file-cases path: load a `Vec<TestCase>` fixture and drive its
/// node-level + flow-level cases (unchanged behavior).
async fn run_file_cases(
    engine: &wash_runtime::engine::Engine,
    args: &TestKitBenchArgs,
) -> anyhow::Result<bool> {
    println!("# wamn-gates testkitbench — 11.4 assertion library (cases-as-data)");
    let path = args
        .cases
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CASES));
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read cases fixture {}", path.display()))?;
    let cases: Vec<TestCase> = serde_json::from_str(&raw)
        .with_context(|| format!("parse Vec<TestCase> from {}", path.display()))?;
    println!(
        "loaded {} case(s) from {} (cases-as-data path)",
        cases.len(),
        path.display()
    );

    let node_cases: Vec<&TestCase> = cases.iter().filter(|c| c.node_ref.is_some()).collect();
    let flow_cases: Vec<&TestCase> = cases.iter().filter(|c| c.flow_ref.is_some()).collect();

    let mut ok = true;
    if !node_cases.is_empty() {
        ok &= node_phase(engine, &args.node, &node_cases).await?;
    }
    if !flow_cases.is_empty() {
        ok &= flow_phase(engine, args, &flow_cases).await?;
    }
    Ok(ok)
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

// ===========================================================================
// 11.2-exec (wamn-0lfu): the stored-suite EXECUTOR
//
// Loads `test_suites` / `test_cases` rows from Postgres (validated against the
// `wamn-testkit` vocabulary on READ) and executes each stored case as its OWN
// run through the t92 doubles seam — a FRESH ephemeral schema per case (the
// source schema is read-only), the graph read from `{source_schema}.flows`,
// `DoubleSet::virtual_host` + `EgressRecorder` + `RunWorker` + drain, then
// `wamn_testkit::evaluate` per case. Selection is `--suite <flow@version>`
// (single-tenant, all suites of the version) OR `--impact-report` (a JSON array
// of `SuiteSelector`, the flattened `ImpactReport` tuples the 12g auto-run seam
// will emit).
//
// PER-CASE ISOLATION DECISION: a fresh exec schema per CASE (the brief's primary
// recommendation) — each case's db-state asserts see only its own run's writes,
// with no cross-case contamination. Suite sizes here are small (a handful of
// cases); `provision_case` drops+recreates the ~5-table runner_ddl union, which
// measures sub-second per case locally, so per-case provisioning is not a
// bottleneck. If a future suite is large enough that this dominates, fall back
// to one exec schema per SUITE with unique run ids (drain processes one run at a
// time) — measure first.
//
// RLS POSTURE: the source suite/graph rows are read via the ADMIN (superuser,
// RLS-bypassing) session with an EXPLICIT `(tenant, flow_id, flow_version
// [, suite_id])` WHERE predicate — matching the impact/cross-tenant read model,
// and the executor already needs admin for provisioning + db-state asserts. The
// `deploy/sql/flow-tests.sql` FORCE-RLS floor is UNTOUCHED; this admin read
// BYPASS is deliberate and guarded by the explicit tenant predicate (the running
// FLOW still exercises the app-role RLS floor via the `wamn_app` pool).
// ===========================================================================

/// The demo flow the `--seed-demo` hermetic gate seeds + runs.
const DEMO_FLOW_ID: &str = "tk-demo-flow";
const DEMO_SUITE_ID: &str = "demo";

/// The flowrunner guest's BUILT-IN dispatch arms (`components/flowrunner`
/// `dispatch_node`) — the node types the doubles path drives directly, BEYOND
/// the standard node library (which the guest delegates to
/// `wamn_nodes::is_standard`). Curated here because the guest is a wasm crate
/// (not a host dep); drift-guarded against the guest source by
/// `builtin_node_types_pinned_against_the_guest`.
const BUILTIN_NODE_TYPES: &[&str] = &[
    "webhook-in",
    "transform",
    "conditional",
    "respond",
    "pg-write",
    "delay",
    "http-call",
    "custom",
];

/// The standard node library (`wamn-nodes`) types the guest delegates to via
/// `wamn_nodes::is_standard`. Drift-guarded against `crates/wamn-nodes/src/lib.rs`
/// `NODE_TYPES` (name + count) by `standard_node_types_pinned_against_wamn_nodes`.
const STANDARD_NODE_TYPES: &[&str] = &[
    "transform",
    "conditional",
    "time-shift",
    "http-request",
    "postgres",
    "postgres-query",
    "respond",
];

/// Whether the doubles path can dispatch `node_type` at all — the union of the
/// flowrunner built-in arms and the standard node library. A type outside this
/// set hits the guest's `other => Err("unknown node type")` arm, so the executor
/// REFUSES a flow carrying one (rather than crashing mid-drive).
fn is_drivable(node_type: &str) -> bool {
    BUILTIN_NODE_TYPES.contains(&node_type) || STANDARD_NODE_TYPES.contains(&node_type)
}

/// The sorted-unique undrivable node types in a flow graph — the drivability
/// refusal message's payload. Reads the `nodes[].type` array only (the trigger
/// type, e.g. `cron`, is not a node and is not checked).
fn undrivable_node_types(graph: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = graph
        .get("nodes")
        .and_then(|n| n.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n.get("type").and_then(|t| t.as_str()))
                .filter(|t| !is_drivable(t))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// The egress authorities a case's OWN egress assertions name (the
/// `ExactlyThese` / `Includes` matcher `authority` fields) — the allowlist the
/// spy recorder is primed with, so the flow's declared calls forward and every
/// other authority is recorded + denied.
fn expected_authorities(case: &TestCase) -> Vec<String> {
    let mut out = Vec::new();
    for a in &case.expect {
        if let Assertion::Egress { calls, .. } = a {
            let matchers = match calls {
                EgressAssertion::ExactlyThese(m) | EgressAssertion::Includes(m) => m.as_slice(),
                _ => &[][..],
            };
            for m in matchers {
                if let Some(auth) = &m.authority {
                    out.push(auth.clone());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A bare lowercase SQL identifier (a schema name is interpolated into DDL).
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The `SuiteEdge` `flow_version` boundary: `i32` (the SQL `int` column) → the
/// `u32` `wamn-testkit` `FlowRef` uses. A negative version is invalid.
fn u32_from_version(v: i32) -> anyhow::Result<u32> {
    u32::try_from(v).map_err(|_| anyhow::anyhow!("flow_version must be non-negative, got {v}"))
}

/// Parse a `--suite` selector `<flow_id>@<version>` (the `@` splits from the
/// RIGHT so a flow id may itself contain `@`).
fn parse_flow_at_version(s: &str) -> anyhow::Result<(String, i32)> {
    let (flow, ver) = s
        .rsplit_once('@')
        .with_context(|| format!("--suite must be <flow_id>@<version>: {s:?}"))?;
    if flow.is_empty() {
        bail!("--suite flow_id is empty: {s:?}");
    }
    let version: i32 = ver
        .parse()
        .with_context(|| format!("--suite version must be an integer: {s:?}"))?;
    u32_from_version(version)?; // reject a negative version at the boundary
    Ok((flow.to_string(), version))
}

/// A stored-suite run target before enumeration: a whole flow version (every
/// suite — `--suite` / `--seed-demo`) or one concrete suite tuple
/// (`--impact-report`).
enum SuiteTarget {
    Flow {
        tenant: String,
        flow_id: String,
        flow_version: i32,
    },
    Suite(SuiteSelector),
}

/// Build the run targets from the selection args (run() has already enforced
/// exactly-one selection source).
fn build_targets(args: &TestKitBenchArgs) -> anyhow::Result<Vec<SuiteTarget>> {
    if args.seed_demo {
        let tenant = args.tenant.clone().context("--seed-demo needs --tenant")?;
        return Ok(vec![SuiteTarget::Flow {
            tenant,
            flow_id: DEMO_FLOW_ID.to_string(),
            flow_version: 1,
        }]);
    }
    if !args.suites.is_empty() {
        let tenant = args
            .tenant
            .clone()
            .context("--suite needs --tenant (the tenant that owns the stored suites)")?;
        let mut targets = Vec::new();
        for s in &args.suites {
            let (flow_id, flow_version) = parse_flow_at_version(s)?;
            targets.push(SuiteTarget::Flow {
                tenant: tenant.clone(),
                flow_id,
                flow_version,
            });
        }
        return Ok(targets);
    }
    if let Some(path) = &args.impact_report {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read impact-report {}", path.display()))?;
        let selectors: Vec<SuiteSelector> = serde_json::from_str(&raw)
            .with_context(|| format!("parse Vec<SuiteSelector> from {}", path.display()))?;
        let mut targets = Vec::new();
        for sel in selectors {
            u32_from_version(sel.flow_version)?; // boundary check per tuple
            targets.push(SuiteTarget::Suite(sel));
        }
        return Ok(targets);
    }
    bail!("no stored-suite selection source (bug: run() routed here without one)")
}

/// The stored-suite executor entry point (`--suite` / `--impact-report` /
/// `--seed-demo`).
async fn run_stored_suites(
    engine: &wash_runtime::engine::Engine,
    args: &TestKitBenchArgs,
) -> anyhow::Result<bool> {
    println!("# wamn-gates testkitbench — 11.2-exec stored-suite executor (t92 doubles)");

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    cfg.pool_max_size = args.pool_max;
    if cfg.database_url.is_none() {
        bail!(
            "stored-suite execution needs a wamn_app url: --database-url or DATABASE_URL / WAMN_PG_URL"
        );
    }
    let admin_url = args
        .admin_database_url
        .clone()
        .context("stored-suite execution needs --admin-database-url or WAMN_PG_ADMIN_URL")?;
    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read flowrunner {}", args.flowrunner.display()))?;

    if !is_bare_ident(&args.source_schema) {
        bail!(
            "--source-schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.source_schema
        );
    }
    if !is_bare_ident(&args.exec_schema) {
        bail!(
            "--exec-schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.exec_schema
        );
    }

    // The admin (superuser) session: provisions the per-case EXEC schemas from
    // the runner_ddl union, reads the source rows (explicit-tenant WHERE), seeds
    // runs, and runs the db-state asserts.
    let provisioner =
        EphemeralSchemaProvisioner::connect(&admin_url, crate::runnerbench::runner_ddl)
            .await
            .context("connect stored-suite provisioner")?;
    let admin = provisioner.admin();

    // The suiteexec gate of record: self-seed the source schema hermetically.
    if args.seed_demo {
        let tenant = args
            .tenant
            .as_deref()
            .context("--seed-demo needs --tenant")?;
        seed_demo(admin, &args.source_schema, tenant).await?;
    }

    // Resolve the selection to concrete (tenant, flow_id, flow_version, suite_id)
    // tuples: a Flow target enumerates the version's suites; a Suite target is
    // already concrete.
    let mut selectors: Vec<SuiteSelector> = Vec::new();
    for target in build_targets(args)? {
        match target {
            SuiteTarget::Flow {
                tenant,
                flow_id,
                flow_version,
            } => {
                scope_session(admin, &tenant, &args.source_schema).await?;
                let rows = admin
                    .query(
                        &wamn_flow_tests::sql::select_suites_for_flow_sql(),
                        &[&tenant, &flow_id, &flow_version],
                    )
                    .await
                    .context("enumerate suites for flow")?;
                if rows.is_empty() {
                    println!(
                        "  [WARN] no suites for {flow_id}@{flow_version} (tenant {tenant}) in {}",
                        args.source_schema
                    );
                }
                for r in rows {
                    selectors.push(SuiteSelector {
                        tenant: tenant.clone(),
                        flow_id: flow_id.clone(),
                        flow_version,
                        suite_id: r.get::<usize, String>(0),
                    });
                }
            }
            SuiteTarget::Suite(sel) => selectors.push(sel),
        }
    }
    println!(
        "selected {} suite(s) from source schema {}",
        selectors.len(),
        args.source_schema
    );

    let mut ok = true;
    let (mut executed, mut skipped, mut refused) = (0usize, 0usize, 0usize);
    let mut case_counter = 0usize;

    for sel in &selectors {
        let SuiteSelector {
            tenant,
            flow_id,
            flow_version,
            suite_id,
        } = sel;
        println!("\n## suite {suite_id} — flow {flow_id}@{flow_version} (tenant {tenant})");

        // --- read the flow GRAPH from the source schema (read-only) ---
        scope_session(admin, tenant, &args.source_schema).await?;
        let graph_row = admin
            .query_opt(
                "SELECT graph_json::text FROM flows \
                 WHERE tenant_id = $1 AND flow_id = $2 AND version = $3",
                &[tenant, flow_id, flow_version],
            )
            .await
            .context("read flow graph")?;
        let Some(graph_row) = graph_row else {
            check(
                &mut ok,
                &format!(
                    "{suite_id} :: flow graph {flow_id}@{flow_version} not found in {}",
                    args.source_schema
                ),
                false,
            );
            continue;
        };
        let graph_json: String = graph_row.get(0);
        let graph: serde_json::Value =
            serde_json::from_str(&graph_json).context("parse flow graph_json")?;

        // --- DRIVABILITY refusal (load-bearing cross-lane contract) ---
        let undrivable = undrivable_node_types(&graph);
        if !undrivable.is_empty() {
            println!(
                "  [SKIP] suite {suite_id} REFUSED — flow {flow_id} carries undrivable node type(s): \
                 {} (the doubles path drives only the flowrunner std node set; a guest-baked flow is \
                 out of scope for the executor)",
                undrivable.join(", ")
            );
            refused += 1;
            continue;
        }

        // --- read the suite's cases (validated on read, below) ---
        let case_rows = admin
            .query(
                &wamn_flow_tests::sql::select_cases_for_suite_sql(),
                &[tenant, flow_id, flow_version, suite_id],
            )
            .await
            .context("read suite cases")?;
        if case_rows.is_empty() {
            println!("  [WARN] suite {suite_id} has no cases");
        }

        for row in &case_rows {
            let case_id: String = row.get(0);
            let case_body: String = row.get(2);
            case_counter += 1;

            // --- validate-on-read: parse the body against the vocabulary ---
            let case: TestCase = match serde_json::from_str(&case_body) {
                Ok(c) => c,
                Err(e) => {
                    check(
                        &mut ok,
                        &format!(
                            "{suite_id}/{case_id} :: case_body is not a valid wamn-testkit TestCase — {e}"
                        ),
                        false,
                    );
                    continue;
                }
            };

            // Stored node-level case: no compiled node artifact in the catalog.
            if case.node_ref.is_some() && case.flow_ref.is_none() {
                println!(
                    "  [SKIP] {suite_id}/{case_id} — stored node-level case not executable without a \
                     node artifact (node execution stays the --node/--cases path)"
                );
                skipped += 1;
                continue;
            }

            // --- coherence guard: the body's flow_ref must agree with the row ---
            let Some(flow_ref) = &case.flow_ref else {
                check(
                    &mut ok,
                    &format!("{suite_id}/{case_id} :: case has neither flow-ref nor node-ref"),
                    false,
                );
                continue;
            };
            if &flow_ref.flow_id != flow_id || flow_ref.version as i32 != *flow_version {
                check(
                    &mut ok,
                    &format!(
                        "{suite_id}/{case_id} :: COHERENCE — case flow-ref {}@{} disagrees with the \
                         stored row {flow_id}@{flow_version}",
                        flow_ref.flow_id, flow_ref.version
                    ),
                    false,
                );
                continue;
            }

            // --- execute the case as its OWN run in a FRESH ephemeral schema ---
            let exec_schema = format!("{}_{}", args.exec_schema, case_counter);
            let case_ok = drive_stored_case(
                engine,
                &guest,
                &cfg,
                &provisioner,
                &exec_schema,
                tenant,
                flow_id,
                *flow_version,
                &graph_json,
                suite_id,
                &case_id,
                &case,
            )
            .await?;
            ok &= case_ok;
            executed += 1;
        }
    }

    println!(
        "\n# stored-suite summary — {executed} case(s) executed, {skipped} skipped, \
         {refused} suite(s) refused; PASS so far: {ok}"
    );

    // The --seed-demo source schema is throwaway: drop it on the way out (the
    // per-case exec schemas are already dropped at case teardown), unless --keep.
    if args.seed_demo && !args.keep {
        admin
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {} CASCADE",
                args.source_schema
            ))
            .await
            .ok();
    }
    Ok(ok)
}

/// Drive ONE stored case as its own run: fresh exec schema (runner_ddl) ← the
/// real graph + a run carrying the case's trigger input; `DoubleSet` +
/// `EgressRecorder` (allowlist from the case's own egress asserts) + `RunWorker`
/// + drain; then `evaluate` the case against the captured run/egress/db facts.
#[allow(clippy::too_many_arguments)]
async fn drive_stored_case(
    engine: &wash_runtime::engine::Engine,
    guest: &[u8],
    cfg: &WamnPostgresConfig,
    provisioner: &EphemeralSchemaProvisioner,
    exec_schema: &str,
    tenant: &str,
    flow_id: &str,
    flow_version: i32,
    graph_json: &str,
    suite_id: &str,
    case_id: &str,
    case: &TestCase,
) -> anyhow::Result<bool> {
    let admin = provisioner.admin();

    // A FRESH execution schema (the runner_ddl union) — never the source schema.
    provisioner
        .provision_case(exec_schema)
        .await
        .with_context(|| format!("provision exec schema {exec_schema}"))?;
    scope_session(admin, tenant, exec_schema).await?;
    seed_flow_version(admin, tenant, flow_id, flow_version, true, graph_json, true)
        .await
        .context("seed flow graph into exec schema")?;

    // One run carrying the case's trigger input.
    let run_id = format!("tk-{suite_id}-{case_id}");
    let input_text = case.input.to_string();
    admin
        .execute(
            &write_ahead_triggered_run_sql(),
            &[&run_id, &flow_id, &flow_version, &"cron", &input_text],
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

    // The egress double: the case's OWN egress asserts name the allowlist; owner
    // == flow_id (the suite-authoring convention). Spy: everything else is
    // recorded + denied.
    let recorder = Arc::new(EgressRecorder::spying());
    recorder.expect(flow_id, expected_authorities(case));
    let (doubles, _clock) = DoubleSet::virtual_host(
        TEST_EPOCH_SECS,
        TEST_SEED,
        recorder.clone() as Arc<dyn HostHandler>,
    );

    // A FRESH app pool per case (prepared-plan isolation across exec schemas).
    let plugin = case_pool(cfg, tenant, exec_schema, flow_id).context("build case pool")?;
    let vault = Arc::new(WamnCredentials::empty());
    let mut worker = RunWorker::instantiate(
        engine,
        guest,
        plugin.clone(),
        vault,
        Arc::new(WamnLogging::from_env()?),
        RunnerIdentity {
            owner: flow_id,
            tenant,
            schema: Some(exec_schema),
            project: "default",
        },
        Arc::from([]),
        30_000,
        Some(doubles),
    )
    .await
    .context("instantiate RunWorker with the test double set")?;
    let report = worker.drain().await.context("drain run_queue")?;

    // The run outcome (from the drain report, as flow_phase derives it) + egress
    // audit log. fail_kind/fail_node stay None (a v0 limitation: a stored case
    // constraining them would need the runs-row read; see build-and-test.md).
    let status = if report.failed > 0 {
        RunStatus::Failed
    } else if report.completed > 0 {
        RunStatus::Completed
    } else {
        RunStatus::Running
    };
    let egress = recorder.records();
    println!(
        "  case {case_id}: drain={report:?}, status={status:?}, egress={} call(s)",
        egress.len()
    );

    // DB-state asserts read through the admin session, scoped to the exec schema.
    scope_session(admin, tenant, exec_schema).await?;
    let mut captured = Captured {
        run: Some(RunFacts {
            status,
            fail_kind: None,
            fail_node: None,
        }),
        egress,
        ..Default::default()
    };
    for a in &case.expect {
        if let Assertion::DbState { query, params, .. } = a {
            let rows = admin_query_json(admin, query, params)
                .await
                .with_context(|| format!("db-state query for {suite_id}/{case_id}"))?;
            captured.db.push(DbCapture {
                query: query.clone(),
                params: params.clone(),
                rows,
            });
        }
    }

    let mut ok = true;
    fold_outcome(&mut ok, &evaluate(case, &captured));

    drop(worker);
    drop(plugin);
    provisioner.drop_case(exec_schema).await.ok();
    Ok(ok)
}

/// The `--seed-demo` bootstrap: provision `source_schema` (the production
/// `ensure_*` path) with a drivable NO-EGRESS demo flow
/// (`webhook-in -> pg-write -> respond`) + a 2-case suite, so the stored path
/// runs hermetically with no external fixture data and no live egress target.
async fn seed_demo(
    admin: &tokio_postgres::Client,
    source_schema: &str,
    tenant: &str,
) -> anyhow::Result<()> {
    println!(
        "## --seed-demo: provisioning source schema {source_schema} + demo flow/suite (tenant {tenant})"
    );
    // Fresh source schema + the wamn_app role (idempotent) — mirrors suiteproof.
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {source_schema} CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
               CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
             END IF; END $$;"
        ))
        .await
        .context("reset source schema + ensure wamn_app role")?;
    // run-state creates the schema; flow-registry adds flows; flow-tests FKs
    // into flows — this ORDER matters (the suiteproof precedent).
    ensure_runstate(admin, source_schema)
        .await
        .context("ensure run-state")?;
    ensure_flow_registry(admin, source_schema)
        .await
        .context("ensure flow registry")?;
    ensure_flow_tests(admin, source_schema)
        .await
        .context("ensure flow-test tables")?;

    scope_session(admin, tenant, source_schema).await?;
    seed_flow_version(
        admin,
        tenant,
        DEMO_FLOW_ID,
        1,
        true,
        &demo_graph_json(),
        true,
    )
    .await
    .context("seed demo flow")?;
    seed_test_suite(
        admin,
        tenant,
        DEMO_FLOW_ID,
        1,
        DEMO_SUITE_ID,
        "stored-suite executor demo",
    )
    .await
    .context("seed demo suite")?;
    for (case_id, ordinal, body) in demo_cases() {
        seed_test_case(
            admin,
            tenant,
            DEMO_FLOW_ID,
            1,
            DEMO_SUITE_ID,
            &case_id,
            ordinal,
            &body,
        )
        .await
        .context("seed demo case")?;
    }
    Ok(())
}

/// A drivable, NO-EGRESS demo flow: `webhook-in -> pg-write -> respond` (drives
/// straight to completion with no live egress target — hermetic).
fn demo_graph_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{DEMO_FLOW_ID}","version":1,
            "trigger":{{"type":"webhook"}},"entry":"in",
            "nodes":[
              {{"id":"in","type":"webhook-in"}},
              {{"id":"w","type":"pg-write"}},
              {{"id":"out","type":"respond"}}
            ],
            "edges":[{{"from":"in","to":"w"}},{{"from":"w","to":"out"}}]}}"#
    )
}

/// The demo suite's cases (validated `wamn-testkit` TestCase bodies, mirroring
/// what pin-run writes): a plain completion, and a completion + a db-state assert
/// that the run's `pg-write` reached `sink` (fresh exec schema ⇒ exactly 1 row).
fn demo_cases() -> Vec<(String, i32, String)> {
    let completes = serde_json::json!({
        "schema-version": "0.1",
        "name": "demo-completes",
        "flow-ref": { "flow-id": DEMO_FLOW_ID, "version": 1 },
        "input": { "receipt": "demo" },
        "expect": [ { "run-outcome": { "status": "completed" } } ]
    })
    .to_string();
    let writes_sink = serde_json::json!({
        "schema-version": "0.1",
        "name": "demo-writes-sink",
        "flow-ref": { "flow-id": DEMO_FLOW_ID, "version": 1 },
        "input": { "receipt": "demo-2" },
        "expect": [
            { "run-outcome": { "status": "completed" } },
            { "db-state": { "query": "SELECT to_jsonb(sink) FROM sink", "params": [], "expect": { "row-count": 1 } } }
        ]
    })
    .to_string();
    vec![
        ("demo-completes".to_string(), 0, completes),
        ("demo-writes-sink".to_string(), 1, writes_sink),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wamn_testkit::AssertionResult;

    /// The `--impact-report` input row is the `wamn_impact::SuiteEdge` shape
    /// field-for-field: names `tenant / flow_id / flow_version / suite_id`, types
    /// `String / String / i32 / String`. A wrong field name or an extra field is
    /// REFUSED (the 12g input contract stays locked). (Mutant: impact-tuple
    /// parse field-swap.)
    #[test]
    fn suite_selector_matches_the_suite_edge_shape() {
        // The exact SuiteEdge shape parses, with the right values/types.
        let sel: SuiteSelector = serde_json::from_value(json!({
            "tenant": "t", "flow_id": "f", "flow_version": 3, "suite_id": "s"
        }))
        .expect("SuiteEdge-shaped JSON parses");
        assert_eq!(
            sel,
            SuiteSelector {
                tenant: "t".into(),
                flow_id: "f".into(),
                flow_version: 3,
                suite_id: "s".into(),
            }
        );
        // A camelCase / renamed field (`flow` for flow_id, `version` for
        // flow_version, `flowId`) drops a REQUIRED field ⇒ parse fails.
        for wrong in [
            json!({"tenant":"t","flow":"f","flow_version":1,"suite_id":"s"}),
            json!({"tenant":"t","flow_id":"f","version":1,"suite_id":"s"}),
            json!({"tenant":"t","flowId":"f","flowVersion":1,"suiteId":"s"}),
        ] {
            assert!(
                serde_json::from_value::<SuiteSelector>(wrong.clone()).is_err(),
                "a wrong field name must be refused: {wrong}"
            );
        }
        // An EXTRA field is refused (deny_unknown_fields locks the tuple).
        assert!(
            serde_json::from_value::<SuiteSelector>(json!({
                "tenant":"t","flow_id":"f","flow_version":1,"suite_id":"s","extra":1
            }))
            .is_err(),
            "an extra field must be refused"
        );
    }

    /// The i32→u32 `flow_version` boundary: a non-negative value casts; a
    /// negative one is rejected.
    #[test]
    fn flow_version_i32_to_u32_boundary() {
        assert_eq!(u32_from_version(0).unwrap(), 0);
        assert_eq!(u32_from_version(7).unwrap(), 7);
        assert_eq!(u32_from_version(i32::MAX).unwrap(), i32::MAX as u32);
        assert!(u32_from_version(-1).is_err());
    }

    /// `--suite <flow@version>` splits from the right and validates the version.
    #[test]
    fn parse_flow_at_version_splits_and_validates() {
        assert_eq!(
            parse_flow_at_version("receipt-received@2").unwrap(),
            ("receipt-received".to_string(), 2)
        );
        // Rightmost `@` wins (a flow id may contain `@`).
        assert_eq!(
            parse_flow_at_version("a@b@3").unwrap(),
            ("a@b".to_string(), 3)
        );
        assert!(parse_flow_at_version("noversion").is_err());
        assert!(parse_flow_at_version("f@notanint").is_err());
        assert!(parse_flow_at_version("f@-1").is_err());
        assert!(parse_flow_at_version("@1").is_err());
    }

    /// The per-case fail aggregation: ANY failing assertion flips overall PASS →
    /// FAIL; an all-pass outcome leaves it true. (Mutant: fold aggregation — an
    /// OR-instead-of-AND, or last-only, fold fails this.)
    #[test]
    fn fold_outcome_flips_ok_on_any_failing_assertion() {
        let mut ok = true;
        let outcome = Outcome {
            name: "c".into(),
            results: vec![
                AssertionResult {
                    assertion: Assertion::Port("main".into()),
                    passed: true,
                    detail: None,
                },
                AssertionResult {
                    assertion: Assertion::Port("x".into()),
                    passed: false,
                    detail: Some("wrong port".into()),
                },
            ],
        };
        fold_outcome(&mut ok, &outcome);
        assert!(!ok, "a failing assertion must flip overall ok to false");

        let mut ok2 = true;
        fold_outcome(
            &mut ok2,
            &Outcome {
                name: "c".into(),
                results: vec![AssertionResult {
                    assertion: Assertion::Port("main".into()),
                    passed: true,
                    detail: None,
                }],
            },
        );
        assert!(ok2, "an all-pass outcome keeps ok true");
    }

    /// Drivability: F1's guest-baked family is REFUSED; the flowrunner std +
    /// built-in set is drivable. (Mutant: drivability check inverted flips both.)
    #[test]
    fn drivability_refuses_guest_baked_and_accepts_std_and_builtin() {
        for t in [
            "validate-receipt",
            "upsert-receipt",
            "evaluate-specs",
            "create-holds",
        ] {
            assert!(!is_drivable(t), "{t} must be refused (guest-baked)");
        }
        for t in [
            "webhook-in",
            "pg-write",
            "respond",
            "delay",
            "http-call",
            "custom",
            "transform",
            "conditional",
            "time-shift",
            "http-request",
            "postgres",
            "postgres-query",
        ] {
            assert!(is_drivable(t), "{t} must be drivable");
        }
    }

    /// The drivability refusal names EXACTLY the undrivable node types in a
    /// graph, sorted-unique; a fully-drivable graph yields none.
    #[test]
    fn undrivable_node_types_names_the_guest_baked_family() {
        let f1 = json!({
            "nodes": [
                {"id":"v","type":"validate-receipt"},
                {"id":"u","type":"upsert-receipt"},
                {"id":"r","type":"respond"}
            ]
        });
        assert_eq!(
            undrivable_node_types(&f1),
            vec!["upsert-receipt".to_string(), "validate-receipt".to_string()]
        );
        let s6 = json!({
            "nodes": [
                {"id":"in","type":"webhook-in"},
                {"id":"w","type":"pg-write"},
                {"id":"out","type":"respond"}
            ]
        });
        assert!(undrivable_node_types(&s6).is_empty());
    }

    /// The egress allowlist is derived from the case's own egress assertions
    /// (ExactlyThese/Includes authorities), sorted-unique.
    #[test]
    fn expected_authorities_derives_from_egress_assertions() {
        let case: TestCase = serde_json::from_value(json!({
            "name": "e",
            "flow-ref": {"flow-id": "f", "version": 1},
            "input": {},
            "expect": [
                {"egress": {"flow": "f", "calls": {"exactly-these": [
                    {"method":"GET","authority":"erp.example:443"},
                    {"authority":"erp.example:443"}
                ]}}},
                {"egress": {"flow": "f", "calls": {"includes": [
                    {"authority":"notify.example:443"}
                ]}}},
                {"run-outcome": {"status": "completed"}}
            ]
        }))
        .unwrap();
        assert_eq!(
            expected_authorities(&case),
            vec![
                "erp.example:443".to_string(),
                "notify.example:443".to_string()
            ]
        );
        // A no-egress case yields an empty allowlist (deny-all under spy).
        let plain: TestCase = serde_json::from_value(json!({
            "name": "p", "flow-ref": {"flow-id":"f","version":1}, "input": {},
            "expect": [{"run-outcome": {"status":"completed"}}]
        }))
        .unwrap();
        assert!(expected_authorities(&plain).is_empty());
    }

    /// Drift guard: the curated built-in node arms are pinned against the
    /// flowrunner guest source (its `dispatch_node` match). If the guest renames
    /// or removes an arm this fails, forcing the curated list — and the
    /// drivability refusal it powers — back into agreement.
    #[test]
    fn builtin_node_types_pinned_against_the_guest() {
        let guest = include_str!("../../../components/flowrunner/src/lib.rs");
        for t in BUILTIN_NODE_TYPES {
            assert!(
                guest.contains(&format!("\"{t}\"")),
                "flowrunner guest dropped built-in node arm {t:?}"
            );
        }
        // The guest delegates the standard library set + refuses unknown types —
        // the two behaviors the drivability check relies on.
        assert!(
            guest.contains("wamn_nodes::is_standard"),
            "guest no longer delegates the standard node set"
        );
        assert!(
            guest.contains("unknown node type"),
            "guest no longer refuses unknown node types (drivability refusal rests on this)"
        );
    }

    /// Drift guard: the curated standard set is pinned against `wamn-nodes`
    /// `NODE_TYPES` — both the names AND the array LENGTH, so a NEW standard type
    /// (a bumped `[&str; N]`) breaks this until `STANDARD_NODE_TYPES` catches up.
    #[test]
    fn standard_node_types_pinned_against_wamn_nodes() {
        let nodes = include_str!("../../../crates/wamn-nodes/src/lib.rs");
        assert!(
            nodes.contains(&format!(
                "NODE_TYPES: [&str; {}]",
                STANDARD_NODE_TYPES.len()
            )),
            "wamn-nodes NODE_TYPES length drifted from STANDARD_NODE_TYPES ({})",
            STANDARD_NODE_TYPES.len()
        );
        for t in STANDARD_NODE_TYPES {
            assert!(
                nodes.contains(&format!("\"{t}\"")),
                "wamn-nodes NODE_TYPES no longer lists {t:?}"
            );
        }
    }

    /// The demo suite the `--seed-demo` gate seeds is drivable + valid: its graph
    /// has no undrivable node type, and each case body is a valid, coherent
    /// wamn-testkit TestCase (a broken demo fails here, not only against PG).
    #[test]
    fn seed_demo_graph_and_cases_are_drivable_and_valid() {
        let graph: serde_json::Value = serde_json::from_str(&demo_graph_json()).unwrap();
        assert!(undrivable_node_types(&graph).is_empty());
        for (case_id, _ord, body) in demo_cases() {
            let case: TestCase = serde_json::from_str(&body)
                .unwrap_or_else(|e| panic!("demo case {case_id} invalid: {e}"));
            let flow_ref = case.flow_ref.expect("demo case is flow-level");
            assert_eq!(flow_ref.flow_id, DEMO_FLOW_ID);
            assert_eq!(flow_ref.version, 1);
        }
    }

    #[test]
    fn is_bare_ident_rejects_injection() {
        assert!(is_bare_ident("wamn_suiteexec"));
        assert!(is_bare_ident("tk_suiteexec_12"));
        assert!(!is_bare_ident("a; DROP"));
        assert!(!is_bare_ident("Cap"));
        assert!(!is_bare_ident("9lead"));
    }
}
