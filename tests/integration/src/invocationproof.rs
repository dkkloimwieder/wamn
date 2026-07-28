//! Exact-image proof for admitted HTTP run execution.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::json;
use tokio_postgres::{Client, Config, NoTls};
use wamn_catalog::Artifact;
use wamn_execution_host::{ExecutionHost, ExecutionIdentity, production_capabilities};
use wamn_flow::Flow;
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};

const TENANT: &str = "inline-tenant";
const OWNER: &str = "inline-provider";
const FLOW_ID: &str = "inline-echo";
const CATALOG_ID: &str = "inline-catalog";

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
    let artifact = Artifact::new(TENANT, &flow, vec![])?;
    let graph = String::from_utf8(flow.canonical_bytes()).expect("canonical graph is UTF-8");
    let interfaces = String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec())
        .expect("canonical interfaces are UTF-8");
    let components = serde_json::to_value(artifact.supplied_components())?;
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
                artifact_hash,interface_bundle_json,interface_bundle_hash,component_digests) \
             VALUES ($1,$2,1,'0.1',$3::text::jsonb,$4,$5,$6,$7,$8)",
            &[
                &TENANT,
                &FLOW_ID,
                &graph,
                &artifact.graph_hash(),
                &artifact_hash,
                &interfaces,
                &artifact.interface_bundle().hash(),
                &components,
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
        pg_config.database_url = Some(app_url);
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

        ticker.abort();
        println!(
            "invocationproof PASS: exact image drove fresh + recovered claims under one fence"
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
    fn exact_fixture_is_model_owned_and_artifact_buildable() {
        let flow = echo_flow().unwrap();
        let artifact = Artifact::new(TENANT, &flow, vec![]).unwrap();
        assert_eq!(artifact.interface_bundle().interfaces().len(), 0);
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
