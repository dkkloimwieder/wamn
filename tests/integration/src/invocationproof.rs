//! Exact-image proof for admitted HTTP run execution.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::json;
use tokio_postgres::{Client, Config, NoTls};
use wamn_catalog::{Artifact, NodeImplementation};
use wamn_execution_host::{ExecutionHost, ExecutionIdentity, production_capabilities};
use wamn_flow::Flow;
use wamn_flow_invocation::{BeginResult, InvokeRequest, InvokeResult};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::flow_invocation::{
    InlineRunClaim, InlineRunDriver, InvocationService, InvocationServiceConfig,
    PostgresInvocationBackend,
};
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};

const TENANT: &str = "inline-tenant";
const OWNER: &str = "inline-provider";
const FLOW_ID: &str = "inline-echo";
const CATALOG_ID: &str = "inline-catalog";
const ATTACHMENT_ID: &str = "http-proof";
const DEFINITION_HASH: &str = "sha256:inline-proof";

struct ProofInlineDriver {
    host: Arc<tokio::sync::Mutex<ExecutionHost>>,
}

impl InlineRunDriver for ProofInlineDriver {
    fn start(&self, claim: InlineRunClaim) -> anyhow::Result<()> {
        let host = self.host.clone();
        tokio::spawn(async move {
            host.lock()
                .await
                .execute_claimed(&claim.run_id, &claim.lease_owner, claim.lease_generation)
                .await
                .expect("proof inline execution must complete");
        });
        Ok(())
    }
}

struct ProofPausedDriver;

impl InlineRunDriver for ProofPausedDriver {
    fn start(&self, _claim: InlineRunClaim) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Args)]
pub struct InvocationProofArgs {
    /// Exact flowrunner component baked into the gates image.
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// Application-role PostgreSQL URL used by the production plugin.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// Administrative PostgreSQL URL used to create the throwaway proof DB.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,
}

fn proof_database_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    format!("wamn_invocationproof_{}_{}", std::process::id(), nanos)
}

fn database_url(url: &str, database: &str) -> anyhow::Result<String> {
    let (prefix, tail) = url
        .rsplit_once('/')
        .context("PostgreSQL URL must contain a database path")?;
    let query = tail
        .find('?')
        .map(|index| &tail[index..])
        .unwrap_or_default();
    Ok(format!("{prefix}/{database}{query}"))
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let config: Config = url.parse().context("parse PostgreSQL URL")?;
    let (client, connection) = config.connect(NoTls).await?;
    let handle = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, handle))
}

fn echo_flow() -> anyhow::Result<Flow> {
    Flow::from_json(
        r#"{
          "schema-version":"0.1",
          "flow-id":"inline-echo",
          "version":1,
          "nodes":[
            {"id":"request","type":"request","config":{"input-schema":true}},
            {"id":"respond","type":"respond","config":{"status":200}}
          ],
          "edges":[{"from":"request","to":"respond"}]
        }"#,
    )
    .map_err(|error| anyhow::anyhow!("parse exact-run fixture: {error}"))
}

fn echo_implementations() -> anyhow::Result<Vec<NodeImplementation>> {
    ["request", "respond"]
        .into_iter()
        .map(|node_type| {
            let descriptor = wamn_standard_nodes::describe(node_type)
                .with_context(|| format!("missing standard-node descriptor for {node_type}"))?;
            let contract =
                wamn_standard_nodes::resolve_descriptor(descriptor).map_err(anyhow::Error::new)?;
            NodeImplementation::from_resolved_platform_contract(contract)
                .map_err(anyhow::Error::new)
        })
        .collect()
}

async fn create_database(admin_url: &str, name: &str) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url).await?;
    client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .await?;
    drop(client);
    let _ = handle.await;
    Ok(())
}

async fn drop_database(admin_url: &str, name: &str) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url).await?;
    let result = client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .await;
    drop(client);
    let _ = handle.await;
    result.context("drop invocationproof database")
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (mut client, handle) = connect(admin_url).await?;
    client
        .batch_execute(concat!(
            include_str!("../../../deploy/sql/catalog-schema.sql"),
            "\n",
            include_str!("../../../deploy/sql/run-state.sql"),
            "\n",
            include_str!("../../../deploy/sql/run-queue.sql")
        ))
        .await
        .context("apply catalog and run-plane DDL")?;

    let flow = echo_flow()?;
    let artifact = Artifact::new(TENANT, &flow, echo_implementations()?)?;
    let graph = String::from_utf8(flow.canonical_bytes()).expect("canonical graph is UTF-8");
    let interfaces = String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec())
        .expect("canonical interfaces are UTF-8");
    let components = serde_json::to_value(artifact.supplied_components())?;
    let occurrence_recovery = String::from_utf8(artifact.occurrence_recovery_bytes().to_vec())
        .expect("canonical occurrence recovery selections are UTF-8");
    let artifact_hash = artifact.identity().artifact_hash().as_str();
    let members = json!([{
        "flow-id": FLOW_ID,
        "flow-version": 1,
        "artifact-hash": artifact_hash
    }]);

    client
        .execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state,document) \
             VALUES ($1,$2,1,'proof','0.1','applied','{}')",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    client
        .execute(
            "INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                artifact_hash,interface_bundle_json,interface_bundle_hash,component_digests, \
                occurrence_recovery_json,occurrence_recovery_hash) \
             VALUES ($1,$2,1,'0.1',$3::text::jsonb,$4,$5,$6,$7,$8,$9,$10)",
            &[
                &TENANT,
                &FLOW_ID,
                &graph,
                &artifact.graph_hash(),
                &artifact_hash,
                &interfaces,
                &artifact.interface_bundle().hash(),
                &components,
                &occurrence_recovery,
                &artifact.occurrence_recovery_hash(),
            ],
        )
        .await?;
    let release = client.transaction().await?;
    release
        .execute(
            "INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version,members_json) VALUES ($1,$2,1,$3)",
            &[&TENANT, &CATALOG_ID, &members],
        )
        .await?;
    release
        .execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
             VALUES ($1,$2,1,$3,1)",
            &[&TENANT, &CATALOG_ID, &FLOW_ID],
        )
        .await?;
    release
        .execute(
            "INSERT INTO catalog.release_exposure_manifests \
               (tenant_id,catalog_id,catalog_version,definitions_json) \
             VALUES ($1,$2,1,'{}')",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    release
        .execute(
            "INSERT INTO catalog.release_sources \
               (tenant_id,catalog_id,catalog_version,source_id,source_kind,definition_json,source_hash) \
             VALUES ($1,$2,1,'auth-proof','auth','{}','sha256:auth-proof')",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    release
        .execute(
            "INSERT INTO catalog.release_attachments \
               (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id,source_id, \
                definition_hash,definition_json,route_host,route_path,route_template,route_method) \
             VALUES ($1,$2,1,$3,'http',$4,'auth-proof',$5, \
                     '{\"run-deadline-ms\":60000,\"response-deadline-ms\":30000}', \
                     'proof.test','/echo','/echo','POST')",
            &[
                &TENANT,
                &CATALOG_ID,
                &ATTACHMENT_ID,
                &FLOW_ID,
                &DEFINITION_HASH,
            ],
        )
        .await?;
    release
        .execute(
            "INSERT INTO catalog.catalog_heads \
               (tenant_id,catalog_id,environment,applied_catalog_version) \
             VALUES ($1,$2,'proof',1)",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    release
        .execute(
            "INSERT INTO catalog.attachment_activation \
               (tenant_id,catalog_id,environment,attachment_id,confirmed_definition_hash,enabled) \
             VALUES ($1,$2,'proof',$3,$4,true)",
            &[&TENANT, &CATALOG_ID, &ATTACHMENT_ID, &DEFINITION_HASH],
        )
        .await?;
    release.commit().await?;
    drop(client);
    let _ = handle.await;
    Ok(())
}

async fn seed_claimed(
    admin_url: &str,
    run_id: &str,
    generation: i64,
    status: &str,
    expired: bool,
) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url).await?;
    let expiry = if expired {
        "now() - interval '1 second'"
    } else {
        "now() + interval '30 seconds'"
    };
    client
        .batch_execute(&format!(
            "INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                attachment_id,status,trigger_source,input_json,response_deadline_at,run_deadline_at) \
             VALUES ('{TENANT}','{run_id}','{FLOW_ID}',1,'{CATALOG_ID}',1, \
                     'http-proof','{status}','http','{{\"echo\":\"ok\"}}', \
                     now()+interval '30 seconds',now()+interval '1 minute'); \
             INSERT INTO wamn_run.run_queue \
               (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
             VALUES ('{TENANT}','{run_id}','{OWNER}',{expiry},{generation});"
        ))
        .await?;
    drop(client);
    let _ = handle.await;
    Ok(())
}

async fn assert_completed(admin_url: &str, run_id: &str) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url).await?;
    let row = client
        .query_one(
            "SELECT status, caller_outcome_kind, caller_http_status, \
                    caller_outcome_json, caller_outcome_hash, \
                    NOT EXISTS (SELECT FROM wamn_run.run_queue q \
                                 WHERE q.tenant_id=r.tenant_id AND q.run_id=r.run_id) \
               FROM wamn_run.runs r WHERE tenant_id=$1 AND run_id=$2",
            &[&TENANT, &run_id],
        )
        .await?;
    let passed = row.get::<_, String>(0) == "completed"
        && row.get::<_, Option<String>>(1).as_deref() == Some("responded")
        && row.get::<_, Option<i32>>(2) == Some(200)
        && row.get::<_, Option<serde_json::Value>>(3) == Some(json!({"echo": "ok"}))
        && row
            .get::<_, Option<String>>(4)
            .is_some_and(|hash| hash.starts_with("sha256:"))
        && row.get::<_, bool>(5);
    drop(client);
    let _ = handle.await;
    if !passed {
        bail!("run {run_id} did not store one terminal response and dequeue");
    }
    Ok(())
}

async fn promote_attachment_definition(
    admin_url: &str,
    definition_hash: &str,
) -> anyhow::Result<()> {
    let (mut client, handle) = connect(admin_url).await?;
    let transaction = client.transaction().await?;
    transaction
        .execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state,document) \
             VALUES ($1,$2,2,'proof','0.1','staged','{}')",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    for statement in [
        "INSERT INTO catalog.release_manifests \
           SELECT tenant_id,catalog_id,2,members_json \
           FROM catalog.release_manifests \
           WHERE tenant_id=$1 AND catalog_id=$2 AND catalog_version=1",
        "INSERT INTO catalog.release_flows \
           SELECT tenant_id,catalog_id,2,flow_id,flow_version \
           FROM catalog.release_flows \
           WHERE tenant_id=$1 AND catalog_id=$2 AND catalog_version=1",
        "INSERT INTO catalog.release_exposure_manifests \
           SELECT tenant_id,catalog_id,2,definitions_json \
           FROM catalog.release_exposure_manifests \
           WHERE tenant_id=$1 AND catalog_id=$2 AND catalog_version=1",
        "INSERT INTO catalog.release_sources \
           SELECT tenant_id,catalog_id,2,source_id,source_kind,definition_json,source_hash \
           FROM catalog.release_sources \
           WHERE tenant_id=$1 AND catalog_id=$2 AND catalog_version=1",
    ] {
        transaction
            .execute(statement, &[&TENANT, &CATALOG_ID])
            .await?;
    }
    transaction
        .execute(
            "INSERT INTO catalog.release_attachments \
               (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id,source_id, \
                definition_hash,definition_json,route_host,route_path,route_template,route_method) \
             SELECT tenant_id,catalog_id,2,attachment_id,attachment_kind,flow_id,source_id, \
                    $3,definition_json,route_host,route_path,route_template,route_method \
             FROM catalog.release_attachments \
             WHERE tenant_id=$1 AND catalog_id=$2 AND catalog_version=1",
            &[&TENANT, &CATALOG_ID, &definition_hash],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.catalogs SET state='superseded' \
             WHERE tenant_id=$1 AND catalog_id=$2 AND environment='proof' \
               AND version=1 AND state='applied'",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.catalogs SET state='applied' \
             WHERE tenant_id=$1 AND catalog_id=$2 AND environment='proof' \
               AND version=2 AND state='staged'",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.catalog_heads SET applied_catalog_version=2 \
             WHERE tenant_id=$1 AND catalog_id=$2 AND environment='proof'",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.attachment_activation \
             SET confirmed_definition_hash=$3, enabled=true \
             WHERE tenant_id=$1 AND catalog_id=$2 AND environment='proof' \
               AND attachment_id='http-proof'",
            &[&TENANT, &CATALOG_ID, &definition_hash],
        )
        .await?;
    transaction.commit().await?;
    drop(client);
    let _ = handle.await;
    Ok(())
}

pub async fn run(args: InvocationProofArgs) -> anyhow::Result<()> {
    let name = proof_database_name();
    create_database(&args.admin_database_url, &name).await?;
    let admin_url = database_url(&args.admin_database_url, &name)?;
    let app_url = database_url(&args.database_url, &name)?;
    let result = async {
        provision(&admin_url).await?;
        seed_claimed(&admin_url, "exact-positive", 1, "dispatched", false).await?;
        seed_claimed(&admin_url, "exact-fence", 3, "dispatched", false).await?;
        seed_claimed(&admin_url, "exact-recovery", 4, "running", true).await?;

        let guest = std::fs::read(&args.flowrunner)
            .with_context(|| format!("read {}", args.flowrunner.display()))?;
        let engine = build_engine(&[])?;
        let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
        let mut pg_config = WamnPostgresConfig::from_env();
        pg_config.database_url = Some(app_url.clone());
        let postgres = Arc::new(WamnPostgres::new(pg_config)?);
        let credentials = Arc::new(WamnCredentials::empty());
        let logging = Arc::new(WamnLogging::from_env()?);

        // Instantiation fault after admission leaves the durable claim untouched.
        assert!(
            ExecutionHost::instantiate(
                &engine,
                &guest[..guest.len() / 2],
                postgres.clone(),
                credentials.clone(),
                logging.clone(),
                ExecutionIdentity {
                    owner: OWNER,
                    tenant: TENANT,
                    schema: Some("wamn_run"),
                    project: "proof",
                },
                production_capabilities(Arc::from([]), Arc::new(RunnerEgressPolicy::default())),
                30_000,
            )
            .await
            .is_err()
        );

        let mut host = ExecutionHost::instantiate(
            &engine,
            &guest,
            postgres,
            credentials,
            logging,
            ExecutionIdentity {
                owner: OWNER,
                tenant: TENANT,
                schema: Some("wamn_run"),
                project: "proof",
            },
            production_capabilities(Arc::from([]), Arc::new(RunnerEgressPolicy::default())),
            30_000,
        )
        .await?;

        assert_eq!(host.execute_claimed("exact-positive", OWNER, 1).await?, 0);
        assert_completed(&admin_url, "exact-positive").await?;

        let stale = host
            .execute_claimed("exact-fence", OWNER, 2)
            .await
            .expect_err("stale generation must refuse");
        assert!(stale.to_string().contains("fence-lost"));
        assert_eq!(host.execute_claimed("exact-fence", OWNER, 3).await?, 0);
        assert_completed(&admin_url, "exact-fence").await?;

        assert_eq!(host.execute_claimed("exact-recovery", OWNER, 4).await?, 0);
        assert_completed(&admin_url, "exact-recovery").await?;
        let observed_after_completion = host
            .execute_claimed("exact-recovery", OWNER, 4)
            .await
            .expect_err("completed run has no claimed queue row");
        assert!(observed_after_completion.to_string().contains("not-found"));

        let host = Arc::new(tokio::sync::Mutex::new(host));
        let service = InvocationService::new(
            PostgresInvocationBackend::from_database_url(&app_url)?,
            Some(app_url.clone()),
            InvocationServiceConfig {
                tenant_id: TENANT.to_string(),
                catalog_id: CATALOG_ID.to_string(),
                environment: "proof".to_string(),
                project: "proof".to_string(),
                schema: Some("wamn_run".to_string()),
                executor_id: OWNER.to_string(),
                platform_revision: "invocationproof".to_string(),
                lease_ttl: std::time::Duration::from_secs(30),
                admission_ttl: std::time::Duration::from_secs(60),
            },
            Arc::new(ProofInlineDriver { host }),
        );
        let request = InvokeRequest {
            attachment_id: ATTACHMENT_ID.to_string(),
            expected_catalog_version: 1,
            expected_definition_hash: DEFINITION_HASH.to_string(),
            client_request_fingerprint: "sha256:provider-request".to_string(),
            payload: r#"{"echo":"provider"}"#.to_string(),
            idempotency_key: Some("provider-key".to_string()),
            principal: "proof-principal".to_string(),
            deadline_override: None,
            trace: None,
        };
        let BeginResult::Admitted(admitted) = service.begin(request.clone()).await? else {
            bail!("provider refused the valid proof request");
        };
        let Some(InvokeResult::Responded(response)) =
            service.wait(admitted.run_id.clone(), 5_000).await?
        else {
            bail!("provider did not return the exact stored response");
        };
        if response.body != r#"{"echo":"provider"}"# || response.status_hint != Some(200) {
            bail!("provider changed the stored response");
        }
        let BeginResult::Admitted(recovered) = service.begin(request.clone()).await? else {
            bail!("provider did not recover the completed admission");
        };
        if recovered.run_id != admitted.run_id {
            bail!("provider recovery changed the durable run id");
        }
        let mut conflicting = request.clone();
        conflicting.client_request_fingerprint = "sha256:different-body".to_string();
        let BeginResult::Rejected(rejection) = service.begin(conflicting).await? else {
            bail!("provider accepted a conflicting idempotency body");
        };
        if rejection.code != "idempotency-key-reused" {
            bail!("provider collapsed the idempotency conflict");
        }

        let (client, connection) = connect(&admin_url).await?;
        client
            .execute(
                "UPDATE catalog.attachment_activation SET enabled=false \
                  WHERE tenant_id=$1 AND catalog_id=$2 AND environment='proof' AND attachment_id=$3",
                &[&TENANT, &CATALOG_ID, &ATTACHMENT_ID],
            )
            .await?;
        drop(client);
        let _ = connection.await;
        let BeginResult::Admitted(disabled_recovery) = service.begin(request.clone()).await? else {
            bail!("disabled attachment did not recover its stored admission");
        };
        if disabled_recovery.run_id != admitted.run_id {
            bail!("disabled recovery changed the durable run id");
        }
        let mut fresh_disabled = request.clone();
        fresh_disabled.idempotency_key = Some("provider-key-disabled".to_string());
        let BeginResult::Rejected(rejection) = service.begin(fresh_disabled).await? else {
            bail!("disabled attachment admitted a new run");
        };
        if rejection.code != "attachment-disabled" {
            bail!("disabled new admission returned the wrong refusal");
        }

        let changed_definition_hash = "sha256:inline-proof-edited";
        promote_attachment_definition(&admin_url, changed_definition_hash).await?;
        let BeginResult::Rejected(rejection) = service.begin(request.clone()).await? else {
            bail!("provider recovered a key across attachment-definition drift");
        };
        if rejection.code != "idempotency-scope-changed" {
            bail!("provider collapsed attachment-definition drift");
        }

        let paused_service = InvocationService::new(
            PostgresInvocationBackend::from_database_url(&app_url)?,
            Some(app_url.clone()),
            InvocationServiceConfig {
                tenant_id: TENANT.to_string(),
                catalog_id: CATALOG_ID.to_string(),
                environment: "proof".to_string(),
                project: "proof".to_string(),
                schema: Some("wamn_run".to_string()),
                executor_id: OWNER.to_string(),
                platform_revision: "invocationproof".to_string(),
                lease_ttl: std::time::Duration::from_secs(30),
                admission_ttl: std::time::Duration::from_secs(60),
            },
            Arc::new(ProofPausedDriver),
        );
        let mut in_flight = request;
        in_flight.expected_catalog_version = 2;
        in_flight.expected_definition_hash = changed_definition_hash.to_string();
        in_flight.idempotency_key = Some("provider-key-in-flight".to_string());
        let BeginResult::Admitted(_) = paused_service.begin(in_flight.clone()).await? else {
            bail!("provider refused the in-flight fixture");
        };
        let BeginResult::Rejected(rejection) = paused_service.begin(in_flight).await? else {
            bail!("provider admitted a duplicate while the first run was in flight");
        };
        if rejection.code != "in-flight" {
            bail!("provider returned the wrong in-flight duplicate refusal");
        }

        ticker.abort();
        println!(
            "invocationproof PASS: provider admitted, drove, waited, recovered, and enforced the complete idempotency matrix"
        );
        anyhow::Ok(())
    }
    .await;
    let cleanup = drop_database(&args.admin_database_url, &name).await;
    result.and(cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_fixture_is_uniformly_pinned_and_artifact_buildable() {
        let flow = echo_flow().unwrap();
        let artifact = Artifact::new(TENANT, &flow, echo_implementations().unwrap()).unwrap();
        assert_eq!(artifact.interface_bundle().interfaces().len(), 2);
        assert_eq!(artifact.identity().id().flow_id(), FLOW_ID);
    }

    #[test]
    fn database_url_replaces_only_the_database_path() {
        assert_eq!(
            database_url("postgresql://u:p@db:5432/base?sslmode=disable", "proof").unwrap(),
            "postgresql://u:p@db:5432/proof?sslmode=disable"
        );
    }
}
