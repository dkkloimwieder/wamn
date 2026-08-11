//! Repository-only fixture and process adapter for the stored-scenario worker gate.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, bail};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::process::Command;
use tokio_postgres::{Client, NoTls};

use crate::ctl_process;
use wamn_gate_harness::{seed_flow_version, seed_test_case, seed_test_suite};
use wamn_scenario_model::{ScenarioRefusal, ScenarioReport};
use wamn_scenario_runtime::ScenarioSchemaName;

const DEMO_FLOW_PREFIX: &str = "tk-demo-flow";
const UNDRIVABLE_FLOW_PREFIX: &str = "tk-undrivable-flow";
const UNDRIVABLE_NODE_TYPE: &str = "disposition-recommendation";
const RELEASE_ENVIRONMENT: &str = "dev";
const RELEASE_VERSION: i32 = 1;
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Stored-suite compatibility inputs forwarded to the product worker process.
#[derive(Debug)]
pub struct StoredSuiteGateArgs {
    pub worker: PathBuf,
    pub flowrunner: PathBuf,
    pub node: PathBuf,
    pub database_url: Option<String>,
    pub admin_database_url: String,
    pub suites: Vec<String>,
    pub tenant: Option<String>,
    pub impact_report: Option<PathBuf>,
    pub source_schema: String,
    pub execution_schema_base: String,
    pub seed_demo: bool,
    pub keep: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteSelector {
    tenant: String,
    flow_id: String,
    flow_version: i32,
    suite_id: String,
}

#[derive(Debug)]
struct SelectedSuite {
    selector: SuiteSelector,
    expected: ExpectedExit,
}

#[derive(Debug, Clone, Copy)]
enum ExpectedExit {
    Pass,
    Success,
    Refusal(&'static str),
    Failure(&'static str),
}

#[derive(Debug)]
struct FixtureFiles {
    root: PathBuf,
}

impl FixtureFiles {
    fn create() -> anyhow::Result<Self> {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "wamn-suiteexec-publish-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root)
            .with_context(|| format!("create scenario publication inputs {}", root.display()))?;
        Ok(Self { root })
    }

    fn write(&self, name: &str, contents: impl AsRef<[u8]>) -> anyhow::Result<PathBuf> {
        let path = self.root.join(name);
        std::fs::write(&path, contents)
            .with_context(|| format!("write scenario publication input {}", path.display()))?;
        Ok(path)
    }
}

impl Drop for FixtureFiles {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            tracing::warn!(path = %self.root.display(), %error, "remove scenario publication inputs");
        }
    }
}

#[derive(Debug)]
struct ScenarioPublication {
    _files: FixtureFiles,
    catalog_id: String,
    demo_flow_id: String,
    undrivable_flow_id: String,
    catalog: PathBuf,
    demo_flow: PathBuf,
    undrivable_flow: PathBuf,
    custom_node: PathBuf,
}

impl ScenarioPublication {
    fn create(node: &Path) -> anyhow::Result<Self> {
        let component_bytes = std::fs::read(node)
            .with_context(|| format!("read undrivable component fixture {}", node.display()))?;
        let component_digest = component_digest(&component_bytes);
        let suffix = component_digest
            .strip_prefix("sha256:")
            .expect("component digest has the canonical prefix")
            .get(..12)
            .expect("sha256 digest has at least twelve hex digits");
        let catalog_id = format!("tk-scenario-{suffix}");
        let demo_flow_id = format!("{DEMO_FLOW_PREFIX}-{suffix}");
        let undrivable_flow_id = format!("{UNDRIVABLE_FLOW_PREFIX}-{suffix}");
        let files = FixtureFiles::create()?;
        let component = files.write("disposition-node.wasm", component_bytes)?;
        let manifest = files.write(
            "disposition-node.manifest.json",
            serde_json::json!({
                "schema-version": "0.1",
                "node-type": UNDRIVABLE_NODE_TYPE,
                "name": "Disposition Recommendation",
                "description": "POC-F2 zero-import pure custom node: compares a deterministic quality-hold recommendation with the recorded decision and emits string confidence.",
                "version": "0.1.0",
                "contract": "0.1.0",
                "ordering": ["unordered"],
                "purity": "pure"
            })
            .to_string(),
        )?;
        let custom_node = files.write(
            "disposition-node.component.json",
            serde_json::json!({
                "node-type": UNDRIVABLE_NODE_TYPE,
                "component": component,
                "manifest": manifest,
                "component-digest": component_digest,
            })
            .to_string(),
        )?;
        let catalog = files.write("catalog.json", demo_catalog(&catalog_id))?;
        let demo_flow = files.write("demo.flow.json", demo_graph(&demo_flow_id))?;
        let undrivable_flow = files.write(
            "undrivable.flow.json",
            undrivable_graph(&undrivable_flow_id),
        )?;
        Ok(Self {
            _files: files,
            catalog_id,
            demo_flow_id,
            undrivable_flow_id,
            catalog,
            demo_flow,
            undrivable_flow,
            custom_node,
        })
    }

    fn publish_command(&self, admin_url: &str, tenant: &str, schema: &str) -> Vec<OsString> {
        vec![
            "publish-catalog".into(),
            "--catalog".into(),
            self.catalog.as_os_str().to_owned(),
            "--admin-database-url".into(),
            admin_url.into(),
            "--tenant".into(),
            tenant.into(),
            "--schema".into(),
            schema.into(),
            "--provision".into(),
            "--runstate".into(),
            "--flow".into(),
            self.demo_flow.as_os_str().to_owned(),
            "--flow".into(),
            self.undrivable_flow.as_os_str().to_owned(),
            "--custom-node".into(),
            self.custom_node.as_os_str().to_owned(),
        ]
    }

    async fn publish(&self, admin_url: &str, tenant: &str, schema: &str) -> anyhow::Result<()> {
        ctl_process::run_checked(self.publish_command(admin_url, tenant, schema))
            .await
            .with_context(|| format!("publish immutable scenario release into {schema}"))?;
        Ok(())
    }
}

fn component_digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let hex = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

/// Provision repository fixtures, invoke the product binary, and clean every sandbox.
pub async fn run(args: StoredSuiteGateArgs) -> anyhow::Result<()> {
    ScenarioSchemaName::new(args.source_schema.clone())
        .context("source schema is not a valid scenario schema name")?;
    let database_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("stored-suite gate needs --database-url or WAMN_PG_URL / DATABASE_URL")?;
    let (admin, connection) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("connect stored-suite gate admin")?;
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });

    let result = async {
        let publication = if args.seed_demo {
            Some(
                seed_demo(
                    &admin,
                    &args.admin_database_url,
                    &args.source_schema,
                    args.tenant.as_deref(),
                    &args.node,
                )
                .await?,
            )
        } else {
            None
        };
        let suites = select_suites(&admin, &args, publication.as_ref()).await?;
        if suites.is_empty() {
            bail!("stored-suite selection resolved to no suites");
        }
        for (index, selected) in suites.iter().enumerate() {
            run_selected_suite(
                &admin,
                &database_url,
                &args,
                selected,
                index,
                publication.as_ref(),
            )
            .await?;
        }
        Ok(())
    }
    .await;

    let source_cleanup = if args.seed_demo && !args.keep {
        drop_schema(&admin, &args.source_schema).await
    } else {
        Ok(())
    };
    drop(admin);
    connection_task.abort();
    combine(result, source_cleanup, "drop seeded source schema")
}

async fn select_suites(
    client: &Client,
    args: &StoredSuiteGateArgs,
    publication: Option<&ScenarioPublication>,
) -> anyhow::Result<Vec<SelectedSuite>> {
    let source_count = usize::from(args.seed_demo)
        + usize::from(!args.suites.is_empty())
        + usize::from(args.impact_report.is_some());
    if source_count != 1 {
        bail!("exactly one stored-suite selection source is required");
    }
    if args.seed_demo {
        let tenant = args.tenant.clone().context("--seed-demo needs --tenant")?;
        let publication = publication.context("--seed-demo publication was not prepared")?;
        return Ok(vec![
            selected(
                &tenant,
                &publication.demo_flow_id,
                "success",
                ExpectedExit::Pass,
            ),
            selected(
                &tenant,
                &publication.demo_flow_id,
                "malformed",
                ExpectedExit::Failure("parse stored case"),
            ),
            selected(
                &tenant,
                &publication.undrivable_flow_id,
                "undrivable",
                ExpectedExit::Refusal(UNDRIVABLE_NODE_TYPE),
            ),
            selected(
                &tenant,
                &publication.demo_flow_id,
                "assertion-failure",
                ExpectedExit::Failure("stored scenario assertions failed"),
            ),
        ]);
    }
    if !args.suites.is_empty() {
        let tenant = args.tenant.clone().context("--suite needs --tenant")?;
        let mut selected = Vec::new();
        for value in &args.suites {
            let (flow_id, flow_version) = parse_flow_at_version(value)?;
            scope_session(client, &tenant, &args.source_schema).await?;
            let rows = client
                .query(
                    &wamn_scenario_catalog::sql::select_suites_for_flow_sql(),
                    &[&tenant, &flow_id, &flow_version],
                )
                .await
                .context("enumerate suites for flow")?;
            for row in rows {
                selected.push(SelectedSuite {
                    selector: SuiteSelector {
                        tenant: tenant.clone(),
                        flow_id: flow_id.clone(),
                        flow_version,
                        suite_id: row.get(0),
                    },
                    expected: ExpectedExit::Success,
                });
            }
        }
        return Ok(selected);
    }
    let path = args
        .impact_report
        .as_ref()
        .expect("selection count checked");
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read impact report {}", path.display()))?;
    let selectors: Vec<SuiteSelector> = serde_json::from_str(&raw)
        .with_context(|| format!("parse impact report {}", path.display()))?;
    selectors
        .into_iter()
        .map(|selector| {
            if selector.flow_version < 0 {
                bail!("flow_version must be non-negative");
            }
            Ok(SelectedSuite {
                selector,
                expected: ExpectedExit::Success,
            })
        })
        .collect()
}

fn selected(tenant: &str, flow_id: &str, suite_id: &str, expected: ExpectedExit) -> SelectedSuite {
    SelectedSuite {
        selector: SuiteSelector {
            tenant: tenant.to_owned(),
            flow_id: flow_id.to_owned(),
            flow_version: 1,
            suite_id: suite_id.to_owned(),
        },
        expected,
    }
}

fn execution_schema_template(base: &str, index: usize) -> anyhow::Result<String> {
    let template = format!("{base}_{index}_{{ordinal}}");
    ScenarioSchemaName::new(template.replace("{ordinal}", "0"))
        .context("execution schema template produces an invalid scenario schema")?;
    Ok(template)
}

fn parse_flow_at_version(value: &str) -> anyhow::Result<(String, i32)> {
    let (flow_id, version) = value
        .rsplit_once('@')
        .with_context(|| format!("--suite must be <flow_id>@<version>: {value:?}"))?;
    if flow_id.is_empty() {
        bail!("--suite flow_id is empty");
    }
    let version: i32 = version
        .parse()
        .context("--suite version must be an integer")?;
    if version < 0 {
        bail!("--suite version must be non-negative");
    }
    Ok((flow_id.to_owned(), version))
}

fn accepted_success_label(
    report: &ScenarioReport,
    expected: ExpectedExit,
) -> anyhow::Result<&'static str> {
    match expected {
        ExpectedExit::Pass if report.refusal.is_none() => Ok("PASS"),
        ExpectedExit::Pass => bail!("scenario-worker unexpectedly refused a drivable suite"),
        ExpectedExit::Success => Ok(if report.refusal.is_some() {
            "REFUSED"
        } else {
            "PASS"
        }),
        ExpectedExit::Refusal(expected_node_type) => match &report.refusal {
            Some(ScenarioRefusal::UndrivableNodes { node_types })
                if node_types
                    .iter()
                    .any(|node_type| node_type == expected_node_type) =>
            {
                Ok("expected refusal PASS")
            }
            Some(ScenarioRefusal::UndrivableNodes { node_types }) => bail!(
                "scenario-worker refusal did not name expected node type {expected_node_type:?}: {node_types:?}"
            ),
            Some(other) => bail!(
                "scenario-worker refused with {other:?} instead of an undrivable-node refusal naming {expected_node_type:?}"
            ),
            None => bail!("scenario-worker unexpectedly executed an undrivable suite"),
        },
        ExpectedExit::Failure(_) => bail!("expected a scenario-worker process failure"),
    }
}

async fn run_selected_suite(
    admin: &Client,
    database_url: &str,
    args: &StoredSuiteGateArgs,
    selected: &SelectedSuite,
    index: usize,
    publication: Option<&ScenarioPublication>,
) -> anyhow::Result<()> {
    let selector = &selected.selector;
    println!(
        "# scenario-worker black-box: {} ({})",
        selector.suite_id, selector.flow_id
    );
    scope_session(admin, &selector.tenant, &args.source_schema).await?;
    let rows = admin
        .query(
            &wamn_scenario_catalog::sql::select_cases_for_suite_sql(),
            &[
                &selector.tenant,
                &selector.flow_id,
                &selector.flow_version,
                &selector.suite_id,
            ],
        )
        .await
        .context("read suite case ordinals")?;
    if rows.is_empty() {
        bail!("suite {:?} has no cases", selector.suite_id);
    }

    let template = execution_schema_template(&args.execution_schema_base, index)?;
    let mut schemas = Vec::with_capacity(rows.len());
    for row in rows {
        let ordinal: i32 = row.get(1);
        if ordinal < 0 {
            bail!("suite {:?} has a negative case ordinal", selector.suite_id);
        }
        let schema = template.replace("{ordinal}", &ordinal.to_string());
        schemas.push(schema);
        if let Err(error) = provision_execution_schema(
            admin,
            &args.admin_database_url,
            schemas.last().unwrap(),
            &selector.tenant,
            publication,
        )
        .await
        {
            return combine(
                Err(error),
                cleanup_schemas(admin, &schemas).await,
                "drop partially provisioned scenario schemas",
            );
        }
    }

    let output = Command::new(&args.worker)
        .arg("--log-level")
        .arg("error")
        .arg("--flowrunner")
        .arg(&args.flowrunner)
        .arg("--database-url")
        .arg(database_url)
        .arg("--tenant")
        .arg(&selector.tenant)
        .arg("--source-schema")
        .arg(&args.source_schema)
        .arg("--execution-schema-template")
        .arg(&template)
        .arg("--execution-id")
        .arg(format!("gate-{index}"))
        .arg("--flow-id")
        .arg(&selector.flow_id)
        .arg("--flow-version")
        .arg(selector.flow_version.to_string())
        .arg("--suite-id")
        .arg(&selector.suite_id)
        .output()
        .await
        .with_context(|| format!("spawn {}", args.worker.display()));

    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return combine(
                Err(error),
                cleanup_schemas(admin, &schemas).await,
                "drop scenario execution schemas",
            );
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }
    let result = async {
        match selected.expected {
            ExpectedExit::Pass | ExpectedExit::Success | ExpectedExit::Refusal(_) => {
                if !output.status.success() {
                    bail!(
                        "scenario-worker unexpectedly failed for {}",
                        selector.suite_id
                    );
                }
                let report = serde_json::from_slice::<ScenarioReport>(&output.stdout)
                    .with_context(|| format!("parse worker JSON for {}", selector.suite_id))?;
                let label = accepted_success_label(&report, selected.expected)?;
                println!("scenario-worker black-box {}: {label}", selector.suite_id);
            }
            ExpectedExit::Failure(fragment) => {
                if output.status.success() {
                    bail!(
                        "scenario-worker unexpectedly accepted {}",
                        selector.suite_id
                    );
                }
                let combined = format!("{stdout}\n{stderr}");
                if !combined.contains(fragment) {
                    bail!(
                        "scenario-worker failure for {} did not contain {fragment:?}",
                        selector.suite_id
                    );
                }
                println!(
                    "scenario-worker black-box {}: expected refusal/failure PASS",
                    selector.suite_id
                );
            }
        }
        let expected = expected_scenario_run_count(selected.expected, schemas.len());
        assert_scenario_run_pins(admin, &schemas, selector, publication, expected).await
    }
    .await;
    let cleanup = cleanup_schemas(admin, &schemas).await;
    combine(result, cleanup, "drop scenario execution schemas")?;
    assert_schemas_absent(admin, &schemas).await?;
    println!(
        "scenario-worker black-box {} schema-cleanup: PASS",
        selector.suite_id
    );
    Ok(())
}

async fn provision_execution_schema(
    client: &Client,
    admin_url: &str,
    schema: &str,
    tenant: &str,
    publication: Option<&ScenarioPublication>,
) -> anyhow::Result<()> {
    ScenarioSchemaName::new(schema.to_owned()).context("validate execution schema")?;
    drop_schema(client, schema).await?;
    if let Some(publication) = publication {
        publication.publish(admin_url, tenant, schema).await?;
    }
    ctl_process::run_checked([
        "reconcile-run-plane",
        "--admin-database-url",
        admin_url,
        "--schema",
        schema,
    ])
    .await
    .context("apply canonical run-plane schema")?;
    if publication.is_some() {
        return Ok(());
    }
    client
        .batch_execute(&format!(
            "CREATE TABLE {schema}.sink (\
                tenant_id text NOT NULL, run_id text NOT NULL, step int NOT NULL, \
                payload text NOT NULL, \
                CONSTRAINT sink_idem UNIQUE (tenant_id, run_id, step));\
             ALTER TABLE {schema}.sink ENABLE ROW LEVEL SECURITY;\
             ALTER TABLE {schema}.sink FORCE ROW LEVEL SECURITY;\
             CREATE POLICY sink_tenant ON {schema}.sink \
                USING (tenant_id = NULLIF(current_setting('app.tenant', true), '')) \
                WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));\
             GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.sink TO wamn_app;"
        ))
        .await
        .context("apply repository-only sink fixture")?;
    Ok(())
}

fn expected_scenario_run_count(expected: ExpectedExit, case_count: usize) -> Option<usize> {
    match expected {
        ExpectedExit::Pass | ExpectedExit::Failure("stored scenario assertions failed") => {
            Some(case_count)
        }
        ExpectedExit::Refusal(_) | ExpectedExit::Failure(_) => Some(0),
        ExpectedExit::Success => None,
    }
}

async fn assert_scenario_run_pins(
    client: &Client,
    schemas: &[String],
    selector: &SuiteSelector,
    publication: Option<&ScenarioPublication>,
    expected_count: Option<usize>,
) -> anyhow::Result<()> {
    let mut total = 0_usize;
    let expected_catalog = publication.map(|publication| publication.catalog_id.as_str());
    let expected_environment = publication.map(|_| RELEASE_ENVIRONMENT);
    let expected_catalog_version = publication.map(|_| i64::from(RELEASE_VERSION));
    for schema in schemas {
        let schema = ScenarioSchemaName::new(schema.clone())
            .with_context(|| format!("validate scenario pin schema {schema:?}"))?;
        let row = client
            .query_one(
                &format!(
                    "SELECT count(*)::bigint, \
                            count(*) FILTER (WHERE \
                              r.catalog_id IS NOT NULL \
                              AND r.catalog_version IS NOT NULL \
                              AND r.environment IS NOT NULL \
                              AND ($5::text IS NULL OR r.catalog_id = $5) \
                              AND ($6::text IS NULL OR r.environment = $6) \
                              AND ($7::bigint IS NULL OR r.catalog_version = $7) \
                              AND r.admission_context_version = '0.1' \
                              AND r.platform_revision <> '' \
                              AND r.invocation_context #>> '{{principal,tenant-id}}' = r.tenant_id \
                              AND r.invocation_context #>> '{{principal,environment}}' = r.environment \
                              AND r.invocation_context #>> '{{principal,catalog-id}}' = r.catalog_id \
                              AND r.invocation_context #>> '{{principal,catalog-version}}' = r.catalog_version::text \
                              AND r.invocation_context #>> '{{principal,run-id}}' = r.run_id \
                              AND r.invocation_context #>> '{{principal,flow-id}}' = r.flow_id \
                              AND r.invocation_context #>> '{{principal,flow-version}}' = r.flow_version::text \
                              AND r.invocation_context #>> '{{source,producer}}' = 'scenario' \
                              AND r.invocation_context #>> '{{source,suite-id}}' = $4 \
                              AND NULLIF(r.invocation_context #>> '{{source,case-id}}', '') IS NOT NULL \
                              AND EXISTS ( \
                                SELECT 1 \
                                  FROM catalog.catalog_heads AS h \
                                  JOIN catalog.release_flows AS rf \
                                    ON rf.tenant_id = h.tenant_id \
                                   AND rf.catalog_id = h.catalog_id \
                                   AND rf.catalog_version = h.applied_catalog_version \
                                  JOIN catalog.release_manifests AS rm \
                                    ON rm.tenant_id = rf.tenant_id \
                                   AND rm.catalog_id = rf.catalog_id \
                                   AND rm.catalog_version = rf.catalog_version \
                                  JOIN catalog.flow_artifacts AS a \
                                    ON a.tenant_id = rf.tenant_id \
                                   AND a.flow_id = rf.flow_id \
                                   AND a.flow_version = rf.flow_version \
                                 WHERE h.tenant_id = r.tenant_id \
                                   AND h.catalog_id = r.catalog_id \
                                   AND h.environment = r.environment \
                                   AND h.applied_catalog_version::bigint = r.catalog_version \
                                   AND rf.flow_id = r.flow_id \
                                   AND rf.flow_version = r.flow_version \
                                   AND a.artifact_hash = r.invocation_context #>> '{{principal,artifact-digest}}' \
                                   AND (SELECT count(*) \
                                          FROM jsonb_array_elements(rm.members_json) AS member \
                                         WHERE member ->> 'flow-id' = rf.flow_id \
                                           AND (member ->> 'flow-version')::int = rf.flow_version \
                                           AND member ->> 'artifact-hash' = a.artifact_hash) = 1 \
                              ))::bigint \
                       FROM {schema}.runs AS r \
                      WHERE r.tenant_id = $1 AND r.flow_id = $2 \
                        AND r.flow_version = $3 AND r.trigger_source = 'scenario'"
                ),
                &[
                    &selector.tenant,
                    &selector.flow_id,
                    &selector.flow_version,
                    &selector.suite_id,
                    &expected_catalog,
                    &expected_environment,
                    &expected_catalog_version,
                ],
            )
            .await
            .with_context(|| format!("verify immutable scenario pins in {schema}"))?;
        let schema_total =
            usize::try_from(row.get::<_, i64>(0)).context("scenario run count exceeds usize")?;
        let valid = usize::try_from(row.get::<_, i64>(1))
            .context("valid scenario pin count exceeds usize")?;
        if schema_total != valid {
            bail!(
                "scenario schema {schema} contains {schema_total} runs but only {valid} exact immutable release pins"
            );
        }
        total = total
            .checked_add(schema_total)
            .context("scenario run count overflow")?;
    }
    if let Some(expected) = expected_count
        && total != expected
    {
        bail!("scenario suite expected {expected} admitted runs, found {total}");
    }
    println!(
        "scenario-worker black-box {} immutable-pin: PASS ({total} runs)",
        selector.suite_id
    );
    Ok(())
}

async fn cleanup_schemas(client: &Client, schemas: &[String]) -> anyhow::Result<()> {
    let mut first_error = None;
    for schema in schemas {
        if let Err(error) = drop_schema(client, schema).await {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn assert_schemas_absent(client: &Client, schemas: &[String]) -> anyhow::Result<()> {
    for schema in schemas {
        let present: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)",
                &[schema],
            )
            .await?
            .get(0);
        if present {
            bail!("scenario execution schema {schema:?} survived cleanup");
        }
    }
    Ok(())
}

async fn drop_schema(client: &Client, schema: &str) -> anyhow::Result<()> {
    let schema = ScenarioSchemaName::new(schema.to_owned())
        .with_context(|| format!("invalid scenario schema {schema:?}"))?;
    client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .await
        .with_context(|| format!("drop schema {schema}"))
}

async fn scope_session(client: &Client, tenant: &str, schema: &str) -> anyhow::Result<()> {
    let schema = ScenarioSchemaName::new(schema.to_owned())
        .with_context(|| format!("invalid scenario schema {schema:?}"))?;
    client
        .query_one(
            "SELECT set_config('app.tenant', $1, false), set_config('search_path', $2, false)",
            &[&tenant, &schema.as_str()],
        )
        .await?;
    Ok(())
}

fn combine<T>(
    result: anyhow::Result<T>,
    cleanup: anyhow::Result<()>,
    cleanup_action: &str,
) -> anyhow::Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error).context(cleanup_action.to_owned()),
        (Err(error), Err(cleanup_error)) => Err(error).with_context(|| {
            format!("{cleanup_action} also failed after the primary error: {cleanup_error:#}")
        }),
    }
}

async fn seed_demo(
    client: &Client,
    admin_url: &str,
    schema: &str,
    tenant: Option<&str>,
    node: &Path,
) -> anyhow::Result<ScenarioPublication> {
    let tenant = tenant.context("--seed-demo needs --tenant")?;
    ScenarioSchemaName::new(schema.to_owned()).context("validate source schema")?;
    let publication = ScenarioPublication::create(node)?;
    drop_schema(client, schema).await?;
    publication.publish(admin_url, tenant, schema).await?;
    scope_session(client, tenant, schema).await?;

    seed_flow_version(
        client,
        tenant,
        &publication.demo_flow_id,
        1,
        true,
        &demo_graph(&publication.demo_flow_id),
        true,
    )
    .await?;
    seed_flow_version(
        client,
        tenant,
        &publication.undrivable_flow_id,
        1,
        true,
        &undrivable_graph(&publication.undrivable_flow_id),
        true,
    )
    .await?;
    seed_suite_cases(
        client,
        tenant,
        &publication.demo_flow_id,
        "success",
        success_cases(&publication.demo_flow_id),
    )
    .await?;
    seed_malformed_case(client, tenant, &publication.demo_flow_id).await?;
    seed_suite_cases(
        client,
        tenant,
        &publication.undrivable_flow_id,
        "undrivable",
        vec![(
            "undrivable",
            0,
            completion_case("undrivable", &publication.undrivable_flow_id),
        )],
    )
    .await?;
    seed_suite_cases(
        client,
        tenant,
        &publication.demo_flow_id,
        "assertion-failure",
        vec![(
            "assertion-failure",
            0,
            serde_json::json!({
                "schema-version": "0.1",
                "name": "assertion-failure",
                "flow-ref": {"flow-id": publication.demo_flow_id.as_str(), "version": 1},
                "input": {},
                "expect": [{"run-terminal-outcome": {"status": "failed"}}]
            })
            .to_string(),
        )],
    )
    .await?;
    let poisoned = client
        .execute(
            "UPDATE flows SET graph_json = '{}'::jsonb \
             WHERE tenant_id = $1 AND flow_id IN ($2, $3) AND version = 1",
            &[
                &tenant,
                &publication.demo_flow_id,
                &publication.undrivable_flow_id,
            ],
        )
        .await
        .context("poison mutable scenario flow projections")?;
    if poisoned != 2 {
        bail!("mutable scenario projection poison affected {poisoned} rows");
    }
    Ok(publication)
}

async fn seed_malformed_case(
    client: &Client,
    tenant: &str,
    demo_flow_id: &str,
) -> anyhow::Result<()> {
    seed_suite_cases(
        client,
        tenant,
        demo_flow_id,
        "malformed",
        vec![("malformed", 0, completion_case("malformed", demo_flow_id))],
    )
    .await?;
    // Corrupt only the repository-owned negative fixture after the normal
    // validating write path so the product worker must reject it on read.
    let updated = client
        .execute(
            "UPDATE test_cases SET case_body = '{}'::jsonb \
             WHERE tenant_id = $1 AND flow_id = $2 AND flow_version = 1 \
               AND suite_id = 'malformed' AND case_id = 'malformed'",
            &[&tenant, &demo_flow_id],
        )
        .await
        .context("corrupt repository-only malformed scenario fixture")?;
    if updated != 1 {
        bail!("malformed scenario fixture update affected {updated} rows");
    }
    Ok(())
}

async fn seed_suite_cases(
    client: &Client,
    tenant: &str,
    flow_id: &str,
    suite_id: &str,
    cases: Vec<(&str, i32, String)>,
) -> anyhow::Result<()> {
    seed_test_suite(client, tenant, flow_id, 1, suite_id, suite_id).await?;
    for (case_id, ordinal, body) in cases {
        seed_test_case(
            client, tenant, flow_id, 1, suite_id, case_id, ordinal, &body,
        )
        .await?;
    }
    Ok(())
}

fn success_cases(demo_flow_id: &str) -> Vec<(&'static str, i32, String)> {
    vec![
        (
            "success",
            0,
            completion_test_case("success", demo_flow_id, serde_json::json!({})),
        ),
        (
            "writes-receipt",
            1,
            completion_test_case(
                "writes-receipt",
                demo_flow_id,
                serde_json::json!({"receipt": "demo"}),
            ),
        ),
    ]
}

fn completion_test_case(name: &str, flow_id: &str, input: serde_json::Value) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "name": name,
        "flow-ref": {"flow-id": flow_id, "version": 1},
        "input": input,
        "expect": [{"run-terminal-outcome": {"status": "completed"}}]
    })
    .to_string()
}

fn completion_case(name: &str, flow_id: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "name": name,
        "flow-ref": {"flow-id": flow_id, "version": 1},
        "input": {},
        "expect": [{"run-terminal-outcome": {"status": "completed"}}]
    })
    .to_string()
}

fn demo_catalog(catalog_id: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "catalog-id": catalog_id,
        "version": RELEASE_VERSION,
        "entities": [{
            "id": "sink",
            "name": "sink",
            "fields": [{
                "id": "receipt",
                "name": "receipt",
                "type": {"kind": "text"},
                "nullable": true
            }]
        }]
    })
    .to_string()
}

fn demo_graph(flow_id: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "flow-id": flow_id,
        "version": RELEASE_VERSION,
        "nodes": [
            {"id": "request", "type": "request", "config": {"input-schema": true}},
            {"id": "write", "type": "postgres", "config": {"entity": "sink", "op": "create"}},
            {"id": "respond", "type": "respond", "config": {"status": 200}}
        ],
        "edges": [
            {"from": "request", "to": "write"},
            {"from": "write", "to": "respond"}
        ]
    })
    .to_string()
}

fn undrivable_graph(flow_id: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "flow-id": flow_id,
        "version": RELEASE_VERSION,
        "nodes": [
            {"id": "request", "type": "request", "config": {"input-schema": true}},
            {"id": "recommend", "type": UNDRIVABLE_NODE_TYPE},
            {"id": "respond", "type": "respond", "config": {"status": 200}}
        ],
        "edges": [
            {"from": "request", "to": "recommend"},
            {"from": "recommend", "to": "respond"}
        ]
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_suite_is_an_expected_product_process_failure() {
        let selected = selected("t", "demo", "malformed", ExpectedExit::Failure("parse"));
        assert!(matches!(selected.expected, ExpectedExit::Failure("parse")));
    }

    #[test]
    fn canonical_refusal_is_visible_and_requires_the_seeded_node_type() {
        let report = ScenarioReport {
            execution_id: "gate-2".into(),
            scenario_epoch_secs: Some(1_700_000_000),
            flow_id: "undrivable".into(),
            flow_version: 1,
            suite_id: "undrivable".into(),
            refusal: Some(ScenarioRefusal::UndrivableNodes {
                node_types: vec![UNDRIVABLE_NODE_TYPE.into()],
            }),
            cases: Vec::new(),
        };

        assert_eq!(
            accepted_success_label(&report, ExpectedExit::Refusal(UNDRIVABLE_NODE_TYPE)).unwrap(),
            "expected refusal PASS"
        );
        assert_eq!(
            accepted_success_label(&report, ExpectedExit::Success).unwrap(),
            "REFUSED"
        );
        assert!(accepted_success_label(&report, ExpectedExit::Pass).is_err());
        assert!(accepted_success_label(&report, ExpectedExit::Refusal("wrong-node")).is_err());
    }

    #[test]
    fn schema_cleanup_uses_the_canonical_scenario_name_boundary() {
        assert!(ScenarioSchemaName::new("tk_suiteexec_0_12").is_ok());
        assert!(ScenarioSchemaName::new("tk;drop").is_err());
        assert!(ScenarioSchemaName::new("s".repeat(64)).is_err());
    }

    #[test]
    fn worker_template_preserves_the_literal_ordinal_placeholder() {
        assert_eq!(
            execution_schema_template("tk_suiteexec", 3).unwrap(),
            "tk_suiteexec_3_{ordinal}"
        );
    }

    #[test]
    fn success_assertion_failure_and_undrivable_fixtures_are_distinct() {
        assert_ne!(demo_graph("demo"), undrivable_graph("undrivable"));
        assert!(success_cases("demo")[0].2.contains("\"completed\""));
        assert!(completion_case("u", "undrivable").contains("undrivable"));
    }

    #[test]
    fn publishable_fixture_graphs_do_not_reintroduce_retired_entry_or_trigger_fields() {
        for graph in [demo_graph("demo"), undrivable_graph("undrivable")] {
            let graph: serde_json::Value = serde_json::from_str(&graph).unwrap();
            let graph = graph.as_object().unwrap();
            assert!(!graph.contains_key("entry"));
            assert!(!graph.contains_key("trigger"));
        }
    }

    #[test]
    fn hermetic_run_count_expectations_distinguish_preflight_from_execution() {
        assert_eq!(expected_scenario_run_count(ExpectedExit::Pass, 2), Some(2));
        assert_eq!(
            expected_scenario_run_count(ExpectedExit::Refusal(UNDRIVABLE_NODE_TYPE), 1),
            Some(0)
        );
        assert_eq!(
            expected_scenario_run_count(ExpectedExit::Failure("parse stored case"), 1),
            Some(0)
        );
        assert_eq!(
            expected_scenario_run_count(
                ExpectedExit::Failure("stored scenario assertions failed"),
                1,
            ),
            Some(1)
        );
    }

    #[test]
    fn success_cases_use_only_terminal_run_assertions() {
        let cases = success_cases("demo");

        assert_eq!(cases.len(), 2);
        for (_, _, body) in cases {
            let case: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                case["expect"],
                serde_json::json!([{"run-terminal-outcome": {"status": "completed"}}])
            );
        }
    }

    /// The `--impact-report` input row is the `wamn_schema_control::impact::SuiteEdge`
    /// shape field-for-field: names `tenant / flow_id / flow_version / suite_id`,
    /// types `String / String / i32 / String`. A wrong field name or an extra field
    /// is REFUSED, so the wamn-12g input contract stays locked.
    #[test]
    fn suite_selector_matches_the_suite_edge_shape() {
        // The exact SuiteEdge shape parses, with the right values/types.
        let sel: SuiteSelector =
            serde_json::from_str(r#"{"tenant":"t","flow_id":"f","flow_version":3,"suite_id":"s"}"#)
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
        // A camelCase / renamed field drops a REQUIRED field ⇒ parse fails; an
        // EXTRA field is refused by `deny_unknown_fields`.
        for wrong in [
            r#"{"tenant":"t","flow":"f","flow_version":1,"suite_id":"s"}"#,
            r#"{"tenant":"t","flow_id":"f","version":1,"suite_id":"s"}"#,
            r#"{"tenant":"t","flowId":"f","flowVersion":1,"suiteId":"s"}"#,
            r#"{"tenant":"t","flow_id":"f","flow_version":1,"suite_id":"s","extra":1}"#,
        ] {
            assert!(
                serde_json::from_str::<SuiteSelector>(wrong).is_err(),
                "a wrong or extra field must be refused: {wrong}"
            );
        }
    }

    /// Producer ↔ consumer: what the wamn-12g seam EMITS
    /// (`wamn_schema_control::impact::suite_selectors_json`, the flattened
    /// `ImpactReport.entities[].suites[]`) is exactly what this executor READS into
    /// its `SuiteSelector` rows — same field names, same types, same order, no
    /// rendered text in between. Guards the two shapes against drifting apart.
    #[test]
    fn flattened_impact_suites_deserialize_as_suite_selectors() {
        use wamn_schema_control::impact::{
            EntityChangeKind, EntityImpact, ImpactReport, SuiteEdge, suite_selectors_json,
        };

        let edge = |tenant: &str, flow: &str, version: i32, suite: &str| SuiteEdge {
            tenant: tenant.into(),
            flow_id: flow.into(),
            flow_version: version,
            suite_id: suite.into(),
        };
        let impacted = |suites: Vec<SuiteEdge>| EntityImpact {
            entity_id: "receipts".into(),
            entity_name: "receipts".into(),
            change: EntityChangeKind::Changed,
            destructive: true,
            flows_via_registration: Vec::new(),
            flows_via_node_config: Vec::new(),
            suites,
            api_resources: Vec::new(),
        };

        // Two affected entities sharing a suite, deliberately out of order.
        let shared = edge("acme", "receiving", 1, "happy");
        let report = ImpactReport {
            entities: vec![
                impacted(vec![edge("acme", "receiving", 2, "happy"), shared.clone()]),
                impacted(vec![shared, edge("acme", "billing", 1, "b")]),
            ],
        };

        let emitted = suite_selectors_json(&report).expect("complete suite edges flatten");
        let read: Vec<SuiteSelector> =
            serde_json::from_str(&emitted).expect("the emitted array is SuiteSelector-shaped");

        assert_eq!(
            read,
            vec![
                SuiteSelector {
                    tenant: "acme".into(),
                    flow_id: "billing".into(),
                    flow_version: 1,
                    suite_id: "b".into(),
                },
                SuiteSelector {
                    tenant: "acme".into(),
                    flow_id: "receiving".into(),
                    flow_version: 1,
                    suite_id: "happy".into(),
                },
                SuiteSelector {
                    tenant: "acme".into(),
                    flow_id: "receiving".into(),
                    flow_version: 2,
                    suite_id: "happy".into(),
                },
            ],
            "the producer's de-duplicated, identity-ordered array round-trips into the consumer",
        );
        // Every read selector survives this gate's own non-negative version check.
        assert!(read.iter().all(|s| s.flow_version >= 0));
    }
}
