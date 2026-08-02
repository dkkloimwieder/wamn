//! `testkitbench` — the 11.4 assertion-library gate: cases-as-data driven
//! through the pure `wamn-scenario-model` vocabulary.
//!
//! A checked-in JSON fixture (`--cases`, a `Vec<TestCase>`) proves the
//! cases-as-data path (the 828 lane's catalog-jsonb store reads the identical
//! shape). Each case routes by its ref:
//!
//!   node-level (`node_ref`) — the gate warm-instantiates the node in a REAL
//!     [`ServeNode`] (the f2invoke template) and `.invoke()`s it with the case
//!     input/config, capturing the emission / port / error.
//!   flow-level (`flow_ref`) — the gate drives the flow under the test-double
//!     set (`ScenarioCapabilities::virtualized` + a spying `RecordingEgress` + the 9-arg
//!     [`ExecutionHost::instantiate`]) exactly as `testhostbench`'s runworker phase
//!     does, then captures the run outcome (from the
//!     [`DrainReport`](wamn_execution_host::DrainReport)), the egress
//!     log, and tenant-scoped application DB reads.
//!
//! Every captured fact bundle is folded through [`wamn_scenario_model::evaluate`] and
//! each [`AssertionResult`](wamn_scenario_model::AssertionResult) becomes a
//! [`wamn_gate_harness::check`] line — the library decides, the gate only drives.
//!
//! DB-state note: flow-level DB reads use a dedicated `wamn_app` connection
//! scoped to the runner's tenant + schema. Each stored assertion executes in a
//! fresh read-only transaction and is explicitly rolled back.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::HostHandler;

use crate::node_host_support::{self as serve_node, ServeNode, ServeNodeAuthn};
use wamn_execution_host::{ExecutionHost, ExecutionIdentity, injected_capabilities};
use wamn_gate_harness::{check, scope_session};
use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, WireNodeError, WirePayload, WireRunContext,
};
use wamn_run_state::queue::{enqueue_sql, write_ahead_triggered_run_sql};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_scenario_model::{
    Captured, NodeErrorKind, Outcome, RunFacts, RunStatus, TestCase, evaluate,
};
use wamn_scenario_runtime::{
    EphemeralSchemaProvisioner, RecordingEgress, ScenarioCapabilities, ScenarioSchemaName,
    capture_db_assertions,
};

/// The virtual-clock epoch + seed the flow-level test host uses (matching the
/// scenario-runtime defaults).
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

    /// Product stored-suite worker executable used by the compatibility adapter.
    #[arg(long, default_value = "/usr/local/bin/wamn-scenario-worker")]
    pub scenario_worker: PathBuf,

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
    /// `wamn_schema_control::impact::ImpactReport` suite tuples (the 12g auto-run input
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

    let stored_path = args.seed_demo || !args.suites.is_empty() || args.impact_report.is_some();
    if stored_path {
        return wamn_test_infrastructure::scenario_worker_gate::run(
            wamn_test_infrastructure::scenario_worker_gate::StoredSuiteGateArgs {
                worker: args.scenario_worker,
                flowrunner: args.flowrunner,
                database_url: args.database_url,
                admin_database_url: args.admin_database_url.context(
                    "stored-suite execution needs --admin-database-url or WAMN_PG_ADMIN_URL",
                )?,
                suites: args.suites,
                tenant: args.tenant,
                impact_report: args.impact_report,
                source_schema: args.source_schema,
                execution_schema_base: args.exec_schema,
                seed_demo: args.seed_demo,
                keep: args.keep,
            },
        )
        .await;
    }

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let result = run_file_cases(&engine, &args).await;
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
    for case in &cases {
        case.validate()
            .with_context(|| format!("validate case {:?} from {}", case.name, path.display()))?;
    }
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
    let mut ctx = case
        .ctx
        .clone()
        .map(wamn_scenario_catalog::compat::run_context_to_wire)
        .unwrap_or_else(|| default_ctx(case));
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
        context: "{}".into(),
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
// flow-level: ExecutionHost under the test-double set (the runworker template)
// ---------------------------------------------------------------------------

async fn flow_phase(
    engine: &wash_runtime::engine::Engine,
    args: &TestKitBenchArgs,
    cases: &[&TestCase],
) -> anyhow::Result<bool> {
    println!(
        "\n## flow — {} case(s) drive poc-s6 under the test-double set (ExecutionHost + RecordingEgress)",
        cases.len()
    );

    let mut cfg = WamnPostgresConfig::from_env();
    if let Some(url) = &args.database_url {
        cfg.database_url = Some(url.clone());
    }
    cfg.pool_max_size = args.pool_max;
    let observer_url = cfg.database_url.clone().context(
        "flow-level cases need a database url: --database-url or DATABASE_URL / WAMN_PG_URL",
    )?;
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
    let runworker_schema = ScenarioSchemaName::new(RW_SCHEMA)?;
    provisioner
        .provision_case(&runworker_schema)
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
    // seeded random + trusted outer/flow egress policy intersection).
    let plugin = Arc::new(WamnPostgres::new(cfg.clone())?);
    let vault = Arc::new(WamnCredentials::empty());
    let egress_policy = Arc::new(RunnerEgressPolicy::default());
    let recorder = Arc::new(RecordingEgress::spying(egress_policy.clone()));
    let allowed_hosts: Arc<[AllowedHost]> = vec![echo_authority.parse::<AllowedHost>()?].into();
    let (scenario, _clock) = ScenarioCapabilities::virtualized(
        TEST_EPOCH_SECS,
        TEST_SEED,
        recorder.clone() as Arc<dyn HostHandler>,
    );
    let mut worker = ExecutionHost::instantiate(
        engine,
        &guest,
        plugin.clone(),
        vault,
        Arc::new(WamnLogging::from_env()?),
        ExecutionIdentity {
            owner: RW_OWNER,
            tenant: RW_TENANT,
            schema: Some(runworker_schema.as_str()),
            project: "default",
        },
        injected_capabilities(scenario.wasi, scenario.egress, allowed_hosts, egress_policy),
        30_000,
    )
    .await
    .context("instantiate ExecutionHost with the test double set")?;
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

    // Observe through the same least-privileged application identity as the
    // scenario, independently scoped to its tenant and schema.
    let (mut observer, observer_connection) = tokio_postgres::connect(&observer_url, NoTls)
        .await
        .context("connect flow db-state observer")?;
    let observer_task = tokio::spawn(async move {
        let _ = observer_connection.await;
    });
    scope_session(&observer, RW_TENANT, RW_SCHEMA).await?;

    let mut ok = true;
    for case in cases {
        let captured = Captured {
            run: Some(RunFacts {
                status,
                fail_kind: None,
                fail_node: None,
            }),
            egress: egress.clone(),
            db: capture_db_assertions(&mut observer, case).await?,
            ..Default::default()
        };
        fold_outcome(&mut ok, &evaluate(case, &captured));
    }

    drop(observer);
    observer_task.abort();
    drop(worker);
    drop(plugin);
    provisioner.drop_case(&runworker_schema).await.ok();
    echo_task.abort();
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use wamn_scenario_model::{Assertion, AssertionResult};

    use super::{Outcome, fold_outcome};

    #[test]
    fn integration_gate_uses_scenario_runtime_without_importing_worker() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("wamn-scenario-runtime"));
        assert!(!manifest.contains("wamn-scenario-worker"));
    }

    #[test]
    fn aggregate_fold_turns_red_when_any_stored_assertion_fails() {
        let mut ok = true;
        fold_outcome(
            &mut ok,
            &Outcome {
                name: "stored failure".into(),
                results: vec![AssertionResult {
                    assertion: Assertion::Equals(serde_json::json!({"expected": true})),
                    passed: false,
                    detail: Some("captured output differed".into()),
                }],
            },
        );
        assert!(!ok, "a failed stored assertion must fail testkitbench");
    }
}
