//! Product composition for executing stored deterministic flow scenarios.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;
use wash_runtime::host::allowed_hosts::AllowedHost;

use wamn_execution_host::{
    DEFAULT_FLOWRUNNER_PATH, ExecutionHost, ExecutionIdentity, injected_capabilities,
};
use wamn_run_state::queue::{admit_pinned_triggered_run_sql, lock_pinned_trigger_catalog_head_sql};
use wamn_run_state::sql::select_completed_node_runs_sql;
use wamn_run_state::{FailKind as StoredFailKind, RunStatus as StoredRunStatus};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::WamnPostgresConfig;
use wamn_scenario_model::{
    Captured, CaseReport, FailKind, RunFacts, RunStatus, ScenarioRefusal, ScenarioReport, TestCase,
    evaluate,
};
use wamn_scenario_runtime::{
    DatabaseClockBoundary, RUN_QUEUE_DUE_NUDGE_SQL, RUN_QUEUE_NEXT_WAKE_SQL, RecordingEgress,
    ScenarioCapabilities, ScenarioClock, ScenarioScheduler, ScenarioSchemaName, SchedulerBackend,
    capture_db_assertions, case_pool, load_scenario_credentials, validate_queue_due_nudge,
};

const DEFAULT_EPOCH_SECS: u64 = 1_700_000_000;
const DEFAULT_RANDOM_SEED: u64 = 0x7492_5EED_5EED_7492;

const RELEASE_CANDIDATES_SQL: &str = "\
SELECT h.tenant_id, h.catalog_id, h.applied_catalog_version, h.environment, \
       rf.flow_id, rf.flow_version, a.graph_json::text, a.graph_hash, \
       a.artifact_hash, a.interface_bundle_json, a.interface_bundle_hash, \
       a.component_digests::text, a.occurrence_recovery_json, \
       a.occurrence_recovery_hash, \
       (SELECT count(*) FROM jsonb_array_elements(rm.members_json) AS member \
         WHERE member ->> 'flow-id' = rf.flow_id \
           AND (member ->> 'flow-version')::int = rf.flow_version \
           AND member ->> 'artifact-hash' = a.artifact_hash) AS manifest_matches \
  FROM catalog.catalog_heads AS h \
  JOIN catalog.release_flows AS rf \
    ON rf.tenant_id = h.tenant_id AND rf.catalog_id = h.catalog_id \
   AND rf.catalog_version = h.applied_catalog_version \
  JOIN catalog.release_manifests AS rm \
    ON rm.tenant_id = rf.tenant_id AND rm.catalog_id = rf.catalog_id \
   AND rm.catalog_version = rf.catalog_version \
  JOIN catalog.flow_artifacts AS a \
    ON a.tenant_id = rf.tenant_id AND a.flow_id = rf.flow_id \
   AND a.flow_version = rf.flow_version \
 WHERE h.tenant_id = $1 AND rf.flow_id = $2 AND rf.flow_version = $3 \
 ORDER BY h.catalog_id, h.environment, h.applied_catalog_version";

#[derive(Debug, Clone)]
struct ReleaseCandidate {
    tenant_id: String,
    catalog_id: String,
    catalog_version: i32,
    environment: String,
    flow_id: String,
    flow_version: i32,
    graph_json: String,
    graph_hash: String,
    artifact_hash: String,
    interface_bundle_json: String,
    interface_bundle_hash: String,
    component_digests_json: String,
    occurrence_recovery_json: Option<String>,
    occurrence_recovery_hash: Option<String>,
    manifest_matches: i64,
}

#[derive(Debug, Clone)]
struct ReleasePin {
    catalog_id: String,
    catalog_version: i32,
    environment: String,
    artifact_hash: String,
    graph_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScenarioAdmissionResult {
    Admitted,
    MembershipDrift,
    Duplicate,
}

impl ScenarioAdmissionResult {
    fn from_sql(value: &str) -> anyhow::Result<Self> {
        match value {
            "admitted" => Ok(Self::Admitted),
            "membership-drift" => Ok(Self::MembershipDrift),
            "duplicate" => Ok(Self::Duplicate),
            other => bail!("unknown scenario admission result {other:?}"),
        }
    }
}

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

    /// Template for caller-provisioned case schemas; must contain `{ordinal}` once.
    #[arg(long)]
    pub execution_schema_template: String,

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

    /// Trusted scenario outbound HTTP allowlist. Empty denies all egress.
    #[arg(
        long = "allowed-hosts",
        env = "WAMN_SCENARIO_ALLOWED_HOSTS",
        value_delimiter = ','
    )]
    pub allowed_hosts: Vec<String>,

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

fn database_url(args: &ScenarioWorkerArgs) -> anyhow::Result<String> {
    args.database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")
}

fn execution_schema_for_case(template: &str, ordinal: i32) -> anyhow::Result<ScenarioSchemaName> {
    if ordinal < 0 {
        bail!("scenario case ordinal must not be negative: {ordinal}");
    }
    if template.matches("{ordinal}").count() != 1 {
        bail!("execution-schema-template must contain `{{ordinal}}` exactly once");
    }
    let schema = template.replace("{ordinal}", &ordinal.to_string());
    ScenarioSchemaName::new(schema.clone()).with_context(|| {
        format!("execution-schema-template produced invalid scenario schema {schema:?}")
    })
}

fn execution_schemas_for_cases(
    template: &str,
    source_schema: &ScenarioSchemaName,
    ordinals: impl IntoIterator<Item = i32>,
) -> anyhow::Result<Vec<ScenarioSchemaName>> {
    ordinals
        .into_iter()
        .map(|ordinal| {
            let schema = execution_schema_for_case(template, ordinal)?;
            if &schema == source_schema {
                bail!(
                    "scenario case ordinal {ordinal} execution schema must differ from the source catalog"
                );
            }
            Ok(schema)
        })
        .collect()
}

async fn scope_session(
    client: &tokio_postgres::Client,
    tenant: &str,
    schema: &ScenarioSchemaName,
) -> anyhow::Result<()> {
    client
        .query_one(
            "SELECT set_config('app.tenant', $1, false), \
                    set_config('search_path', $2, false)",
            &[&tenant, &schema.as_str()],
        )
        .await?;
    Ok(())
}

fn parse_allowed_hosts(values: &[String]) -> anyhow::Result<Arc<[AllowedHost]>> {
    values
        .iter()
        .map(|value| value.parse::<AllowedHost>())
        .collect::<Result<Vec<_>, _>>()
        .context("parse --allowed-hosts")
        .map(Into::into)
}

async fn release_candidates(
    client: &tokio_postgres::Client,
    tenant: &str,
    flow_id: &str,
    flow_version: i32,
) -> anyhow::Result<Vec<ReleaseCandidate>> {
    client
        .query(RELEASE_CANDIDATES_SQL, &[&tenant, &flow_id, &flow_version])
        .await
        .context("resolve applied immutable scenario release")?
        .into_iter()
        .map(|row| {
            Ok(ReleaseCandidate {
                tenant_id: row.try_get(0)?,
                catalog_id: row.try_get(1)?,
                catalog_version: row.try_get(2)?,
                environment: row.try_get(3)?,
                flow_id: row.try_get(4)?,
                flow_version: row.try_get(5)?,
                graph_json: row.try_get(6)?,
                graph_hash: row.try_get(7)?,
                artifact_hash: row.try_get(8)?,
                interface_bundle_json: row.try_get(9)?,
                interface_bundle_hash: row.try_get(10)?,
                component_digests_json: row.try_get(11)?,
                occurrence_recovery_json: row.try_get(12)?,
                occurrence_recovery_hash: row.try_get(13)?,
                manifest_matches: row.try_get(14)?,
            })
        })
        .collect()
}

fn resolve_release_member(
    tenant: &str,
    flow_id: &str,
    flow_version: i32,
    mut candidates: Vec<ReleaseCandidate>,
) -> anyhow::Result<ReleasePin> {
    if candidates.is_empty() {
        bail!("scenario flow {flow_id}@{flow_version} has no applied immutable release member");
    }
    if candidates.len() != 1 {
        bail!(
            "scenario flow {flow_id}@{flow_version} has ambiguous applied immutable release membership: {} candidates",
            candidates.len()
        );
    }
    let candidate = candidates.pop().expect("one release candidate checked");
    if candidate.tenant_id != tenant
        || candidate.flow_id != flow_id
        || candidate.flow_version != flow_version
        || candidate.catalog_id.is_empty()
        || candidate.catalog_version <= 0
        || candidate.environment.is_empty()
        || candidate.manifest_matches != 1
    {
        bail!("scenario flow {flow_id}@{flow_version} has mismatched release membership");
    }
    if candidate.occurrence_recovery_json.is_none() || candidate.occurrence_recovery_hash.is_none()
    {
        bail!(
            "scenario flow {flow_id}@{flow_version} has an unverifiable immutable release artifact: canonical occurrence recovery is absent"
        );
    }
    let verified_flow_version = u32::try_from(flow_version)
        .context("scenario release flow version is not a positive u32")?;
    let artifact = wamn_catalog::PinnedArtifact::from_storage(
        tenant,
        flow_id,
        verified_flow_version,
        &candidate.graph_json,
        &candidate.graph_hash,
        &candidate.artifact_hash,
        &candidate.interface_bundle_json,
        &candidate.interface_bundle_hash,
        &candidate.component_digests_json,
        candidate.occurrence_recovery_json.as_deref(),
        candidate.occurrence_recovery_hash.as_deref(),
    )
    .with_context(|| {
        format!(
            "scenario flow {flow_id}@{flow_version} has an unverifiable immutable release artifact"
        )
    })?;
    Ok(ReleasePin {
        catalog_id: candidate.catalog_id,
        catalog_version: candidate.catalog_version,
        environment: candidate.environment,
        artifact_hash: candidate.artifact_hash,
        graph_json: artifact.flow().to_json(),
    })
}

async fn resolve_applied_release_member(
    client: &tokio_postgres::Client,
    tenant: &str,
    flow_id: &str,
    flow_version: i32,
) -> anyhow::Result<ReleasePin> {
    let candidates = release_candidates(client, tenant, flow_id, flow_version).await?;
    resolve_release_member(tenant, flow_id, flow_version, candidates)
}

fn verify_locked_catalog_version(expected: i32, locked: Option<i32>) -> anyhow::Result<()> {
    if locked != Some(expected) {
        bail!(
            "scenario release head drifted before admission: expected {expected}, found {locked:?}"
        );
    }
    Ok(())
}

async fn capture_terminal_node(
    client: &tokio_postgres::Client,
    run_id: &str,
) -> anyhow::Result<(Option<serde_json::Value>, Option<String>)> {
    let rows = client
        .query(&select_completed_node_runs_sql(), &[&run_id])
        .await
        .context("read terminal scenario node")?;
    let Some(row) = rows.last() else {
        return Ok((None, None));
    };
    let output_text: Option<String> = row.get(4);
    let output = output_text
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("parse terminal scenario node output")?;
    let port = row
        .get::<usize, Option<String>>(3)
        .or_else(|| output.as_ref().map(|_| "main".to_string()));
    Ok((output, port))
}

fn parse_fail_kind(value: Option<&str>) -> anyhow::Result<Option<FailKind>> {
    value
        .map(|value| {
            StoredFailKind::from_sql(value)
                .map(wamn_scenario_catalog::compat::fail_kind_from_store)
                .with_context(|| format!("unknown persisted fail_kind {value:?}"))
        })
        .transpose()
}

fn parse_run_status(value: &str) -> anyhow::Result<RunStatus> {
    StoredRunStatus::from_sql(value)
        .map(wamn_scenario_catalog::compat::run_status_from_store)
        .with_context(|| format!("unknown persisted run status {value:?}"))
}

fn unix_nanos(instant: SystemTime) -> anyhow::Result<u64> {
    let nanos = instant
        .duration_since(UNIX_EPOCH)
        .context("database clock instant precedes unix epoch")?
        .as_nanos();
    u64::try_from(nanos).context("database clock instant exceeds u64 nanos")
}

fn logical_schedule_deadline(clock: &ScenarioClock, state_json: &str) -> anyhow::Result<u64> {
    let state: serde_json::Value =
        serde_json::from_str(state_json).context("parse scenario scheduling state")?;
    let wake = state
        .get("wake")
        .and_then(serde_json::Value::as_object)
        .and_then(|wake| wake.values().filter_map(serde_json::Value::as_u64).min())
        .map(|seconds| {
            seconds
                .checked_mul(1_000_000_000)
                .context("scenario wake deadline exceeds u64 nanos")
        })
        .transpose()?;
    let retry = state.get("retry").and_then(serde_json::Value::as_object);
    let retry_deadline = retry
        .and_then(|retry| retry.get("delay-ms"))
        .and_then(serde_json::Value::as_u64)
        .map(|delay_ms| {
            let delay_nanos = delay_ms
                .checked_mul(1_000_000)
                .context("scenario retry delay exceeds u64 nanos")?;
            clock
                .now_nanos()
                .checked_add(delay_nanos)
                .context("scenario retry deadline exceeds u64 nanos")
        })
        .transpose()?;

    match (wake, retry_deadline) {
        (Some(wake), Some(retry)) => Ok(wake.min(retry)),
        (Some(wake), None) => Ok(wake),
        (None, Some(retry)) => Ok(retry),
        (None, None) if retry.is_some() => {
            bail!("legacy retry schedule has no deterministic delay-ms")
        }
        (None, None) => bail!("parked scenario run has no virtual wake or retry schedule"),
    }
}

struct QueueScenarioBackend<'a> {
    client: &'a tokio_postgres::Client,
    host: &'a mut ExecutionHost,
    run_id: &'a str,
    clock: ScenarioClock,
    clock_boundary: DatabaseClockBoundary,
    selected_at: Option<SystemTime>,
    selected_deadline_nanos: Option<u64>,
}

impl<'a> QueueScenarioBackend<'a> {
    fn new(
        client: &'a tokio_postgres::Client,
        host: &'a mut ExecutionHost,
        run_id: &'a str,
        clock: ScenarioClock,
        clock_boundary: DatabaseClockBoundary,
    ) -> Self {
        Self {
            client,
            host,
            run_id,
            clock,
            clock_boundary,
            selected_at: None,
            selected_deadline_nanos: None,
        }
    }
}

#[async_trait::async_trait]
impl SchedulerBackend for QueueScenarioBackend<'_> {
    async fn wake_deadlines_nanos(&mut self) -> anyhow::Result<Vec<u64>> {
        let rows = self
            .client
            .query(RUN_QUEUE_NEXT_WAKE_SQL, &[&self.run_id])
            .await?;
        if rows.len() > 1 {
            return Err(wamn_scenario_runtime::QueueScheduleShiftError::Ambiguous {
                run_id: self.run_id.to_string(),
                matched: u64::try_from(rows.len()).context("queue row count exceeds u64")?,
            }
            .into());
        }
        let Some(row) = rows.first() else {
            self.selected_at = None;
            self.selected_deadline_nanos = None;
            return Ok(Vec::new());
        };
        let selected_at: SystemTime = row.get(0);
        let state_json: String = row.get(1);
        let nanos = logical_schedule_deadline(&self.clock, &state_json)?;
        self.selected_at = Some(selected_at);
        self.selected_deadline_nanos = Some(nanos);
        Ok(vec![nanos])
    }

    async fn redrive(&mut self) -> anyhow::Result<()> {
        let selected_at = self.selected_at.take().ok_or_else(|| {
            wamn_scenario_runtime::QueueScheduleShiftError::Stale {
                run_id: self.run_id.to_string(),
            }
        })?;
        let selected_deadline_nanos = self.selected_deadline_nanos.take().ok_or_else(|| {
            wamn_scenario_runtime::QueueScheduleShiftError::Stale {
                run_id: self.run_id.to_string(),
            }
        })?;
        let release_nanos = self
            .clock_boundary
            .release_nanos(selected_deadline_nanos)
            .context("scenario scheduler attempted to release work before its logical deadline")?;
        let release_at = UNIX_EPOCH
            .checked_add(Duration::from_nanos(release_nanos))
            .context("database release instant exceeds SystemTime")?;
        let row = self
            .client
            .query_one(
                RUN_QUEUE_DUE_NUDGE_SQL,
                &[&self.run_id, &selected_at, &release_at],
            )
            .await?;
        let matched =
            u64::try_from(row.get::<_, i64>(0)).context("negative queue candidate count")?;
        let shifted = u64::try_from(row.get::<_, i64>(1)).context("negative queue shift count")?;
        validate_queue_due_nudge(self.run_id, matched, shifted)?;
        self.host.drain().await?;
        Ok(())
    }
}

/// Execute one stored suite through the compiled flowrunner and evaluate it.
pub async fn execute(args: &ScenarioWorkerArgs) -> anyhow::Result<ScenarioReport> {
    let source_schema = ScenarioSchemaName::new(args.source_schema.clone())
        .context("source schema is not a valid scenario schema name")?;
    execution_schema_for_case(&args.execution_schema_template, 0)?;
    if args.execution_id.is_empty() {
        bail!("execution-id must not be empty");
    }
    let allowed_hosts = parse_allowed_hosts(&args.allowed_hosts)?;

    wash_runtime::init_crypto();
    let database_url = database_url(args)?;
    let (mut client, connection) = tokio_postgres::connect(&database_url, NoTls)
        .await
        .context("connect scenario catalog")?;
    let connection_task = tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(error = %error, "scenario catalog connection failed");
        }
    });

    scope_session(&client, &args.tenant, &source_schema).await?;
    let release =
        resolve_applied_release_member(&client, &args.tenant, &args.flow_id, args.flow_version)
            .await?;
    let graph_json = &release.graph_json;
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

    let mut seen_ordinals = BTreeSet::new();
    let mut stored_cases = Vec::with_capacity(case_rows.len());
    for row in case_rows {
        let case_id: String = row.get(0);
        let ordinal: i32 = row.get(1);
        let case_body: String = row.get(2);
        if !seen_ordinals.insert(ordinal) {
            bail!(
                "suite {:?} has duplicate case ordinal {ordinal}",
                args.suite_id
            );
        }
        stored_cases.push((case_id, ordinal, case_body));
    }
    // Validate every generated name as one batch before the first staging write.
    // A later ordinal can add enough digits to cross PostgreSQL's byte limit.
    let execution_schemas = execution_schemas_for_cases(
        &args.execution_schema_template,
        &source_schema,
        stored_cases.iter().map(|(_, ordinal, _)| *ordinal),
    )?;
    let stored_cases = stored_cases
        .into_iter()
        .zip(execution_schemas)
        .map(|((case_id, ordinal, case_body), execution_schema)| {
            let case: TestCase = serde_json::from_str(&case_body)
                .with_context(|| format!("parse stored case {}/{}", args.suite_id, case_id))?;
            case.validate()
                .with_context(|| format!("validate stored case {}/{}", args.suite_id, case_id))?;
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
            Ok((case_id, ordinal, case, execution_schema))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read flowrunner component {}", args.flowrunner.display()))?;
    let mut postgres_config = WamnPostgresConfig::from_env();
    postgres_config.database_url = Some(database_url);
    let credentials =
        load_scenario_credentials(args.scenario_credentials_file.as_deref())?.into_plugin();
    let logging = Arc::new(WamnLogging::from_env().context("wamn:logging plugin init")?);
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let mut reports = Vec::with_capacity(stored_cases.len());
    let mut checked_flow = false;

    for (case_id, ordinal, case, execution_schema) in stored_cases {
        let egress_policy = Arc::new(RunnerEgressPolicy::default());
        let recorder = Arc::new(RecordingEgress::spying(egress_policy.clone()));
        let (scenario, clock) =
            ScenarioCapabilities::virtualized(args.epoch_secs, args.random_seed, recorder.clone());
        let postgres = case_pool(
            &postgres_config,
            &args.tenant,
            &execution_schema,
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
                schema: Some(execution_schema.as_str()),
                project: &args.project,
            },
            injected_capabilities(
                scenario.wasi,
                scenario.egress,
                allowed_hosts.clone(),
                egress_policy,
            ),
            args.lease_ttl_ms,
        )
        .await
        .context("instantiate scenario flowrunner")?;
        if !checked_flow {
            let node_types = host
                .check_flow(graph_json)
                .await
                .context("preflight stored scenario flow")?;
            if !node_types.is_empty() {
                drop(host);
                ticker.abort();
                connection_task.abort();
                return Ok(ScenarioReport {
                    execution_id: args.execution_id.clone(),
                    scenario_epoch_secs: Some(args.epoch_secs),
                    flow_id: args.flow_id.clone(),
                    flow_version: args.flow_version,
                    suite_id: args.suite_id.clone(),
                    refusal: Some(ScenarioRefusal::UndrivableNodes { node_types }),
                    cases: reports,
                });
            }
            checked_flow = true;
        }

        scope_session(&client, &args.tenant, &execution_schema).await?;
        let run_id = format!("scenario-{}-{ordinal}", args.execution_id);
        let input = case.input.to_string();
        let database_origin: SystemTime = client
            .query_one("SELECT now()", &[])
            .await
            .context("capture scenario database clock boundary")?
            .get(0);
        let clock_boundary = DatabaseClockBoundary::capture(&clock, unix_nanos(database_origin)?);
        let transaction = client
            .transaction()
            .await
            .context("begin pinned scenario admission")?;
        let locked_catalog_version: Option<i32> = transaction
            .query_one(
                &lock_pinned_trigger_catalog_head_sql(),
                &[&release.catalog_id, &release.environment],
            )
            .await
            .context("lock scenario catalog head")?
            .get(0);
        verify_locked_catalog_version(release.catalog_version, locked_catalog_version)?;
        let admission_row = transaction
            .query_one(
                &admit_pinned_triggered_run_sql(),
                &[
                    &run_id,
                    &args.flow_id,
                    &args.flow_version,
                    &release.catalog_id,
                    &release.catalog_version,
                    &release.environment,
                    &input,
                    &args.suite_id,
                    &case_id,
                    &release.artifact_hash,
                    &env!("CARGO_PKG_VERSION"),
                ],
            )
            .await
            .context("atomically admit and enqueue pinned scenario run")?;
        match ScenarioAdmissionResult::from_sql(admission_row.get(0))? {
            ScenarioAdmissionResult::Admitted => transaction
                .commit()
                .await
                .context("commit pinned scenario admission")?,
            ScenarioAdmissionResult::MembershipDrift => {
                bail!("scenario immutable release membership drifted before admission")
            }
            ScenarioAdmissionResult::Duplicate => {
                bail!("scenario run {run_id:?} already exists; use a new execution-id")
            }
        }

        host.drain().await.context("drive stored scenario case")?;
        ScenarioScheduler::new(clock.clone())
            .drive_to_quiescence(&mut QueueScenarioBackend::new(
                &client,
                &mut host,
                &run_id,
                clock.clone(),
                clock_boundary,
            ))
            .await
            .context("resume delayed scenario work")?;
        drop(host);

        scope_session(&client, &args.tenant, &execution_schema).await?;
        let result_row = client
            .query_one(
                "SELECT status, fail_kind, fail_node FROM runs \
                 WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1",
                &[&run_id],
            )
            .await
            .context("read durable scenario result")?;
        let status_text: String = result_row.get(0);
        let status = parse_run_status(&status_text)?;
        let fail_kind_text: Option<String> = result_row.get(1);
        let (node_output, node_port) = capture_terminal_node(&client, &run_id).await?;
        let captured = Captured {
            node_output,
            node_port,
            run: Some(RunFacts {
                status,
                fail_kind: parse_fail_kind(fail_kind_text.as_deref())?,
                fail_node: result_row.get(2),
            }),
            egress: recorder.records(),
            db: capture_db_assertions(&mut client, &case).await?,
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
        scenario_epoch_secs: Some(args.epoch_secs),
        flow_id: args.flow_id.clone(),
        flow_version: args.flow_version,
        suite_id: args.suite_id.clone(),
        refusal: None,
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

    fn release_candidate() -> ReleaseCandidate {
        let graph_json = r#"{
          "schema-version":"0.1","flow-id":"scenario-flow","version":1,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":true}},
            {"id":"write","type":"postgres","config":{"entity":"sink","op":"create"}},
            {"id":"respond","type":"respond","config":{"status":200}}
          ],
          "edges":[
            {"from":"request","to":"write"},
            {"from":"write","to":"respond"}
          ]
        }"#;
        let flow = wamn_flow::Flow::from_json(graph_json).unwrap();
        let mut implementations = ["request", "postgres", "respond"]
            .into_iter()
            .map(|node_type| {
                let descriptor = wamn_standard_nodes::describe(node_type).unwrap();
                let contract = wamn_standard_nodes::resolve_descriptor(descriptor).unwrap();
                wamn_catalog::NodeImplementation::from_resolved_platform_contract(contract).unwrap()
            })
            .collect::<Vec<_>>();
        implementations
            .sort_by(|left, right| left.interface().node_type.cmp(&right.interface().node_type));
        let artifact = wamn_catalog::Artifact::new("tenant-a", &flow, implementations).unwrap();

        ReleaseCandidate {
            tenant_id: "tenant-a".into(),
            catalog_id: "scenario-catalog".into(),
            catalog_version: 1,
            environment: "dev".into(),
            flow_id: "scenario-flow".into(),
            flow_version: 1,
            graph_json: graph_json.into(),
            graph_hash: artifact.graph_hash().into(),
            artifact_hash: artifact.identity().artifact_hash().as_str().into(),
            interface_bundle_json: String::from_utf8(
                artifact.interface_bundle().canonical_bytes().to_vec(),
            )
            .unwrap(),
            interface_bundle_hash: artifact.interface_bundle().hash().into(),
            component_digests_json: serde_json::to_string(artifact.supplied_components()).unwrap(),
            occurrence_recovery_json: Some(
                String::from_utf8(artifact.occurrence_recovery_bytes().to_vec()).unwrap(),
            ),
            occurrence_recovery_hash: Some(artifact.occurrence_recovery_hash().into()),
            manifest_matches: 1,
        }
    }

    #[test]
    fn exact_verified_release_member_supplies_the_only_execution_graph() {
        let candidate = release_candidate();
        let expected_hash = candidate.artifact_hash.clone();
        let release =
            resolve_release_member("tenant-a", "scenario-flow", 1, vec![candidate]).unwrap();

        assert_eq!(release.catalog_id, "scenario-catalog");
        assert_eq!(release.catalog_version, 1);
        assert_eq!(release.environment, "dev");
        assert_eq!(release.artifact_hash, expected_hash);
        assert_eq!(
            wamn_flow::Flow::from_json(&release.graph_json)
                .unwrap()
                .flow_id,
            "scenario-flow"
        );
        assert!(!RELEASE_CANDIDATES_SQL.contains("FROM flows"));
        assert!(!RELEASE_CANDIDATES_SQL.contains("LEFT JOIN"));
    }

    #[test]
    fn absent_or_ambiguous_release_membership_refuses_before_execution() {
        let missing = resolve_release_member("tenant-a", "scenario-flow", 1, vec![])
            .unwrap_err()
            .to_string();
        assert!(missing.contains("no applied immutable release member"));

        let candidate = release_candidate();
        let mut other = candidate.clone();
        other.catalog_id = "other-catalog".into();
        let ambiguous =
            resolve_release_member("tenant-a", "scenario-flow", 1, vec![candidate, other])
                .unwrap_err()
                .to_string();
        assert!(ambiguous.contains("ambiguous applied immutable release membership"));
    }

    #[test]
    fn mismatched_or_unverifiable_release_membership_refuses_before_execution() {
        let mut mismatched = release_candidate();
        mismatched.manifest_matches = 0;
        let mismatch = resolve_release_member("tenant-a", "scenario-flow", 1, vec![mismatched])
            .unwrap_err()
            .to_string();
        assert!(mismatch.contains("mismatched release membership"));

        let mut tampered = release_candidate();
        tampered.graph_json = tampered
            .graph_json
            .replace("scenario-flow", "tampered-flow");
        let unverifiable =
            resolve_release_member("tenant-a", "scenario-flow", 1, vec![tampered]).unwrap_err();
        assert!(format!("{unverifiable:#}").contains("unverifiable immutable release artifact"));
    }

    #[test]
    fn absent_occurrence_recovery_refuses_before_any_admission() {
        let mut missing_json = release_candidate();
        missing_json.occurrence_recovery_json = None;
        let error = resolve_release_member("tenant-a", "scenario-flow", 1, vec![missing_json])
            .unwrap_err()
            .to_string();
        assert!(error.contains("canonical occurrence recovery is absent"));

        let mut missing_hash = release_candidate();
        missing_hash.occurrence_recovery_hash = None;
        let error = resolve_release_member("tenant-a", "scenario-flow", 1, vec![missing_hash])
            .unwrap_err()
            .to_string();
        assert!(error.contains("canonical occurrence recovery is absent"));
    }

    #[test]
    fn head_drift_between_preflight_and_admission_refuses_before_write() {
        let preflight =
            resolve_release_member("tenant-a", "scenario-flow", 1, vec![release_candidate()])
                .unwrap();
        let mut admission_writes = 0;

        let result = verify_locked_catalog_version(preflight.catalog_version, Some(2));
        if result.is_ok() {
            admission_writes += 1;
        }

        assert!(result.unwrap_err().to_string().contains("head drifted"));
        assert_eq!(admission_writes, 0);
        assert_eq!(
            ScenarioAdmissionResult::from_sql("membership-drift").unwrap(),
            ScenarioAdmissionResult::MembershipDrift
        );
        assert_eq!(
            ScenarioAdmissionResult::from_sql("duplicate").unwrap(),
            ScenarioAdmissionResult::Duplicate
        );
    }

    #[test]
    fn worker_source_contains_no_mutable_flow_projection_fallback() {
        let source = include_str!("lib.rs");
        let mutable_read = ["SELECT graph_json::text ", "FROM flows"].concat();
        let mutable_stage = ["stage scenario ", "flow graph"].concat();
        assert!(!source.contains(&mutable_read));
        assert!(!source.contains(&mutable_stage));
    }

    #[test]
    fn final_admission_locks_rechecks_writes_and_commits_in_one_transaction() {
        let source = include_str!("lib.rs");
        let execute = source
            .split_once("pub async fn execute")
            .expect("execute function")
            .1;
        let transaction = execute.find("let transaction = client").unwrap();
        let lock = execute
            .find("lock_pinned_trigger_catalog_head_sql")
            .unwrap();
        let recheck = execute.find("verify_locked_catalog_version").unwrap();
        let admission = execute.find("admit_pinned_triggered_run_sql").unwrap();
        let commit = execute.find(".commit()").unwrap();

        assert!(transaction < lock);
        assert!(lock < recheck);
        assert!(recheck < admission);
        assert!(admission < commit);
    }

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
    fn trusted_scenario_allowlist_is_separate_and_empty_by_default() {
        assert!(parse_allowed_hosts(&[]).unwrap().is_empty());
        assert_eq!(
            parse_allowed_hosts(&["echo.local:8080".into()])
                .unwrap()
                .len(),
            1
        );
        assert!(parse_allowed_hosts(&["*bad-wildcard".into()]).is_err());
    }

    #[test]
    fn execution_schema_template_is_explicit_and_case_isolated() {
        assert_eq!(
            execution_schema_for_case("scenario_exec_{ordinal}", 7)
                .unwrap()
                .as_str(),
            "scenario_exec_7"
        );
        assert!(execution_schema_for_case("scenario_exec", 0).is_err());
        assert!(execution_schema_for_case("{ordinal}_{ordinal}", 0).is_err());
        assert!(execution_schema_for_case("scenario-exec-{ordinal}", 0).is_err());
        assert!(execution_schema_for_case("scenario_exec_{ordinal}", -1).is_err());
    }

    #[test]
    fn all_case_schema_names_validate_before_the_staging_loop() {
        let source = ScenarioSchemaName::new("wamn_run").unwrap();
        let template = format!("{}{{ordinal}}", "s".repeat(59));
        let mut staging_writes = 0;

        let schemas = execution_schemas_for_cases(&template, &source, [0, 10_000]);
        assert!(schemas.is_err());
        if let Ok(schemas) = schemas {
            for _ in schemas {
                staging_writes += 1;
            }
        }

        assert_eq!(staging_writes, 0);
    }

    #[test]
    fn virtual_wake_and_retry_schedules_ignore_database_calendar_time() {
        let clock = ScenarioClock::at_secs(1_700_000_000);
        let now = clock.now_nanos();
        let wake = logical_schedule_deadline(&clock, r#"{"wake":{"delay":1700000001}}"#).unwrap();
        let retry = logical_schedule_deadline(&clock, r#"{"retry":{"delay-ms":750}}"#).unwrap();

        assert_eq!(wake, now + 1_000_000_000);
        assert_eq!(retry, now + 750_000_000);
        assert!(!clock.is_due(wake));
        assert!(!clock.is_due(retry));
        clock.advance_to_nanos(retry);
        assert!(clock.is_due(retry), "retry is due at equality");
        assert!(!clock.is_due(wake), "later wake remains parked");
        clock.advance_to_nanos(wake);
        assert!(clock.is_due(wake), "wake is due at equality");
    }

    #[test]
    fn legacy_retry_without_a_logical_delay_fails_closed() {
        let clock = ScenarioClock::at_secs(1_700_000_000);
        let error = logical_schedule_deadline(&clock, r#"{"retry":{"node":"call","attempt":1}}"#)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("legacy retry schedule has no deterministic delay-ms")
        );
    }

    #[test]
    fn logical_schedule_arithmetic_is_checked() {
        let clock = ScenarioClock::at_secs(u64::MAX / 1_000_000_000);
        assert!(logical_schedule_deadline(&clock, r#"{"retry":{"delay-ms":1000}}"#).is_err());
        assert!(logical_schedule_deadline(&clock, r#"{"wake":{"delay":18446744074}}"#).is_err());
    }

    #[test]
    fn queue_timestamp_is_only_an_opaque_stale_token() {
        assert!(RUN_QUEUE_NEXT_WAKE_SQL.contains("q.available_at"));
        assert!(RUN_QUEUE_NEXT_WAKE_SQL.contains("r.state_json::text"));
        assert!(!RUN_QUEUE_NEXT_WAKE_SQL.contains("extract(epoch"));
        assert!(!RUN_QUEUE_NEXT_WAKE_SQL.contains("available_at >"));
    }
}
