//! Real-PostgreSQL acceptance gate for flow-draft save and validation.
//!
//! The ignored recipe in `docs/archive/build-and-test.md` supplies one disposable
//! database, a dedicated author credential, and the flowrunner component built
//! from this checkout.

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};

use wamn_catalog::Artifact;
use wamn_scenario_worker::authoring::{
    InternalAuthoringBackend, SaveFlowDraft, SaveFlowDraftResult, ValidateFlowDraft,
};
use wamn_schema_control::{BareSchemaName, rewrite_schema};

const TENANT: &str = "authoring-loop-tenant";
const SOURCE_SCHEMA: &str = "authoring_loop_source";
const FLOW_ID: &str = "authoring-loop-flow";
const DRAFT_ID: &str = "authoring-loop-draft";

const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../deploy/sql/flows.sql");
const FLOW_TESTS_SQL: &str = include_str!("../../../deploy/sql/flow-tests.sql");

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

fn schema_sql(record: &str) -> String {
    let schema = BareSchemaName::new(SOURCE_SCHEMA).expect("gate schema is a valid identifier");
    rewrite_schema(record, &schema)
}

fn release_artifact() -> anyhow::Result<(wamn_flow::Flow, Artifact)> {
    let flow = wamn_flow::Flow::from_json(RELEASE_GRAPH)?;
    let implementations = ["request", "respond"]
        .into_iter()
        .map(|node_type| {
            let interface = wamn_standard_nodes::describe_interface(node_type)
                .with_context(|| format!("resolve standard node {node_type}"))?;
            Ok(interface.clone())
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let artifact = Artifact::new(TENANT, &flow, implementations)?;
    Ok((flow, artifact))
}

async fn reset_and_provision(admin: &mut Client) -> anyhow::Result<String> {
    admin
        .batch_execute(
            "DROP SCHEMA IF EXISTS authoring_loop_source CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP ROLE IF EXISTS wamn_authoring_loop_author; \
             DROP ROLE IF EXISTS wamn_scenario_author; \
             DROP ROLE IF EXISTS wamn_app; \
             CREATE ROLE wamn_app NOLOGIN \
               NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_scenario_author NOLOGIN \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
             CREATE ROLE wamn_authoring_loop_author LOGIN PASSWORD 'wamn-author-live' \
               NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
             GRANT wamn_scenario_author TO wamn_authoring_loop_author;",
        )
        .await
        .context("reset authoring-loop schema and roles")?;
    admin
        .batch_execute(CATALOG_SQL)
        .await
        .context("provision catalog schema")?;
    for record in [RUN_STATE_SQL, FLOWS_SQL, FLOW_TESTS_SQL] {
        admin
            .batch_execute(&schema_sql(record))
            .await
            .context("provision authoring-loop source schema")?;
    }

    let (release_flow, release_artifact) = release_artifact()?;
    let graph_json = release_flow.to_json();
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
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash) \
             VALUES ($1,$2,1,'0.1',$3::text::jsonb,$4,$5)",
            &[
                &TENANT,
                &FLOW_ID,
                &graph_json,
                &release_artifact.graph_hash(),
                &artifact_hash,
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
            "INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             SELECT $1, 'sha256:' || encode(sha256(convert_to('{}','UTF8')), 'hex'), \
                    '0.1', convert_to('{}','UTF8'), 2",
            &[&TENANT],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) \
             SELECT $1,'authoring-loop-catalog',1,$2,1,execution_bundle_hash \
               FROM catalog.execution_bundles WHERE tenant_id=$1",
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
    transaction.commit().await?;
    Ok(artifact_hash)
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the exact disposable-PostgreSQL + compiled-flowrunner recipe"]
async fn authoring_loop_live() -> anyhow::Result<()> {
    let admin_url = env("WAMN_AUTHORING_LOOP_ADMIN_PG_URL")?;
    let author_url = env("WAMN_AUTHORING_LOOP_AUTHOR_PG_URL")?;
    let flowrunner = env("WAMN_AUTHORING_LOOP_FLOWRUNNER")?;
    let flowrunner_bytes = std::fs::read(&flowrunner)
        .with_context(|| format!("read compiled flowrunner {flowrunner}"))?;
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

    let pin = backend
        .validate_flow_draft(
            &ValidateFlowDraft {
                tenant_id: TENANT.to_string(),
                draft_id: DRAFT_ID.to_string(),
                draft_revision: revision,
                catalog_id: "authoring-loop-catalog".to_string(),
                catalog_version: 1,
                environment: "dev".to_string(),
            },
            &flowrunner_bytes,
        )
        .await?
        .map_err(anyhow::Error::new)?;

    assert_eq!(pin.draft_revision, 1);
    assert_eq!(pin.runtime_flow_version, 2);
    assert_eq!(pin.binding_base_artifact_hash, release_artifact_hash);
    let row = admin
        .query_one(
            "SELECT draft_edited_at, execution_bundle_hash, validated_draft_hash \
               FROM catalog.validated_flow_drafts \
              WHERE tenant_id=$1 AND draft_id=$2 AND draft_revision=$3",
            &[&TENANT, &DRAFT_ID, &revision],
        )
        .await?;
    assert_eq!(row.get::<_, std::time::SystemTime>(0), edited_at);
    assert_eq!(row.get::<_, String>(1), pin.execution_bundle_hash);
    assert_eq!(row.get::<_, String>(2), pin.validated_draft_hash);
    assert_eq!(release_counts(&admin).await?, baseline_release_counts);

    drop(backend);
    admin
        .batch_execute(
            "DROP SCHEMA authoring_loop_source CASCADE; \
             DROP SCHEMA catalog CASCADE; \
             DROP ROLE wamn_authoring_loop_author; \
             DROP ROLE wamn_scenario_author; \
             DROP ROLE wamn_app;",
        )
        .await
        .context("clean authoring-loop schema and roles")?;
    admin_task.abort();
    Ok(())
}
