//! Repository-only fixture and process adapter for the stored-scenario worker gate.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use serde::Deserialize;
use tokio::process::Command;
use tokio_postgres::{Client, NoTls};

use wamn_ctl::publish_catalog::{ensure_flow_registry, ensure_flow_tests};
use wamn_gate_harness::{seed_flow_version, seed_test_case, seed_test_suite};
use wamn_scenario_model::{ScenarioRefusal, ScenarioReport};
use wamn_scenario_runtime::ScenarioSchemaName;
use wamn_schema_control::BareSchemaName;

const DEMO_FLOW_ID: &str = "tk-demo-flow";
const UNDRIVABLE_FLOW_ID: &str = "tk-undrivable-flow";

/// Stored-suite compatibility inputs forwarded to the product worker process.
#[derive(Debug)]
pub struct StoredSuiteGateArgs {
    pub worker: PathBuf,
    pub flowrunner: PathBuf,
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
    Success,
    Refusal(&'static str),
    Failure(&'static str),
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
        if args.seed_demo {
            seed_demo(&admin, &args.source_schema, args.tenant.as_deref()).await?;
        }
        let suites = select_suites(&admin, &args).await?;
        if suites.is_empty() {
            bail!("stored-suite selection resolved to no suites");
        }
        for (index, selected) in suites.iter().enumerate() {
            run_selected_suite(&admin, &database_url, &args, selected, index).await?;
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
) -> anyhow::Result<Vec<SelectedSuite>> {
    let source_count = usize::from(args.seed_demo)
        + usize::from(!args.suites.is_empty())
        + usize::from(args.impact_report.is_some());
    if source_count != 1 {
        bail!("exactly one stored-suite selection source is required");
    }
    if args.seed_demo {
        let tenant = args.tenant.clone().context("--seed-demo needs --tenant")?;
        return Ok(vec![
            selected(&tenant, DEMO_FLOW_ID, "success", ExpectedExit::Success),
            selected(
                &tenant,
                DEMO_FLOW_ID,
                "malformed",
                ExpectedExit::Failure("parse stored case"),
            ),
            selected(
                &tenant,
                UNDRIVABLE_FLOW_ID,
                "undrivable",
                ExpectedExit::Refusal("repository-unknown-node"),
            ),
            selected(
                &tenant,
                DEMO_FLOW_ID,
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
        if let Err(error) = provision_execution_schema(admin, schemas.last().unwrap()).await {
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

    let cleanup = cleanup_schemas(admin, &schemas).await;
    let output = combine(output, cleanup, "drop scenario execution schemas")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }
    match selected.expected {
        ExpectedExit::Success | ExpectedExit::Refusal(_) => {
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
    assert_schemas_absent(admin, &schemas).await?;
    println!(
        "scenario-worker black-box {} schema-cleanup: PASS",
        selector.suite_id
    );
    Ok(())
}

async fn provision_execution_schema(client: &Client, schema: &str) -> anyhow::Result<()> {
    let schema_name = BareSchemaName::new(schema).context("validate execution schema")?;
    drop_schema(client, schema).await?;
    wamn_ctl::reconcile_run_plane::reconcile(client, &schema_name, true)
        .await
        .context("apply canonical run-plane schema")?;
    ensure_flow_registry(client, &schema_name)
        .await
        .context("apply canonical flow registry")?;
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

async fn seed_demo(client: &Client, schema: &str, tenant: Option<&str>) -> anyhow::Result<()> {
    let tenant = tenant.context("--seed-demo needs --tenant")?;
    let schema_name = BareSchemaName::new(schema).context("validate source schema")?;
    drop_schema(client, schema).await?;
    wamn_ctl::reconcile_run_plane::reconcile(client, &schema_name, true)
        .await
        .context("apply canonical source run-plane schema")?;
    ensure_flow_registry(client, &schema_name).await?;
    ensure_flow_tests(client, &schema_name).await?;
    scope_session(client, tenant, schema).await?;

    seed_flow_version(client, tenant, DEMO_FLOW_ID, 1, true, &demo_graph(), true).await?;
    seed_flow_version(
        client,
        tenant,
        UNDRIVABLE_FLOW_ID,
        1,
        true,
        &undrivable_graph(),
        true,
    )
    .await?;
    seed_suite_cases(client, tenant, DEMO_FLOW_ID, "success", success_cases()).await?;
    seed_malformed_case(client, tenant).await?;
    seed_suite_cases(
        client,
        tenant,
        UNDRIVABLE_FLOW_ID,
        "undrivable",
        vec![(
            "undrivable",
            0,
            completion_case("undrivable", UNDRIVABLE_FLOW_ID),
        )],
    )
    .await?;
    seed_suite_cases(
        client,
        tenant,
        DEMO_FLOW_ID,
        "assertion-failure",
        vec![(
            "assertion-failure",
            0,
            serde_json::json!({
                "schema-version": "0.1",
                "name": "assertion-failure",
                "flow-ref": {"flow-id": DEMO_FLOW_ID, "version": 1},
                "input": {},
                "expect": [{"run-outcome": {"status": "failed"}}]
            })
            .to_string(),
        )],
    )
    .await
}

async fn seed_malformed_case(client: &Client, tenant: &str) -> anyhow::Result<()> {
    seed_suite_cases(
        client,
        tenant,
        DEMO_FLOW_ID,
        "malformed",
        vec![("malformed", 0, completion_case("malformed", DEMO_FLOW_ID))],
    )
    .await?;
    // Corrupt only the repository-owned negative fixture after the normal
    // validating write path so the product worker must reject it on read.
    let updated = client
        .execute(
            "UPDATE test_cases SET case_body = '{}'::jsonb \
             WHERE tenant_id = $1 AND flow_id = $2 AND flow_version = 1 \
               AND suite_id = 'malformed' AND case_id = 'malformed'",
            &[&tenant, &DEMO_FLOW_ID],
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

fn success_cases() -> Vec<(&'static str, i32, String)> {
    vec![
        ("success", 0, completion_case("success", DEMO_FLOW_ID)),
        (
            "db-state",
            1,
            serde_json::json!({
                "schema-version": "0.1",
                "name": "db-state",
                "flow-ref": {"flow-id": DEMO_FLOW_ID, "version": 1},
                "input": {"receipt": "demo"},
                "expect": [
                    {"run-outcome": {"status": "completed"}},
                    {"db-state": {
                        "query": "SELECT to_jsonb(sink) FROM sink",
                        "params": [],
                        "expect": {"row-count": 1}
                    }}
                ]
            })
            .to_string(),
        ),
    ]
}

fn completion_case(name: &str, flow_id: &str) -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "name": name,
        "flow-ref": {"flow-id": flow_id, "version": 1},
        "input": {},
        "expect": [{"run-outcome": {"status": "completed"}}]
    })
    .to_string()
}

fn demo_graph() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{DEMO_FLOW_ID}","version":1,
        "trigger":{{"type":"webhook"}},"entry":"in",
        "nodes":[{{"id":"in","type":"webhook-in"}},{{"id":"w","type":"pg-write"}},
        {{"id":"out","type":"respond"}}],
        "edges":[{{"from":"in","to":"w"}},{{"from":"w","to":"out"}}]}}"#
    )
}

fn undrivable_graph() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{UNDRIVABLE_FLOW_ID}","version":1,
        "trigger":{{"type":"manual"}},"entry":"unknown",
        "nodes":[{{"id":"unknown","type":"repository-unknown-node"}}],"edges":[]}}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_suite_is_an_expected_product_process_failure() {
        let selected = selected(
            "t",
            DEMO_FLOW_ID,
            "malformed",
            ExpectedExit::Failure("parse"),
        );
        assert!(matches!(selected.expected, ExpectedExit::Failure("parse")));
    }

    #[test]
    fn canonical_refusal_is_visible_and_requires_the_seeded_node_type() {
        let report = ScenarioReport {
            execution_id: "gate-2".into(),
            scenario_epoch_secs: Some(1_700_000_000),
            flow_id: UNDRIVABLE_FLOW_ID.into(),
            flow_version: 1,
            suite_id: "undrivable".into(),
            refusal: Some(ScenarioRefusal::UndrivableNodes {
                node_types: vec!["repository-unknown-node".into()],
            }),
            cases: Vec::new(),
        };

        assert_eq!(
            accepted_success_label(&report, ExpectedExit::Refusal("repository-unknown-node"))
                .unwrap(),
            "expected refusal PASS"
        );
        assert_eq!(
            accepted_success_label(&report, ExpectedExit::Success).unwrap(),
            "REFUSED"
        );
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
        assert_ne!(demo_graph(), undrivable_graph());
        assert!(success_cases()[0].2.contains("\"completed\""));
        assert!(completion_case("u", UNDRIVABLE_FLOW_ID).contains(UNDRIVABLE_FLOW_ID));
    }
}
