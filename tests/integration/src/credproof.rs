//! Live credential proof through production admission and the trusted HTTP adapter.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail, ensure};
use clap::Args;
use serde_json::json;
use tokio_postgres::{Client, Config, NoTls};
use wamn_catalog::Artifact;
use wamn_execution_host::{ExecutionHost, ExecutionIdentity, production_capabilities};
use wamn_flow::Flow;
use wamn_flow_invocation::{BeginResult, InvokeRequest, InvokeResult};
use wamn_node_manifest::ResolvedNodeInterface;
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::flow_invocation::{
    InlineRunClaim, InlineRunDriver, InvocationService, InvocationServiceConfig,
    PostgresInvocationBackend,
};
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_test_fixtures::runner::fnv1a_64;
use wash_runtime::host::allowed_hosts::AllowedHost;

const TENANT: &str = "credproof-tenant";
const PROJECT: &str = "credproof";
const ENVIRONMENT: &str = "proof";
const CATALOG_ID: &str = "credproof-catalog";
const FLOW_ID: &str = "cred-notify";
const DENY_FLOW_ID: &str = "egress-deny";
const ESCAPE_FLOW_ID: &str = "egress-address-escape";
const ATTACHMENT_ID: &str = "credproof-positive";
const DENY_ATTACHMENT_ID: &str = "credproof-deny";
const ESCAPE_ATTACHMENT_ID: &str = "credproof-address-escape";
const DEFINITION_HASH: &str = "sha256:credproof-positive";
const DENY_DEFINITION_HASH: &str = "sha256:credproof-deny";
const ESCAPE_DEFINITION_HASH: &str = "sha256:credproof-address-escape";
const CONNECTION_NAME: &str = "notify-endpoint";
const INSTANCE_ID: &str = "credproof-echo";
const ESCAPE_INSTANCE_ID: &str = "credproof-address-escape";
const CREDENTIAL_HANDLE: &str = "notify-token";
const DEMO_SECRET: &str = "Bearer wamn-cred-proof-7f3a9b2e41d05c68";
const FLOW_JSON: &str = include_str!("../../../deploy/cred/notify.flow.json");
const DENY_FLOW_JSON: &str = include_str!("../../../deploy/cred/deny.flow.json");
// Keep the address-control fixture in this compilation unit so image rebuilds
// cannot retain a stale embedded policy probe when the fixture changes.
const ESCAPE_FLOW_JSON: &str = include_str!("../../../deploy/cred/address-escape.flow.json");

struct ProofDriver {
    host: Arc<tokio::sync::Mutex<ExecutionHost>>,
}

impl InlineRunDriver for ProofDriver {
    fn start(&self, claim: InlineRunClaim) -> anyhow::Result<()> {
        let host = self.host.clone();
        tokio::spawn(async move {
            let result = host
                .lock()
                .await
                .execute_claimed(&claim.run_id, &claim.lease_owner, claim.lease_generation)
                .await;
            if let Err(error) = result {
                tracing::debug!(run_id = %claim.run_id, %error, "credproof run ended with a refusal");
            }
        });
        Ok(())
    }
}

#[derive(Debug, Args)]
pub struct CredProofArgs {
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,
    #[arg(long, default_value = "http://serve-echo:8091")]
    pub echo_url: String,
    #[arg(long, default_value = "http://egress-escape:8091")]
    pub escape_url: String,
    #[arg(long, default_value = DEMO_SECRET)]
    pub secret: String,
    #[arg(long, default_value_t = 60)]
    pub timeout_secs: u64,
}

fn proof_database_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    format!("wamn_credproof_{}_{}", std::process::id(), nanos)
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

async fn create_database(admin_url: &str, name: &str) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url).await?;
    client
        .batch_execute(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await?;
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
    result.context("drop credproof database")
}

fn parse_flow(source: &str) -> anyhow::Result<Flow> {
    Flow::from_json(source).map_err(|error| anyhow::anyhow!("parse credproof flow: {error}"))
}

fn implementations(flow: &Flow) -> anyhow::Result<Vec<ResolvedNodeInterface>> {
    let node_types: BTreeSet<&str> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .collect();
    node_types
        .into_iter()
        .map(|node_type| {
            let descriptor = wamn_standard_nodes::describe(node_type)
                .with_context(|| format!("missing standard-node descriptor for {node_type}"))?;
            let contract =
                wamn_standard_nodes::resolve_descriptor(descriptor).map_err(anyhow::Error::new)?;
            Ok(contract.interface)
        })
        .collect()
}

async fn insert_artifact(client: &Client, flow: &Flow) -> anyhow::Result<Artifact> {
    let artifact = Artifact::new(TENANT, flow, implementations(flow)?)?;
    let graph = String::from_utf8(flow.canonical_bytes()).expect("canonical graph is UTF-8");
    let artifact_hash = artifact.identity().artifact_hash().as_str();
    client
        .execute(
            "INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                artifact_hash) \
             VALUES ($1,$2,1,'0.1',$3::text::jsonb,$4,$5)",
            &[
                &TENANT,
                &flow.flow_id,
                &graph,
                &artifact.graph_hash(),
                &artifact_hash,
            ],
        )
        .await?;

    let named = flow
        .connection_requirements
        .first()
        .context("credproof flow connection requirement")?;
    let requirement_json = serde_json::to_string(&named.requirement)?;
    let requirement_hash = wamn_schema_control::connections::ArtifactConnectionRequirement::new(
        artifact_hash,
        &named.name,
        named.requirement.clone(),
    )
    .requirement_hash();
    client
        .execute(
            wamn_schema_control::connections::insert_connection_requirement_sql(),
            &[
                &TENANT,
                &artifact_hash,
                &named.name,
                &requirement_json,
                &requirement_hash,
            ],
        )
        .await?;
    Ok(artifact)
}

async fn insert_connection(
    client: &Client,
    instance_id: &str,
    authority_url: &str,
    generation_hash: &str,
) -> anyhow::Result<()> {
    client
        .execute(
            wamn_schema_control::connections::insert_connection_instance_sql(),
            &[
                &TENANT,
                &ENVIRONMENT,
                &instance_id,
                &"http",
                &"wamn:connection/http@0.1.0",
            ],
        )
        .await?;
    let primary = format!("{}/", authority_url.trim_end_matches('/'));
    let definition = json!({
        "primary-authority": primary,
        "failover-authorities": [],
        "tls-verification": "disabled",
        "tls-names": [],
        "redirect-policy": "same-authority",
        "proxy-transport": null,
        "credential-set-handle": CREDENTIAL_HANDLE
    });
    let definition_text = serde_json::to_string(&definition)?;
    client
        .execute(
            wamn_schema_control::connections::insert_connection_generation_sql(),
            &[
                &TENANT,
                &ENVIRONMENT,
                &instance_id,
                &1_i64,
                &definition_text,
                &generation_hash,
                &CREDENTIAL_HANDLE,
            ],
        )
        .await?;
    client
        .execute(
            "UPDATE catalog.connection_instances \
                SET active_generation=1,revision=1,updated_at=now()+interval '1 microsecond' \
              WHERE tenant_id=$1 AND environment=$2 AND instance_id=$3",
            &[&TENANT, &ENVIRONMENT, &instance_id],
        )
        .await?;
    Ok(())
}

async fn provision(admin_url: &str, echo_url: &str, escape_url: &str) -> anyhow::Result<()> {
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

    let positive_flow = parse_flow(FLOW_JSON)?;
    let deny_flow = parse_flow(DENY_FLOW_JSON)?;
    let escape_flow = parse_flow(ESCAPE_FLOW_JSON)?;
    let positive = insert_artifact(&client, &positive_flow).await?;
    let deny = insert_artifact(&client, &deny_flow).await?;
    let escape = insert_artifact(&client, &escape_flow).await?;
    let positive_hash = positive.identity().artifact_hash().as_str();
    let deny_hash = deny.identity().artifact_hash().as_str();
    let escape_hash = escape.identity().artifact_hash().as_str();
    let members = json!([
        {"flow-id": FLOW_ID, "flow-version": 1, "artifact-hash": positive_hash},
        {"flow-id": DENY_FLOW_ID, "flow-version": 1, "artifact-hash": deny_hash},
        {"flow-id": ESCAPE_FLOW_ID, "flow-version": 1, "artifact-hash": escape_hash}
    ]);

    client
        .execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state,document) \
             VALUES ($1,$2,1,$3,'0.1','applied','{}')",
            &[&TENANT, &CATALOG_ID, &ENVIRONMENT],
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
            "INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ($1, \
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
               '0.1',decode('7b7d','hex'),2) \
             ON CONFLICT DO NOTHING",
            &[&TENANT],
        )
        .await?;
    for flow_id in [FLOW_ID, DENY_FLOW_ID, ESCAPE_FLOW_ID] {
        release
            .execute(
                "INSERT INTO catalog.release_flows \
                   (tenant_id,catalog_id,catalog_version,flow_id,flow_version, \
                    execution_bundle_hash) \
                 VALUES ($1,$2,1,$3,1, \
                   'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a')",
                &[&TENANT, &CATALOG_ID, &flow_id],
            )
            .await?;
    }
    release
        .execute(
            "INSERT INTO catalog.release_exposure_manifests \
               (tenant_id,catalog_id,catalog_version,definitions_json) VALUES ($1,$2,1,'{}')",
            &[&TENANT, &CATALOG_ID],
        )
        .await?;
    for (source, attachment, flow_id, definition_hash, path) in [
        (
            "positive-source",
            ATTACHMENT_ID,
            FLOW_ID,
            DEFINITION_HASH,
            "/positive",
        ),
        (
            "deny-source",
            DENY_ATTACHMENT_ID,
            DENY_FLOW_ID,
            DENY_DEFINITION_HASH,
            "/deny",
        ),
        (
            "escape-source",
            ESCAPE_ATTACHMENT_ID,
            ESCAPE_FLOW_ID,
            ESCAPE_DEFINITION_HASH,
            "/escape",
        ),
    ] {
        let source_hash = format!("sha256:{source}");
        release
            .execute(
                "INSERT INTO catalog.release_sources \
                   (tenant_id,catalog_id,catalog_version,source_id,source_kind,definition_json,source_hash) \
                 VALUES ($1,$2,1,$3,'auth','{}',$4)",
                &[&TENANT, &CATALOG_ID, &source, &source_hash],
            )
            .await?;
        release
            .execute(
                "INSERT INTO catalog.release_attachments \
                   (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id,source_id, \
                    definition_hash,definition_json,route_host,route_path,route_template,route_method) \
                 VALUES ($1,$2,1,$3,'http',$4,$5,$6, \
                         '{\"run-deadline-ms\":60000,\"response-deadline-ms\":30000}', \
                         'credproof.test',$7,$7,'POST')",
                &[
                    &TENANT,
                    &CATALOG_ID,
                    &attachment,
                    &flow_id,
                    &source,
                    &definition_hash,
                    &path,
                ],
            )
            .await?;
    }
    release
        .execute(
            "INSERT INTO catalog.catalog_heads \
               (tenant_id,catalog_id,environment,applied_catalog_version) VALUES ($1,$2,$3,1)",
            &[&TENANT, &CATALOG_ID, &ENVIRONMENT],
        )
        .await?;
    for (attachment, definition_hash) in [
        (ATTACHMENT_ID, DEFINITION_HASH),
        (DENY_ATTACHMENT_ID, DENY_DEFINITION_HASH),
        (ESCAPE_ATTACHMENT_ID, ESCAPE_DEFINITION_HASH),
    ] {
        release
            .execute(
                "INSERT INTO catalog.attachment_activation \
                   (tenant_id,catalog_id,environment,attachment_id,confirmed_definition_hash,enabled) \
                 VALUES ($1,$2,$3,$4,$5,true)",
                &[
                    &TENANT,
                    &CATALOG_ID,
                    &ENVIRONMENT,
                    &attachment,
                    &definition_hash,
                ],
            )
            .await?;
    }
    release.commit().await?;

    insert_connection(
        &client,
        INSTANCE_ID,
        echo_url,
        "sha256:credproof-generation",
    )
    .await?;
    insert_connection(
        &client,
        ESCAPE_INSTANCE_ID,
        escape_url,
        "sha256:credproof-address-escape-generation",
    )
    .await?;
    client
        .execute(
            wamn_schema_control::connections::insert_connection_binding_sql(),
            &[
                &TENANT,
                &CATALOG_ID,
                &1_i32,
                &positive_hash,
                &CONNECTION_NAME,
                &ENVIRONMENT,
                &INSTANCE_ID,
                &"active",
                &"valid",
                &"sha256:credproof-binding",
            ],
        )
        .await?;
    client
        .execute(
            wamn_schema_control::connections::insert_connection_binding_sql(),
            &[
                &TENANT,
                &CATALOG_ID,
                &1_i32,
                &escape_hash,
                &CONNECTION_NAME,
                &ENVIRONMENT,
                &ESCAPE_INSTANCE_ID,
                &"active",
                &"valid",
                &"sha256:credproof-address-escape-binding",
            ],
        )
        .await?;

    drop(client);
    let _ = handle.await;
    Ok(())
}

fn request(attachment_id: &str, definition_hash: &str, key: &str) -> InvokeRequest {
    InvokeRequest {
        attachment_id: attachment_id.to_string(),
        expected_catalog_version: 1,
        expected_definition_hash: definition_hash.to_string(),
        client_request_fingerprint: format!("sha256:{key}"),
        payload: "{}".to_string(),
        idempotency_key: Some(key.to_string()),
        principal: "credproof-principal".to_string(),
        deadline_override: None,
        trace: None,
    }
}

async fn assert_containment(admin_url: &str, run_id: &str, secret: &str) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url).await?;
    let row = client
        .query_one(
            "SELECT input_json::text,result_json::text,state_json::text,fail_reason \
               FROM wamn_run.runs WHERE tenant_id=$1 AND run_id=$2",
            &[&TENANT, &run_id],
        )
        .await?;
    for index in 0..4 {
        let text: Option<String> = row.get(index);
        ensure!(
            !text.as_deref().is_some_and(|value| value.contains(secret)),
            "secret leaked into run column {index}"
        );
    }
    let rows = client
        .query(
            "SELECT input_json::text,output_json::text,error_detail::text \
               FROM wamn_run.node_runs WHERE tenant_id=$1 AND run_id=$2",
            &[&TENANT, &run_id],
        )
        .await?;
    for row in rows {
        for index in 0..3 {
            let text: Option<String> = row.get(index);
            ensure!(
                !text.as_deref().is_some_and(|value| value.contains(secret)),
                "secret leaked into node attempt column {index}"
            );
        }
    }
    let graphs: Vec<String> = client
        .query(
            "SELECT graph_json::text FROM catalog.flow_artifacts WHERE tenant_id=$1",
            &[&TENANT],
        )
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    ensure!(graphs.iter().all(|graph| !graph.contains(secret)));
    drop(client);
    let _ = handle.await;
    Ok(())
}

async fn assert_delivery(admin_url: &str, run_id: &str, secret: &str) -> anyhow::Result<()> {
    let (client, handle) = connect(admin_url).await?;
    let row = client
        .query_one(
            "SELECT output_json::text FROM wamn_run.node_runs \
               WHERE tenant_id=$1 AND run_id=$2 AND local_node_id='notify'",
            &[&TENANT, &run_id],
        )
        .await?;
    let output: String = row.get(0);
    let output: serde_json::Value = serde_json::from_str(&output)?;
    let expected = format!("{:016x}", fnv1a_64(secret.as_bytes()));
    ensure!(
        output["body"]["authorization-fnv1a"].as_str() == Some(expected.as_str()),
        "credential delivery digest did not match the environment-selected secret"
    );
    drop(client);
    let _ = handle.await;
    Ok(())
}

pub async fn run(args: CredProofArgs) -> anyhow::Result<()> {
    let name = proof_database_name();
    create_database(&args.admin_database_url, &name).await?;
    let admin_url = database_url(&args.admin_database_url, &name)?;
    let app_url = database_url(&args.database_url, &name)?;
    let result = async {
        provision(&admin_url, &args.echo_url, &args.escape_url).await?;
        let guest = std::fs::read(&args.flowrunner)
            .with_context(|| format!("read {}", args.flowrunner.display()))?;
        let engine = build_engine(&[])?;
        let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
        let mut pg_config = WamnPostgresConfig::from_env();
        pg_config.database_url = Some(app_url.clone());
        let postgres = Arc::new(WamnPostgres::new(pg_config)?);
        let credentials = Arc::new(WamnCredentials::from_projects(HashMap::from([(
            PROJECT.to_string(),
            HashMap::from([(
                CREDENTIAL_HANDLE.to_string(),
                json!({"headers": {"authorization": args.secret}}).to_string(),
            )]),
        )])));
        let logging = Arc::new(WamnLogging::from_env()?);
        let uri: hyper::Uri = args.echo_url.parse().context("parse --echo-url")?;
        let authority = uri.authority().context("--echo-url has no authority")?;
        let allowed: AllowedHost = authority.as_str().parse().context("allow echo authority")?;
        let escape_uri: hyper::Uri = args
            .escape_url
            .parse()
            .context("parse --escape-url")?;
        let escape_authority = escape_uri
            .authority()
            .context("--escape-url has no authority")?;
        let escape_allowed: AllowedHost = escape_authority
            .as_str()
            .parse()
            .context("allow escape authority through the hostname ceiling")?;
        let host = ExecutionHost::instantiate(
            &engine,
            &guest,
            postgres,
            credentials,
            logging,
            ExecutionIdentity {
                owner: "credproof-runner",
                tenant: TENANT,
                schema: Some("wamn_run"),
                project: PROJECT,
            },
            production_capabilities(
                Arc::from([allowed, escape_allowed]),
                Arc::new(RunnerEgressPolicy::default()),
            ),
            30_000,
        )
        .await?;
        let service = InvocationService::new(
            PostgresInvocationBackend::from_database_url(&app_url)?,
            Some(app_url.clone()),
            InvocationServiceConfig {
                tenant_id: TENANT.to_string(),
                catalog_id: CATALOG_ID.to_string(),
                environment: ENVIRONMENT.to_string(),
                project: PROJECT.to_string(),
                schema: Some("wamn_run".to_string()),
                executor_id: "credproof-runner".to_string(),
                platform_revision: "credproof".to_string(),
                lease_ttl: std::time::Duration::from_secs(30),
                admission_ttl: std::time::Duration::from_secs(60),
            },
            Arc::new(ProofDriver {
                host: Arc::new(tokio::sync::Mutex::new(host)),
            }),
        );
        let timeout_ms = u32::try_from(args.timeout_secs.saturating_mul(1_000)).unwrap_or(u32::MAX);

        let BeginResult::Admitted(positive) = service
            .begin(request(ATTACHMENT_ID, DEFINITION_HASH, "credproof-positive"))
            .await?
        else {
            bail!("positive credproof admission was refused");
        };
        let Some(InvokeResult::Responded(response)) =
            service.wait(positive.run_id.clone(), timeout_ms).await?
        else {
            bail!("positive credproof did not return a response");
        };
        ensure!(response.status_hint == Some(200));
        ensure!(response.body == "200");
        assert_delivery(&admin_url, &positive.run_id, &args.secret).await?;
        let secret_marker = args.secret.strip_prefix("Bearer ").unwrap_or(&args.secret);
        assert_containment(&admin_url, &positive.run_id, secret_marker).await?;

        let BeginResult::Admitted(denied) = service
            .begin(request(
                DENY_ATTACHMENT_ID,
                DENY_DEFINITION_HASH,
                "credproof-deny",
            ))
            .await?
        else {
            bail!("deny credproof admission was refused before the connection check");
        };
        let Some(InvokeResult::Failed(failure)) =
            service.wait(denied.run_id.clone(), timeout_ms).await?
        else {
            bail!("unbound credproof did not return a stored failure");
        };
        ensure!(
            failure.error.code.contains("unbound")
                || failure
                    .error
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("unbound")),
            "deny failure was not the typed unbound refusal: {failure:?}"
        );

        let BeginResult::Admitted(escape) = service
            .begin(request(
                ESCAPE_ATTACHMENT_ID,
                ESCAPE_DEFINITION_HASH,
                "credproof-address-escape",
            ))
            .await?
        else {
            bail!("address-escape credproof admission was refused before dispatch");
        };
        let escape_result = service.wait(escape.run_id.clone(), timeout_ms).await?;
        ticker.abort();
        match escape_result {
            None | Some(InvokeResult::Failed(_)) => {}
            Some(InvokeResult::Responded(_)) => {
                bail!("address escape reached the environment-owned denied target");
            }
        }
        println!(
            "credproof PASS: admitted binding delivered credentials; unbound artifact denied; address escape blocked; containment held"
        );
        anyhow::Ok(())
    }
    .await;
    let cleanup = drop_database(&args.admin_database_url, &name).await;
    result.and(cleanup)
}
