//! Real-PostgreSQL acceptance gate for flow-draft save and validation, against
//! the CONTROL database (wamn-0h0g.8.18).
//!
//! The ignored recipe in `docs/archive/build-and-test.md` supplies one disposable
//! database and the flowrunner component built from this checkout. The author
//! credential is no longer a separate input: the scoped A/B generation role is
//! derived from the fixed `(org, project, environment)` plus the database the
//! admin URL names, so the gate mints exactly the identity the credential
//! contract requires and a hand-written role can no longer drift from it.

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};

use wamn_catalog::Artifact;
use wamn_control_provision::{
    CONTROL_PORTABLE_STORE_SQL, CredentialGeneration, SYSTEM_SCHEMA_SQL,
    control_author_generation_role,
};
use wamn_scenario_worker::authoring::{
    ControlAuthoringScope, InternalAuthoringBackend, SaveFlowDraft, SaveFlowDraftResult,
    ValidateFlowDraft,
};

const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");

const TENANT: &str = "authoring-loop-tenant";
/// Fixed by the control store: residency, not a renamed schema, distinguishes it.
const SOURCE_SCHEMA: &str = "wamn_run";
const ORG: &str = "acme";
const PROJECT: &str = "receiving";
const ENVIRONMENT: &str = "dev";
const FLOW_ID: &str = "authoring-loop-flow";
const DRAFT_ID: &str = "authoring-loop-draft";
const AUTHOR_PASSWORD: &str = "wamn-authoring-loop-live";

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

/// The database the admin URL names — half of the author generation's scope.
fn database_of(url: &str) -> anyhow::Result<String> {
    let parsed = url::Url::parse(url).context("admin URL is not a URL")?;
    let database = parsed.path().trim_start_matches('/').to_owned();
    anyhow::ensure!(!database.is_empty(), "admin URL names no database");
    Ok(database)
}

/// Rewrite one connection URL's identity, keeping host, port, and database.
///
/// The gate has to AUTHENTICATE as the author, not `SET ROLE` to it: the tenant
/// mapping resolves on `session_user`.
fn as_role(url: &str, role: &str, password: &str) -> anyhow::Result<String> {
    let mut parsed = url::Url::parse(url).context("admin URL is not a URL")?;
    parsed
        .set_username(role)
        .map_err(|()| anyhow::anyhow!("admin URL carries no username"))?;
    parsed
        .set_password(Some(password))
        .map_err(|()| anyhow::anyhow!("admin URL carries no password"))?;
    Ok(parsed.to_string())
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

async fn reset_and_provision(
    admin: &mut Client,
    database: &str,
    author_role: &str,
) -> anyhow::Result<String> {
    admin
        .batch_execute(&format!(
            "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') \
             THEN CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; END $$; \
             {prepare_generation} \
             DO $$ BEGIN EXECUTE format( \
               'GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;",
            // Mints the stable NOLOGIN ACL role AND this scope's generation, using
            // exactly the text ctl applies.
            prepare_generation = wamn_control_provision::sql::prepare_control_author_generation_sql(
                database,
                author_role,
                AUTHOR_PASSWORD,
                "2099-01-01T00:00:00Z",
            ),
        ))
        .await
        .context("reset authoring-loop schemas and mint the control-author generation")?;
    // Exactly the fresh-control bootstrap record, applied as its documented owner.
    admin
        .batch_execute(&format!(
            "SET ROLE wamn_system;\n{SYSTEM_SCHEMA_SQL}\n{CONTROL_PORTABLE_STORE_SQL}\nRESET ROLE;"
        ))
        .await
        .context("provision the control system schema and portable store")?;
    // The owner-maintained login-identity-to-tenant mapping is the authority the
    // restrictive policies resolve through; without it the author reads nothing.
    admin
        .execute(
            "INSERT INTO wamn_authority.author_login_tenants \
               (login_identity, tenant_id, org_id, project_id, environment) \
             VALUES ($1, $2, $3, $4, $5)",
            &[&author_role, &TENANT, &ORG, &PROJECT, &ENVIRONMENT],
        )
        .await
        .context("map the control-author login to its one tenant")?;
    admin
        .batch_execute(&format!(
            "SELECT set_config('app.tenant', '{TENANT}', false);"
        ))
        .await
        .context("scope the seeding session")?;

    let (release_flow, release_artifact) = release_artifact()?;
    let graph_json = release_flow.to_json();
    let artifact_hash = release_artifact
        .identity()
        .artifact_hash()
        .as_str()
        .to_string();
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
               (tenant_id,catalog_id,catalog_version) \
             VALUES ($1,'authoring-loop-catalog',1)",
            &[&TENANT],
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
    let flowrunner = env("WAMN_AUTHORING_LOOP_FLOWRUNNER")?;
    let flowrunner_bytes = std::fs::read(&flowrunner)
        .with_context(|| format!("read compiled flowrunner {flowrunner}"))?;
    anyhow::ensure!(!flowrunner_bytes.is_empty(), "compiled flowrunner is empty");

    // The credential contract, not an operator, names the author identity.
    let database = database_of(&admin_url)?;
    let author_role = control_author_generation_role(
        ORG,
        PROJECT,
        ENVIRONMENT,
        &database,
        CredentialGeneration::A,
    );
    let author_url = as_role(&admin_url, &author_role, AUTHOR_PASSWORD)?;

    let (mut admin, admin_task) = connect(&admin_url).await?;
    let release_artifact_hash = reset_and_provision(&mut admin, &database, &author_role).await?;
    let baseline_release_counts = release_counts(&admin).await?;
    assert_eq!(baseline_release_counts, (1, 1, 1));

    let scope = ControlAuthoringScope {
        org: ORG.to_string(),
        project: PROJECT.to_string(),
        environment: ENVIRONMENT.to_string(),
        tenant_id: TENANT.to_string(),
        source_schema: SOURCE_SCHEMA.to_string(),
    };
    // An out-of-scope input refuses before any I/O: the same URL under a different
    // environment derives a different generation name and is not admitted.
    let wrong_environment = ControlAuthoringScope {
        environment: "prod".to_string(),
        ..scope.clone()
    };
    assert!(
        InternalAuthoringBackend::connect(
            &author_url,
            &wrong_environment,
            flowrunner_bytes.clone(),
        )
        .await
        .is_err(),
        "an out-of-scope authoring connection input was admitted"
    );
    let mut backend =
        InternalAuthoringBackend::connect(&author_url, &scope, flowrunner_bytes.clone()).await?;
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

    // The flowrunner bytes are the ones the process loaded, not a call argument
    // (wamn-0h0g.15.50), so the trusted runtime revision cannot be chosen here.
    let pin = backend
        .validate_flow_draft(&ValidateFlowDraft {
            tenant_id: TENANT.to_string(),
            draft_id: DRAFT_ID.to_string(),
            draft_revision: revision,
            catalog_id: "authoring-loop-catalog".to_string(),
            catalog_version: 1,
            environment: ENVIRONMENT.to_string(),
        })
        .await?
        .map_err(anyhow::Error::new)?;
    // The persisted plan pins the digest of the bytes THIS PROCESS loaded, so the
    // in-image component is genuinely the minting pod's flowrunner source.
    let persisted_plan: serde_json::Value = serde_json::from_slice(
        &admin
            .query_one(
                "SELECT exact_bytes FROM catalog.execution_bundles \
                  WHERE tenant_id = $1 AND execution_bundle_hash = $2",
                &[&TENANT, &pin.execution_bundle_hash],
            )
            .await?
            .get::<_, Vec<u8>>(0),
    )?;
    assert_eq!(
        persisted_plan["header"]["runtime-revision"]["flowrunner-component-digest"]
            .as_str()
            .context("the persisted plan pins a flowrunner digest")?,
        wamn_execution_host::TrustedExecutionRuntimeRevision::from_flowrunner_bytes(
            &flowrunner_bytes
        )
        .flowrunner_component_digest(),
        "the persisted pin names bytes this process did not load"
    );

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
    // Retirement returns the generation slot to inert; slots are reused, never
    // dropped, so the role survives with no authority and no authentication.
    admin
        .batch_execute(
            &wamn_control_provision::sql::retire_control_author_generation_sql(
                &database,
                &author_role,
            ),
        )
        .await
        .context("retire the authoring-loop control-author generation")?;
    admin
        .batch_execute(
            "DROP SCHEMA catalog CASCADE; \
             DROP SCHEMA wamn_run CASCADE; \
             DROP SCHEMA wamn_authority CASCADE;",
        )
        .await
        .context("clean the authoring-loop control store")?;
    admin_task.abort();
    Ok(())
}
