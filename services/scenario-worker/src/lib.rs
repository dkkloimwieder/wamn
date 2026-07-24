//! Product composition for executing stored deterministic flow scenarios.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use serde::Serialize;
use tokio_postgres::NoTls;

use wamn_execution_host::{
    DEFAULT_FLOWRUNNER_PATH, ExecutionHost, ExecutionIdentity, injected_capabilities,
};
use wamn_run_state::queue::{enqueue_sql, write_ahead_triggered_run_sql};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::WamnPostgresConfig;
use wamn_scenario_model::{
    Assertion, Captured, DbCapture, EgressAssertion, FailKind, Outcome, RunFacts, RunStatus,
    TestCase, evaluate,
};
use wamn_scenario_runtime::{
    RUN_QUEUE_DUE_NUDGE_SQL, RUN_QUEUE_NEXT_WAKE_SQL, RecordingEgress, ScenarioCapabilities,
    ScenarioScheduler, SchedulerBackend, case_pool, load_scenario_credentials,
};

const DEFAULT_EPOCH_SECS: u64 = 1_700_000_000;
const DEFAULT_RANDOM_SEED: u64 = 0x7492_5EED_5EED_7492;

/// Scenario-worker configuration.
#[derive(Debug, Args)]
pub struct ScenarioWorkerArgs {
    /// Path to the same compiled flowrunner component used by serving.
    #[arg(long, default_value = DEFAULT_FLOWRUNNER_PATH)]
    pub flowrunner: PathBuf,

    /// App database URL. Overrides WAMN_PG_URL and DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Tenant whose stored suite is executed.
    #[arg(long)]
    pub tenant: String,

    /// Schema containing the stored flow and scenario catalog.
    #[arg(long, default_value = "wamn_run")]
    pub source_schema: String,

    /// Dedicated, pre-provisioned schema in which scenario runs execute.
    #[arg(long)]
    pub execution_schema: String,

    /// Stable caller-provided execution id used to make case run ids unique.
    #[arg(long)]
    pub execution_id: String,

    /// Stored flow id.
    #[arg(long)]
    pub flow_id: String,

    /// Stored flow version.
    #[arg(long)]
    pub flow_version: i32,

    /// Stored suite id.
    #[arg(long)]
    pub suite_id: String,

    /// Scenario-only credential-vault file.
    #[arg(long, env = "WAMN_SCENARIO_CREDENTIALS_FILE")]
    pub scenario_credentials_file: Option<PathBuf>,

    /// Project whose scenario credentials may be resolved.
    #[arg(long, env = "WAMN_PROJECT", default_value = "default")]
    pub project: String,

    /// Virtual-clock epoch seconds.
    #[arg(long, default_value_t = DEFAULT_EPOCH_SECS)]
    pub epoch_secs: u64,

    /// Deterministic random seed.
    #[arg(long, default_value_t = DEFAULT_RANDOM_SEED)]
    pub random_seed: u64,

    /// Lease TTL for a claimed scenario run, in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    pub lease_ttl_ms: u64,
}

/// Report emitted after a stored suite has executed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ScenarioReport {
    pub execution_id: String,
    pub flow_id: String,
    pub flow_version: i32,
    pub suite_id: String,
    pub cases: Vec<CaseReport>,
}

impl ScenarioReport {
    fn passed(&self) -> bool {
        self.cases.iter().all(|case| case.outcome.passed())
    }
}

/// One stored case's durable run identity and evaluated replay outcome.
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CaseReport {
    pub case_id: String,
    pub run_id: String,
    pub outcome: Outcome,
}

fn database_url(args: &ScenarioWorkerArgs) -> anyhow::Result<String> {
    args.database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")
}

fn is_bare_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

async fn scope_session(
    client: &tokio_postgres::Client,
    tenant: &str,
    schema: &str,
) -> anyhow::Result<()> {
    client
        .query_one(
            "SELECT set_config('app.tenant', $1, false), \
                    set_config('search_path', $2, false)",
            &[&tenant, &schema],
        )
        .await?;
    Ok(())
}

fn expected_authorities(case: &TestCase) -> Vec<String> {
    case.expect
        .iter()
        .filter_map(|assertion| match assertion {
            Assertion::Egress {
                calls: EgressAssertion::ExactlyThese(matchers)
                    | EgressAssertion::Includes(matchers),
                ..
            } => Some(matchers),
            _ => None,
        })
        .flatten()
        .filter_map(|matcher| matcher.authority.clone())
        .collect()
}

async fn capture_db_assertions(
    client: &tokio_postgres::Client,
    case: &TestCase,
) -> anyhow::Result<Vec<DbCapture>> {
    let mut captures = Vec::new();
    for assertion in &case.expect {
        let Assertion::DbState { query, params, .. } = assertion else {
            continue;
        };
        let owned: Vec<String> = params
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string())
            })
            .collect();
        let references: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = owned
            .iter()
            .map(|value| value as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = client
            .query(query, &references)
            .await
            .with_context(|| format!("capture db-state assertion for {}", case.name))?;
        captures.push(DbCapture {
            query: query.clone(),
            params: params.clone(),
            rows: rows
                .iter()
                .map(|row| row.get::<usize, serde_json::Value>(0))
                .collect(),
        });
    }
    Ok(captures)
}

fn parse_fail_kind(value: Option<&str>) -> anyhow::Result<Option<FailKind>> {
    value
        .map(|value| serde_json::from_value(serde_json::Value::String(value.to_string())))
        .transpose()
        .context("unknown persisted fail_kind")
}

fn parse_run_status(value: &str) -> anyhow::Result<RunStatus> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .context("unknown persisted run status")
}

struct QueueScenarioBackend<'a> {
    client: &'a tokio_postgres::Client,
    host: &'a mut ExecutionHost,
}

#[async_trait::async_trait]
impl SchedulerBackend for QueueScenarioBackend<'_> {
    async fn wake_deadlines_nanos(&mut self) -> anyhow::Result<Vec<u64>> {
        let row = self.client.query_one(RUN_QUEUE_NEXT_WAKE_SQL, &[]).await?;
        let seconds: Option<i64> = row.get(0);
        seconds
            .map(|seconds| {
                u64::try_from(seconds)
                    .context("scenario wake deadline precedes unix epoch")
                    .map(|seconds| seconds.saturating_mul(1_000_000_000))
            })
            .into_iter()
            .collect()
    }

    async fn redrive(&mut self) -> anyhow::Result<()> {
        self.client.execute(RUN_QUEUE_DUE_NUDGE_SQL, &[]).await?;
        self.host.drain().await?;
        Ok(())
    }
}

/// Execute one stored suite through the compiled flowrunner and evaluate it.
pub async fn execute(args: &ScenarioWorkerArgs) -> anyhow::Result<ScenarioReport> {
    if !is_bare_identifier(&args.source_schema) || !is_bare_identifier(&args.execution_schema) {
        bail!("source and execution schemas must be bare lowercase SQL identifiers");
    }
    if args.source_schema == args.execution_schema {
        bail!("scenario execution requires a schema distinct from the source catalog");
    }
    if args.execution_id.is_empty() {
        bail!("execution-id must not be empty");
    }

    wash_runtime::init_crypto();
    let database_url = database_url(args)?;
    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read flowrunner component {}", args.flowrunner.display()))?;
    let (client, connection) = tokio_postgres::connect(&database_url, NoTls)
        .await
        .context("connect scenario catalog")?;
    let connection_task = tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(error = %error, "scenario catalog connection failed");
        }
    });

    scope_session(&client, &args.tenant, &args.source_schema).await?;
    let graph_row = client
        .query_opt(
            "SELECT graph_json::text FROM flows \
             WHERE tenant_id = $1 AND flow_id = $2 AND version = $3",
            &[&args.tenant, &args.flow_id, &args.flow_version],
        )
        .await
        .context("read scenario flow graph")?
        .with_context(|| {
            format!(
                "flow {}@{} not found in {}",
                args.flow_id, args.flow_version, args.source_schema
            )
        })?;
    let graph_json: String = graph_row.get(0);
    let case_rows = client
        .query(
            &wamn_scenario_catalog::sql::select_cases_for_suite_sql(),
            &[
                &args.tenant,
                &args.flow_id,
                &args.flow_version,
                &args.suite_id,
            ],
        )
        .await
        .context("read stored scenario cases")?;
    if case_rows.is_empty() {
        bail!(
            "suite {:?} has no cases for {}@{}",
            args.suite_id,
            args.flow_id,
            args.flow_version
        );
    }

    let mut postgres_config = WamnPostgresConfig::from_env();
    postgres_config.database_url = Some(database_url);
    let credentials =
        load_scenario_credentials(args.scenario_credentials_file.as_deref())?.into_plugin();
    let logging = Arc::new(WamnLogging::from_env().context("wamn:logging plugin init")?);
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let mut reports = Vec::with_capacity(case_rows.len());

    for row in case_rows {
        let case_id: String = row.get(0);
        let ordinal: i32 = row.get(1);
        let case_body: String = row.get(2);
        let case: TestCase = serde_json::from_str(&case_body)
            .with_context(|| format!("parse stored case {}/{}", args.suite_id, case_id))?;
        let flow_ref = case
            .flow_ref
            .as_ref()
            .with_context(|| format!("stored case {case_id:?} is not a flow scenario"))?;
        if flow_ref.flow_id != args.flow_id || flow_ref.version as i32 != args.flow_version {
            bail!(
                "stored case {:?} targets {}@{}, expected {}@{}",
                case_id,
                flow_ref.flow_id,
                flow_ref.version,
                args.flow_id,
                args.flow_version
            );
        }

        scope_session(&client, &args.tenant, &args.execution_schema).await?;
        client
            .execute(
                "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
                 VALUES (current_setting('app.tenant', true), $1, $2, false, $3::text::jsonb) \
                 ON CONFLICT (tenant_id, flow_id, version) DO UPDATE \
                 SET graph_json = EXCLUDED.graph_json",
                &[&args.flow_id, &args.flow_version, &graph_json],
            )
            .await
            .context("stage scenario flow graph")?;
        let run_id = format!("scenario-{}-{ordinal}", args.execution_id);
        let input = case.input.to_string();
        let inserted = client
            .execute(
                &write_ahead_triggered_run_sql(),
                &[
                    &run_id,
                    &args.flow_id,
                    &args.flow_version,
                    &"scenario",
                    &input,
                ],
            )
            .await
            .context("persist scenario run")?;
        if inserted != 1 {
            bail!("scenario run {run_id:?} already exists; use a new execution-id");
        }
        client
            .execute(
                &enqueue_sql(),
                &[&run_id, &Option::<&str>::None, &0i32, &0i64],
            )
            .await
            .context("enqueue scenario run")?;

        let recorder = Arc::new(RecordingEgress::spying());
        recorder.expect(&args.flow_id, expected_authorities(&case));
        let (scenario, clock) =
            ScenarioCapabilities::virtualized(args.epoch_secs, args.random_seed, recorder.clone());
        let postgres = case_pool(
            &postgres_config,
            &args.tenant,
            &args.execution_schema,
            &args.flow_id,
        )?;
        let mut host = ExecutionHost::instantiate(
            &engine,
            &guest,
            postgres,
            credentials.clone(),
            logging.clone(),
            ExecutionIdentity {
                owner: &args.flow_id,
                tenant: &args.tenant,
                schema: Some(&args.execution_schema),
                project: &args.project,
            },
            injected_capabilities(scenario.wasi, scenario.egress),
            args.lease_ttl_ms,
        )
        .await
        .context("instantiate scenario flowrunner")?;
        host.drain().await.context("drive stored scenario case")?;
        ScenarioScheduler::new(clock)
            .drive_to_quiescence(&mut QueueScenarioBackend {
                client: &client,
                host: &mut host,
            })
            .await
            .context("resume delayed scenario work")?;
        drop(host);

        scope_session(&client, &args.tenant, &args.execution_schema).await?;
        let result_row = client
            .query_one(
                "SELECT status, fail_kind, fail_node FROM runs \
                 WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1",
                &[&run_id],
            )
            .await
            .context("read durable scenario result")?;
        let status_text: String = result_row.get(0);
        let status = parse_run_status(&status_text)
            .with_context(|| format!("persisted value {status_text:?}"))?;
        let fail_kind_text: Option<String> = result_row.get(1);
        let captured = Captured {
            run: Some(RunFacts {
                status,
                fail_kind: parse_fail_kind(fail_kind_text.as_deref())?,
                fail_node: result_row.get(2),
            }),
            egress: recorder.records(),
            db: capture_db_assertions(&client, &case).await?,
            ..Default::default()
        };
        reports.push(CaseReport {
            case_id,
            run_id,
            outcome: evaluate(&case, &captured),
        });
    }

    ticker.abort();
    connection_task.abort();
    Ok(ScenarioReport {
        execution_id: args.execution_id.clone(),
        flow_id: args.flow_id.clone(),
        flow_version: args.flow_version,
        suite_id: args.suite_id.clone(),
        cases: reports,
    })
}

/// Execute a stored suite, print its report, and fail when an assertion fails.
pub async fn run(args: ScenarioWorkerArgs) -> anyhow::Result<()> {
    let report = execute(&args).await?;
    let passed = report.passed();
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !passed {
        bail!("one or more stored scenario assertions failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_worker_uses_serving_flowrunner_path() {
        assert_eq!(DEFAULT_FLOWRUNNER_PATH, "/components/flowrunner.wasm");
    }

    #[test]
    fn manifest_includes_product_scenario_dependencies_without_service_edge() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("wamn-scenario-runtime"));
        assert!(manifest.contains("wamn-scenario-catalog"));
        assert!(manifest.contains("wamn-scenario-model"));
        assert!(!manifest.contains("../executor"));
    }

    #[test]
    fn identifiers_are_restricted_before_search_path_use() {
        assert!(is_bare_identifier("scenario_exec_1"));
        assert!(!is_bare_identifier("scenario; DROP SCHEMA public"));
        assert!(!is_bare_identifier("Upper"));
    }
}
