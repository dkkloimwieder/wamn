//! Real-PostgreSQL acceptance gate for the internal flow-authoring loop.
//!
//! The ignored recipe in `docs/archive/build-and-test.md` supplies one disposable
//! database, distinct canonical app/author credentials, and the flowrunner
//! component compiled from this checkout.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};

use wamn_catalog::{
    Artifact, ExecutionBundleInput, ExecutionBundlePackaging, ExecutionPlugManifest,
    NodeImplementation,
};
use wamn_scenario_model::{
    AuthoringExecutionResult, AuthoringReport, AuthoringReportState, ExecutionLineage,
    PendingAuthoringReportReason, RunStatus, ScenarioRefusal,
};
use wamn_scenario_worker::ScenarioWorkerArgs;
use wamn_scenario_worker::authoring::{
    AuthoringReportQuery, DraftBundleInputs, InternalAuthoringBackend, SaveFlowDraft,
    SaveFlowDraftResult, ValidateFlowDraft,
};
use wamn_schema_control::{BareSchemaName, rewrite_schema};

const TENANT: &str = "authoring-loop-tenant";
const SOURCE_SCHEMA: &str = "authoring_loop_source";
const EXECUTION_SCHEMA: &str = "authoring_loop_case_0";
const FLOW_ID: &str = "authoring-loop-flow";
const SUITE_ID: &str = "authoring-loop-suite";
const CASE_ID: &str = "authoring-loop-case";
const DRAFT_ID: &str = "authoring-loop-draft";

const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const FLOW_TESTS_SQL: &str = include_str!("../../../deploy/sql/flow-tests.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../deploy/sql/run-queue.sql");

const RELEASE_GRAPH: &str = r#"{
  "schema-version":"0.1",
  "flow-id":"authoring-loop-flow",
  "version":1,
  "nodes":[
    {"id":"request","type":"request","config":{"input-schema":true}},
    {"id":"respond","type":"respond","config":{"status":200}}
  ],
  "edges":[{"from":"request","to":"respond"}]
}"#;

const DRAFT_GRAPH: &str = r#"{
  "schema-version":"0.1",
  "flow-id":"authoring-loop-flow",
  "version":2,
  "nodes":[
    {"id":"request","type":"request","config":{"input-schema":true}},
    {"id":"draft-only","type":"transform",
     "config":{"expression":"{marker: 'validated-draft-v2'}"}},
    {"id":"respond","type":"respond","config":{"status":200}}
  ],
  "edges":[
    {"from":"request","to":"draft-only"},
    {"from":"draft-only","to":"respond"}
  ]
}"#;

fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("{name} must be set for the ignored live gate"))
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, task))
}

fn schema_sql(record: &str, schema: &str) -> String {
    let schema = BareSchemaName::new(schema).expect("gate schema is a valid bare identifier");
    rewrite_schema(record, &schema)
}

fn release_artifact() -> anyhow::Result<(wamn_flow::Flow, Artifact)> {
    let flow = wamn_flow::Flow::from_json(RELEASE_GRAPH)?;
    let implementations = ["request", "respond"]
        .into_iter()
        .map(|node_type| {
            let descriptor = wamn_standard_nodes::describe(node_type)
                .with_context(|| format!("resolve standard node {node_type}"))?;
            let contract = wamn_standard_nodes::resolve_descriptor(descriptor)?;
            NodeImplementation::from_resolved_platform_contract(contract).map_err(Into::into)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let artifact = Artifact::new(TENANT, &flow, implementations)?;
    Ok((flow, artifact))
}

async fn reset_and_provision(admin: &mut Client) -> anyhow::Result<String> {
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS authoring_loop_case_0 CASCADE; \
             DROP SCHEMA IF EXISTS authoring_loop_source CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP ROLE IF EXISTS wamn_authoring_loop_author; \
             DROP ROLE IF EXISTS wamn_scenario_author; \
             DROP ROLE IF EXISTS wamn_app; \
             CREATE ROLE wamn_app LOGIN PASSWORD 'wamn-app-live' \
               NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_scenario_author NOLOGIN \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_authoring_loop_author LOGIN PASSWORD 'wamn-author-live' \
               NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
             GRANT wamn_scenario_author TO wamn_authoring_loop_author;",
        )
        .await
        .context("reset authoring-loop schemas and roles")?;
    admin
        .batch_execute(CATALOG_SQL)
        .await
        .context("provision catalog schema")?;
    for (schema, records) in [
        (
            SOURCE_SCHEMA,
            &[RUN_STATE_SQL, FLOWS_SQL, FLOW_TESTS_SQL][..],
        ),
        (EXECUTION_SCHEMA, &[RUN_STATE_SQL, RUN_QUEUE_SQL][..]),
    ] {
        for record in records {
            admin
                .batch_execute(&schema_sql(record, schema))
                .await
                .with_context(|| format!("provision {schema}"))?;
        }
    }

    let (release_flow, release_artifact) = release_artifact()?;
    let graph_json = release_flow.to_json();
    let interface_bundle_json = String::from_utf8(
        release_artifact
            .interface_bundle()
            .canonical_bytes()
            .to_vec(),
    )?;
    let component_digests = serde_json::to_value(release_artifact.supplied_components())?;
    let occurrence_recovery_json =
        String::from_utf8(release_artifact.occurrence_recovery_bytes().to_vec())?;
    let artifact_hash = release_artifact
        .identity()
        .artifact_hash()
        .as_str()
        .to_string();
    let members = serde_json::json!([{
        "flow-id": FLOW_ID,
        "flow-version": 1,
        "artifact-hash": artifact_hash,
    }]);
    let case = serde_json::json!({
        "schema-version": "0.1",
        "name": CASE_ID,
        "flow-ref": {"flow-id": FLOW_ID, "version": 1},
        "input": {},
        "expect": [
            {"path-equals": {"pointer": "/marker", "value": "validated-draft-v2"}},
            {"run-outcome": {"status": "completed"}},
        ],
    });
    let transaction = admin.transaction().await?;
    transaction
        .execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,name,state) \
             VALUES ($1,'authoring-loop-catalog',1,'dev','0.1','live','applied')",
            &[&TENANT],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                artifact_hash,interface_bundle_json,interface_bundle_hash,component_digests, \
                occurrence_recovery_json,occurrence_recovery_hash) \
             VALUES ($1,$2,1,'0.1',$3::text::jsonb,$4,$5,$6,$7,$8,$9,$10)",
            &[
                &TENANT,
                &FLOW_ID,
                &graph_json,
                &release_artifact.graph_hash(),
                &artifact_hash,
                &interface_bundle_json,
                &release_artifact.interface_bundle().hash(),
                &component_digests,
                &occurrence_recovery_json,
                &release_artifact.occurrence_recovery_hash(),
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version,members_json) \
             VALUES ($1,'authoring-loop-catalog',1,$2)",
            &[&TENANT, &members],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
             VALUES ($1,'authoring-loop-catalog',1,$2,1)",
            &[&TENANT, &FLOW_ID],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.catalog_heads \
               (tenant_id,catalog_id,environment,applied_catalog_version) \
             VALUES ($1,'authoring-loop-catalog','dev',1)",
            &[&TENANT],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO authoring_loop_source.flows \
               (tenant_id,flow_id,version,active,graph_json) \
             VALUES ($1,$2,1,true,$3::text::jsonb)",
            &[&TENANT, &FLOW_ID, &graph_json],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO authoring_loop_source.test_suites \
               (tenant_id,flow_id,flow_version,suite_id,name) \
             VALUES ($1,$2,1,$3,'authoring loop live')",
            &[&TENANT, &FLOW_ID, &SUITE_ID],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO authoring_loop_source.test_cases \
               (tenant_id,flow_id,flow_version,suite_id,case_id,ordinal,case_body) \
             VALUES ($1,$2,1,$3,$4,0,$5)",
            &[&TENANT, &FLOW_ID, &SUITE_ID, &CASE_ID, &case],
        )
        .await?;
    transaction.commit().await?;
    Ok(artifact_hash)
}

fn digest(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

fn bundle() -> anyhow::Result<DraftBundleInputs> {
    Ok(DraftBundleInputs {
        packaging: ExecutionBundlePackaging::CapabilityClass,
        runner_identity: "flowrunner@authoring-loop-live".to_string(),
        composition_tool: ExecutionBundleInput::new("wac@authoring-loop-live", digest('4'))?,
        plugs: vec![ExecutionPlugManifest::new(
            "standard-nodes@authoring-loop-live",
            vec![
                "request".to_string(),
                "respond".to_string(),
                "transform".to_string(),
            ],
            digest('5'),
        )?],
        adapters: Vec::new(),
    })
}

fn args(app_url: &str, flowrunner: &Path, execution_id: &str) -> ScenarioWorkerArgs {
    ScenarioWorkerArgs {
        flowrunner: flowrunner.to_path_buf(),
        database_url: Some(app_url.to_string()),
        tenant: TENANT.to_string(),
        source_schema: SOURCE_SCHEMA.to_string(),
        execution_schema_template: "authoring_loop_case_{ordinal}".to_string(),
        execution_id: execution_id.to_string(),
        flow_id: FLOW_ID.to_string(),
        flow_version: 1,
        suite_id: SUITE_ID.to_string(),
        scenario_credentials_file: None,
        project: "authoring-loop-live".to_string(),
        allowed_hosts: Vec::new(),
        epoch_secs: 1_700_000_000,
        random_seed: 7,
        lease_ttl_ms: 30_000,
    }
}

fn finalized(result: AuthoringExecutionResult) -> AuthoringReport {
    match result {
        AuthoringExecutionResult::Finalized(report) => report,
        AuthoringExecutionResult::Pending(report) => {
            panic!("expected finalized authoring report, got {report:?}")
        }
    }
}

async fn release_counts(admin: &Client) -> anyhow::Result<(i64, i64, i64)> {
    let row = admin
        .query_one(
            "SELECT (SELECT count(*) FROM catalog.flow_artifacts), \
                    (SELECT count(*) FROM catalog.release_flows), \
                    (SELECT count(*) FROM catalog.release_manifests)",
            &[],
        )
        .await?;
    Ok((row.get(0), row.get(1), row.get(2)))
}

async fn run_count(admin: &Client) -> anyhow::Result<i64> {
    Ok(admin
        .query_one(
            "SELECT count(*) FROM authoring_loop_case_0.runs WHERE tenant_id=$1",
            &[&TENANT],
        )
        .await?
        .get(0))
}

async fn run_admission(
    admin: &Client,
    run_id: &str,
) -> anyhow::Result<(String, serde_json::Value)> {
    let row = admin
        .query_one(
            "SELECT trigger_source, invocation_context \
             FROM authoring_loop_case_0.runs WHERE tenant_id=$1 AND run_id=$2",
            &[&TENANT, &run_id],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the exact disposable-PostgreSQL + compiled-flowrunner recipe"]
async fn authoring_loop_live() -> anyhow::Result<()> {
    let admin_url = env("WAMN_AUTHORING_LOOP_ADMIN_PG_URL")?;
    let author_url = env("WAMN_AUTHORING_LOOP_AUTHOR_PG_URL")?;
    let app_url = env("WAMN_AUTHORING_LOOP_APP_PG_URL")?;
    let flowrunner = PathBuf::from(env("WAMN_AUTHORING_LOOP_FLOWRUNNER")?);
    let flowrunner_bytes = std::fs::read(&flowrunner)
        .with_context(|| format!("read compiled flowrunner {}", flowrunner.display()))?;
    anyhow::ensure!(!flowrunner_bytes.is_empty(), "compiled flowrunner is empty");

    let (mut admin, admin_task) = connect(&admin_url).await?;
    let release_artifact_hash = reset_and_provision(&mut admin).await?;
    let baseline_release_counts = release_counts(&admin).await?;
    assert_eq!(baseline_release_counts, (1, 1, 1));

    let mut backend = InternalAuthoringBackend::connect(&author_url, TENANT, SOURCE_SCHEMA).await?;
    let saved = backend
        .save_flow_draft(&SaveFlowDraft {
            tenant_id: TENANT.to_string(),
            draft_id: DRAFT_ID.to_string(),
            flow_id: FLOW_ID.to_string(),
            expected_revision: 0,
            definition: DRAFT_GRAPH.to_string(),
        })
        .await?;
    let (revision, edited_at) = match saved {
        SaveFlowDraftResult::Saved {
            revision,
            edited_at,
        } => (revision, edited_at),
        SaveFlowDraftResult::RevisionConflict => anyhow::bail!("fresh draft save conflicted"),
    };
    assert_eq!(revision, 1);

    let pin = backend
        .validate_flow_draft(
            &ValidateFlowDraft {
                tenant_id: TENANT.to_string(),
                draft_id: DRAFT_ID.to_string(),
                draft_revision: revision,
                catalog_id: "authoring-loop-catalog".to_string(),
                catalog_version: 1,
                environment: "dev".to_string(),
                suite_flow_version: 1,
                bundle: bundle()?,
            },
            &flowrunner_bytes,
        )
        .await?
        .map_err(anyhow::Error::new)?;

    let success_args = args(&app_url, &flowrunner, "success");
    let report = finalized(
        backend
            .execute_validated_draft(&success_args, &pin, "report-success")
            .await?,
    );
    assert!(report.passed);
    assert_eq!(report.refusal, None);
    assert_eq!(report.cases.len(), 1);
    assert_eq!(report.cases[0].case_id, CASE_ID);
    assert_eq!(report.cases[0].run_id, "scenario-success-0");
    assert_eq!(report.cases[0].status, RunStatus::Completed);
    assert!(report.cases[0].passed);
    let (draft_trigger, draft_context) = run_admission(&admin, "scenario-success-0").await?;
    assert_eq!(draft_trigger, "scenario-draft");
    assert_eq!(draft_context["source"]["producer"], "draft-scenario");
    let draft_principal = &draft_context["principal"];
    assert_eq!(draft_principal["draft-id"], pin.draft_id);
    assert_eq!(draft_principal["draft-revision"], pin.draft_revision);
    assert_eq!(draft_principal["artifact-digest"], pin.draft_artifact_hash);
    assert_eq!(
        draft_principal["validated-draft-hash"],
        pin.validated_draft_hash
    );
    assert_eq!(
        draft_principal["execution-bundle-hash"],
        pin.execution_bundle_hash
    );
    assert_eq!(
        report.lineage,
        ExecutionLineage::Draft {
            draft_artifact_hash: pin.draft_artifact_hash.clone(),
            runtime_flow_version: pin.runtime_flow_version,
            execution_bundle_hash: pin.execution_bundle_hash.clone(),
            validated_draft_hash: pin.validated_draft_hash.clone(),
            catalog_id: pin.catalog_id.clone(),
            catalog_version: pin.catalog_version,
            environment: pin.environment.clone(),
        }
    );
    let admitted_at: SystemTime = admin
        .query_one(
            "SELECT created_at FROM authoring_loop_case_0.runs \
             WHERE tenant_id=$1 AND run_id='scenario-success-0'",
            &[&TENANT],
        )
        .await?
        .get(0);
    let expected_latency = u64::try_from(admitted_at.duration_since(edited_at)?.as_millis())?;
    assert_eq!(report.edit_to_run_ms, Some(expected_latency));
    assert_eq!(
        backend
            .authoring_report(&AuthoringReportQuery {
                tenant_id: TENANT.to_string(),
                report_id: "report-success".to_string(),
            })
            .await?,
        AuthoringReportState::Finalized(report.clone())
    );

    let run_count_after_success = run_count(&admin).await?;
    let retry = finalized(
        backend
            .execute_validated_draft(&success_args, &pin, "report-success")
            .await?,
    );
    assert_eq!(retry, report, "exact retry returns the immutable report");
    assert_eq!(run_count(&admin).await?, run_count_after_success);

    let release_args = args(&app_url, &flowrunner, "release");
    let release_report = finalized(
        backend
            .execute_released_with_report(&release_args, "report-release")
            .await?,
    );
    assert!(
        !release_report.passed,
        "v1 release must fail the v2-only output assertion"
    );
    assert_eq!(release_report.refusal, None);
    assert_eq!(release_report.cases.len(), 1);
    assert_eq!(release_report.cases[0].status, RunStatus::Completed);
    assert!(!release_report.cases[0].passed);
    assert_eq!(release_report.edit_to_run_ms, None);
    assert_eq!(
        release_report.lineage,
        ExecutionLineage::Release {
            artifact_hash: release_artifact_hash,
            catalog_id: "authoring-loop-catalog".to_string(),
            catalog_version: 1,
            environment: "dev".to_string(),
        }
    );
    let (release_trigger, release_context) = run_admission(&admin, "scenario-release-0").await?;
    assert_eq!(release_trigger, "scenario");
    assert_eq!(release_context["source"]["producer"], "scenario");
    let release_principal = &release_context["principal"];
    for draft_only in [
        "draft-id",
        "draft-revision",
        "validated-draft-hash",
        "execution-bundle-hash",
        "binding-base-artifact-hash",
        "suite-flow-version",
    ] {
        assert!(
            release_principal.get(draft_only).is_none(),
            "release principal leaked draft field {draft_only}"
        );
    }
    assert_eq!(
        backend
            .authoring_report(&AuthoringReportQuery {
                tenant_id: TENANT.to_string(),
                report_id: "report-release".to_string(),
            })
            .await?,
        AuthoringReportState::Finalized(release_report)
    );
    let run_count_before_refusals = run_count(&admin).await?;
    assert_eq!(run_count_before_refusals, run_count_after_success + 1);

    admin
        .batch_execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,name,state,base_version) \
             VALUES ('authoring-loop-tenant','authoring-loop-catalog',2,'dev','0.1','drift','staged',1); \
             UPDATE catalog.catalog_heads SET applied_catalog_version=2 \
             WHERE tenant_id='authoring-loop-tenant' \
               AND catalog_id='authoring-loop-catalog' AND environment='dev';",
        )
        .await?;
    let drift_report = finalized(
        backend
            .execute_validated_draft(&args(&app_url, &flowrunner, "drift"), &pin, "report-drift")
            .await?,
    );
    assert_eq!(
        drift_report.refusal,
        Some(ScenarioRefusal::ValidatedDraftDrift)
    );
    assert!(drift_report.cases.is_empty());
    assert_eq!(drift_report.edit_to_run_ms, None);
    assert_eq!(run_count(&admin).await?, run_count_before_refusals);
    admin
        .execute(
            "UPDATE catalog.catalog_heads SET applied_catalog_version=1 \
             WHERE tenant_id=$1 AND catalog_id='authoring-loop-catalog' AND environment='dev'",
            &[&TENANT],
        )
        .await?;

    admin
        .batch_execute(
            "CREATE FUNCTION authoring_loop_source.interrupt_authoring_fact() \
             RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
               RAISE EXCEPTION USING ERRCODE='40000', MESSAGE='authoring-capture-interrupted'; \
             END $$; \
             CREATE TRIGGER aaa_authoring_loop_interrupt \
             BEFORE INSERT ON authoring_loop_source.authoring_suite_case_facts \
             FOR EACH ROW EXECUTE FUNCTION authoring_loop_source.interrupt_authoring_fact();",
        )
        .await?;
    let interrupted_args = args(&app_url, &flowrunner, "interrupted");
    let interruption = backend
        .execute_validated_draft(&interrupted_args, &pin, "report-interrupted")
        .await
        .expect_err("fact-capture fault must interrupt finalization");
    assert!(
        format!("{interruption:#}").contains("authoring-capture-interrupted"),
        "unexpected interruption: {interruption:#}"
    );
    admin
        .batch_execute(
            "DROP TRIGGER aaa_authoring_loop_interrupt \
               ON authoring_loop_source.authoring_suite_case_facts; \
             DROP FUNCTION authoring_loop_source.interrupt_authoring_fact();",
        )
        .await?;
    let interrupted_run_count = run_count(&admin).await?;
    assert_eq!(interrupted_run_count, run_count_before_refusals + 1);
    let interrupted = match backend
        .authoring_report(&AuthoringReportQuery {
            tenant_id: TENANT.to_string(),
            report_id: "report-interrupted".to_string(),
        })
        .await?
    {
        AuthoringReportState::Pending(report) => report,
        state => anyhow::bail!("expected capture-interrupted pending report, got {state:?}"),
    };
    assert_eq!(
        interrupted.reason,
        PendingAuthoringReportReason::CaptureInterrupted {
            run_ids: vec!["scenario-interrupted-0".to_string()]
        }
    );
    assert!(interrupted.captured_cases.is_empty());
    let retry_pending = backend
        .execute_validated_draft(&interrupted_args, &pin, "report-interrupted")
        .await?;
    assert_eq!(
        retry_pending,
        AuthoringExecutionResult::Pending(interrupted)
    );
    assert_eq!(run_count(&admin).await?, interrupted_run_count);

    assert_eq!(release_counts(&admin).await?, baseline_release_counts);

    drop(backend);
    admin
        .batch_execute(
            "DROP SCHEMA authoring_loop_case_0 CASCADE; \
             DROP SCHEMA authoring_loop_source CASCADE; \
             DROP SCHEMA catalog CASCADE; \
             DROP ROLE wamn_authoring_loop_author; \
             DROP ROLE wamn_scenario_author; \
             DROP ROLE wamn_app;",
        )
        .await
        .context("clean authoring-loop schemas and roles")?;
    admin_task.abort();
    Ok(())
}
