//! `nodeinvoke` — the production custom-node invocation gate (5.6 / wamn-bd5).
//!
//! Proves the WHOLE v0 path end-to-end, locally and repeatably: the REAL runner
//! (the production [`ExecutionHost`] driving `flowrunner.wasm`) executes a flow whose
//! step is a CUSTOM node, which names only the exact component digest admitted
//! by its release. The trusted runner host resolves that digest through its
//! environment-owned placement map, signs the invocation, and dispatches it to
//! a REAL [`ServeNode`] host serving `node-cred.wasm` under the real `wamn:node`
//! world. Both wasmtime stores run concurrently on ONE task via `select!` (no
//! cross-thread store), so the host-owned POST reaches the serve-node's `/run`
//! and the reply folds back into the walk.
//!
//! Assertions (each named):
//!   * DELIVERY — every seeded run completes; the custom node's `node_runs`
//!     output round-trips the input payload (payload in -> node output back);
//!   * GRANT — the node reads its DECLARED credential (`ok:<secret>`): the
//!     runner declared exactly it in the envelope, the serve-node host installed
//!     it as the per-invocation grant;
//!   * NOT-GRANTED — the node's probe of an UNDECLARED (sibling) credential is
//!     `not-granted` at the real WIT boundary (the credprobe negative, now live);
//!   * MEMOIZED — across N runs sharing one custom-node config, the serve-node
//!     parsed that config exactly ONCE (design-note 9b).
//!
//! The runner never gets the trusted grant channel FOR THE NODE — the node is a
//! separate component the serve-node host grants get-only. A forged-wider grant
//! is the mutation (a) target, killed by NOT-GRANTED.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::Artifact;
use wamn_flow::Flow;
use wamn_node_invoke::{
    NodeInvokeRequest, SIGNATURE_HEADER, SIGNING_KEY_CREDENTIAL, SIGNING_KEY_CREDENTIAL_PREVIOUS,
    SignatureError, WirePayload, WireRunContext, granted_credentials, sign_envelope,
    sign_envelope_with_timestamp,
};
use wamn_node_manifest::{CapabilityClass, ResolvedNodeInterface};
use wamn_run_state::queue::{enqueue_sql, write_ahead_triggered_run_sql};

use crate::node_host_support::{self as serve_node, ServeNode, ServeNodeAuthn};
use wamn_execution_host::{ExecutionHost, ExecutionIdentity, production_capabilities};
use wamn_gate_harness::check;
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::node_invocation::NodePlacementMap;
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wash_runtime::host::allowed_hosts::AllowedHost;

const SCHEMA: &str = "wamn_nodeinvoke_bench";
const TENANT: &str = "nodeinvoke-tenant";
const OWNER: &str = "nodeinvoke-bench";
const FLOW_ID: &str = "node-invoke";
const CATALOG_ID: &str = "nodeinvoke-catalog";
const ENVIRONMENT: &str = "test";
const FLOW_VERSION: i32 = 1;
const PROJECT: &str = "default";
const CATALOG_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
/// Distinctive secrets so `ok:<secret>` / the leak are unambiguous.
const SECRET: &str = "node-secret-7c1f2a";
const SIBLING_SECRET: &str = "sibling-secret-do-not-leak";
/// The per-project-env HMAC signing key (wamn-fqg.22), banked in BOTH the
/// runner host's vault (so the trusted invocation plugin signs) and the
/// serve-node's vault (so it verifies) under the reserved
/// `SIGNING_KEY_CREDENTIAL` name. A wrong key is used by the negative host.
const SIGNING_KEY: &str = "fqg22-per-project-env-hmac-0a1b2c3d4e5f";
const WRONG_KEY: &str = "attacker-guessed-the-wrong-key";
/// The PREVIOUS per-project-env key for the wamn-fqg.30 rotation-window assert.
const PREV_KEY: &str = "fqg30-previous-rotation-key-9f8e7d6c";

#[derive(Debug, Args)]
pub struct NodeInvokeArgs {
    /// The flowrunner guest (`flowrunner.wasm`) the runner drives.
    #[arg(long)]
    pub flowrunner: PathBuf,

    /// The credential-reading custom node (`node_cred.wasm`) the serve-node host
    /// serves under the real wamn:node world.
    #[arg(long)]
    pub node_cred: PathBuf,

    /// App (runner) Postgres URL — the NOSUPERUSER wamn_app role.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL: provisions/drops the ephemeral schema.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Loopback port the serve-node HTTP server binds. The trusted host maps the
    /// release-pinned component digest to this authority; the flow never sees it.
    #[arg(long, default_value_t = 8091)]
    pub node_port: u16,

    /// Runs seeded (each drives the same custom-node config, so memoization is
    /// observable — N runs, one config parse).
    #[arg(long, default_value_t = 12)]
    pub iters: usize,
}

/// The custom-node flow: `in -> call(custom) -> done(respond)`. The `call` step
/// declares credential `granted` and probes `granted` (declared -> readable) +
/// `sibling` (undeclared -> not-granted). Placement is deliberately absent.
fn flow_json() -> String {
    format!(
        r#"{{"schema-version":"0.1","flow-id":"{FLOW_ID}","version":{FLOW_VERSION},
            "credentials":[{{"name":"granted"}}],
            "nodes":[
              {{"id":"in","type":"request","config":{{"input-schema":true}}}},
              {{"id":"call","type":"custom","credential":"granted",
                "config":{{"probe":"granted","forbidden":"sibling"}}}},
              {{"id":"done","type":"respond","config":{{"status":200}}}}
            ],
            "edges":[{{"from":"in","to":"call"}},{{"from":"call","to":"done"}}]}}"#
    )
}

#[derive(Debug)]
struct PublishedNodeInvoke {
    graph_json: String,
    artifact_hash: String,
    implementation_digest: String,
}

fn implementation_digest(component: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(component)))
}

fn admitted_artifact(node_wasm: &[u8]) -> anyhow::Result<(Flow, Artifact, String)> {
    let flow = Flow::from_json(&flow_json())
        .map_err(|error| anyhow::anyhow!("parse nodeinvoke release graph: {error}"))?;
    let implementation_digest = implementation_digest(node_wasm);
    let request = wamn_standard_nodes::describe("request")
        .context("missing standard-node descriptor for request")?;
    let request = wamn_standard_nodes::resolve_descriptor(request).map_err(anyhow::Error::new)?;
    let mut implementations = vec![
        request.interface,
        ResolvedNodeInterface::new(
            "custom",
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
    ];
    let respond = wamn_standard_nodes::describe("respond")
        .context("missing standard-node descriptor for respond")?;
    let respond = wamn_standard_nodes::resolve_descriptor(respond).map_err(anyhow::Error::new)?;
    implementations.push(respond.interface);
    implementations.sort_by(|left, right| left.node_type.cmp(&right.node_type));
    let artifact = Artifact::new(TENANT, &flow, implementations)
        .map_err(|error| anyhow::anyhow!("build nodeinvoke release artifact: {error}"))?;
    Ok((flow, artifact, implementation_digest))
}

// --- ephemeral schema (the flowrunner flow tables + the run_queue) -----------
fn runner_ddl(schema: &str) -> String {
    format!(
        "CREATE TABLE {schema}.flows (\
            tenant_id text NOT NULL, flow_id text NOT NULL, version int NOT NULL, \
            active boolean NOT NULL DEFAULT false, graph_json jsonb NOT NULL, \
            PRIMARY KEY (tenant_id, flow_id, version));\
         ALTER TABLE {schema}.flows ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.flows FORCE ROW LEVEL SECURITY;\
         CREATE POLICY flows_tenant ON {schema}.flows \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.flows TO wamn_app;\
         CREATE TABLE {schema}.sink (\
            tenant_id text NOT NULL, run_id text NOT NULL, step int NOT NULL, \
            payload text NOT NULL, \
            CONSTRAINT sink_idem UNIQUE (tenant_id, run_id, step));\
         ALTER TABLE {schema}.sink ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.sink FORCE ROW LEVEL SECURITY;\
         CREATE POLICY sink_tenant ON {schema}.sink \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.sink TO wamn_app;\
         CREATE TABLE {schema}.runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
            flow_version int NOT NULL, \
            status text NOT NULL DEFAULT 'running' \
              CHECK (status IN ('dispatched','running','completed','failed','infrastructure-failure','effect-uncertain')), \
            trigger_source text, input_json jsonb, result_json jsonb, state_json jsonb, \
            created_at timestamptz NOT NULL DEFAULT now(), \
            updated_at timestamptz NOT NULL DEFAULT now(), \
            catalog_id text, catalog_version bigint, environment text, \
            attachment_id text, registration_id text, event_source_run_id text, \
            event_root_run_id text, event_depth int, \
            invocation_context jsonb NOT NULL DEFAULT '{{}}'::jsonb, \
            admission_context_version int NOT NULL DEFAULT 1, \
            platform_revision text NOT NULL DEFAULT 'nodeinvoke', \
            response_deadline_at timestamptz, run_deadline_at timestamptz, \
            terminal_reason text, \
            caller_outcome_kind text, caller_outcome_json jsonb, \
            caller_http_status int, caller_release_node_id text, \
            caller_outcome_hash text, caller_released_at timestamptz, \
            idempotency_key text, replay_of text, root_run_id text, \
            parent_run_id text, parent_node_id text, parent_occurrence int, \
            invoke_depth int NOT NULL DEFAULT 0, invoke_root_run_id text, \
            waiting_child_run_id text, waiting_child_occurrence int, wait_generation bigint, \
            fail_kind text, fail_node text, fail_reason text, \
            PRIMARY KEY (tenant_id, run_id));\
         ALTER TABLE {schema}.runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY runs_tenant ON {schema}.runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.runs TO wamn_app;\
         CREATE TABLE {schema}.node_runs (\
            tenant_id text NOT NULL, run_id text NOT NULL, node_id text NOT NULL, \
            occurrence int NOT NULL DEFAULT 0, seq int NOT NULL, \
            status text NOT NULL, output_port text, output_json jsonb, input_json jsonb, \
            error_kind text, error_detail jsonb, \
            input_ref text, output_ref text, \
            preview_head text, payload_size bigint, payload_hash text, capture_mode text, \
            redacted boolean NOT NULL DEFAULT false, \
            started_at timestamptz NOT NULL DEFAULT now(), ended_at timestamptz, \
            PRIMARY KEY (tenant_id, run_id, node_id, occurrence), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         ALTER TABLE {schema}.node_runs ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.node_runs FORCE ROW LEVEL SECURITY;\
         CREATE POLICY node_runs_tenant ON {schema}.node_runs \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.node_runs TO wamn_app;\
         CREATE TABLE {schema}.run_queue (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text, \
            partition_policy text NOT NULL DEFAULT 'blocking' \
              CHECK (partition_policy IN ('blocking','leapfrog')), \
            priority int NOT NULL DEFAULT 0, available_at timestamptz NOT NULL DEFAULT now(), \
            lease_owner text, lease_expires_at timestamptz, \
            lease_generation bigint NOT NULL DEFAULT 0, \
            attempts int NOT NULL DEFAULT 0, max_attempts int NOT NULL DEFAULT 20, \
            enqueued_at timestamptz NOT NULL DEFAULT now(), \
            stream_seq bigint NOT NULL DEFAULT 0, \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         CREATE INDEX run_queue_claimable ON {schema}.run_queue (tenant_id, available_at, stream_seq, lease_expires_at);\
         CREATE INDEX run_queue_partition ON {schema}.run_queue (tenant_id, partition_key) WHERE partition_key IS NOT NULL;\
         ALTER TABLE {schema}.run_queue ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_queue FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_queue_tenant ON {schema}.run_queue \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.run_queue TO wamn_app;\
         CREATE TABLE {schema}.partition_owner (\
            tenant_id text NOT NULL, partition_key text NOT NULL, \
            lease_owner text NOT NULL, lease_expires_at timestamptz NOT NULL, \
            acquired_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, partition_key));\
         ALTER TABLE {schema}.partition_owner ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.partition_owner FORCE ROW LEVEL SECURITY;\
         CREATE POLICY partition_owner_tenant ON {schema}.partition_owner \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.partition_owner TO wamn_app;\
         CREATE TABLE {schema}.run_dead_letters (\
            tenant_id text NOT NULL, run_id text NOT NULL, partition_key text NOT NULL, \
            flow_id text NOT NULL, reason text NOT NULL, \
            failed_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant_id, run_id), \
            FOREIGN KEY (tenant_id, run_id) REFERENCES {schema}.runs (tenant_id, run_id) ON DELETE CASCADE);\
         ALTER TABLE {schema}.run_dead_letters ENABLE ROW LEVEL SECURITY;\
         ALTER TABLE {schema}.run_dead_letters FORCE ROW LEVEL SECURITY;\
         CREATE POLICY run_dead_letters_tenant ON {schema}.run_dead_letters \
            USING (tenant_id = current_setting('app.tenant', true)) \
            WITH CHECK (tenant_id = current_setting('app.tenant', true));\
         GRANT SELECT, INSERT ON {schema}.run_dead_letters TO wamn_app;"
    )
}

async fn provision(
    admin_url: &str,
    node_wasm: &[u8],
) -> anyhow::Result<(PublishedNodeInvoke, bool)> {
    let (mut client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        let catalog_exists: bool = client
            .query_one("SELECT to_regnamespace('catalog') IS NOT NULL", &[])
            .await?
            .get(0);
        let created_catalog = if catalog_exists {
            let complete: bool = client
                .query_one(
                    "SELECT to_regclass('catalog.flow_artifacts') IS NOT NULL \
                        AND to_regclass('catalog.release_manifests') IS NOT NULL \
                        AND to_regclass('catalog.release_flows') IS NOT NULL \
                        AND to_regclass('catalog.connection_requirements') IS NOT NULL \
                        AND to_regclass('catalog.connection_instances') IS NOT NULL \
                        AND to_regclass('catalog.connection_generations') IS NOT NULL \
                        AND to_regclass('catalog.connection_bindings') IS NOT NULL",
                    &[],
                )
                .await?
                .get(0);
            if !complete {
                bail!(
                    "existing catalog schema is incomplete for trusted node-invocation admission"
                );
            }
            false
        } else {
            client
                .batch_execute(CATALOG_SQL)
                .await
                .context("apply catalog DDL for the standalone gate")?;
            true
        };
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; CREATE SCHEMA {SCHEMA} AUTHORIZATION postgres; GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_app;"
            ))
            .await
            .context("create ephemeral schema")?;
        client
            .batch_execute(&runner_ddl(SCHEMA))
            .await
            .context("apply runner DDL")?;

        let (flow, artifact, implementation_digest) = admitted_artifact(node_wasm)?;
        let graph_json = String::from_utf8(flow.canonical_bytes())
            .context("canonical nodeinvoke graph is not UTF-8")?;
        let artifact_hash = artifact.identity().artifact_hash().as_str().to_string();
        let members = json!([{
            "flow-id": FLOW_ID,
            "flow-version": FLOW_VERSION,
            "artifact-hash": artifact_hash,
        }]);

        let transaction = client.transaction().await?;
        transaction
            .execute(
                "INSERT INTO catalog.catalogs \
                   (tenant_id,catalog_id,version,environment,schema_version,state) \
                 VALUES ($1,$2,$3,$4,'0.1','staged') \
                 ON CONFLICT (tenant_id,catalog_id,version) DO NOTHING",
                &[&TENANT, &CATALOG_ID, &FLOW_VERSION, &ENVIRONMENT],
            )
            .await?;
        transaction
            .execute(
                "SELECT catalog.register_flow_artifact( \
                   $1,$2,$3,$4,$5::text::jsonb,$6,$7)",
                &[
                    &TENANT,
                    &FLOW_ID,
                    &FLOW_VERSION,
                    &artifact.schema_version(),
                    &graph_json,
                    &artifact.graph_hash(),
                    &artifact_hash,
                ],
            )
            .await?;
        transaction
            .execute(
                "SELECT catalog.register_release_manifest($1,$2,$3,$4)",
                &[&TENANT, &CATALOG_ID, &FLOW_VERSION, &members],
            )
            .await?;
        transaction
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
        transaction
            .execute(
                "INSERT INTO catalog.release_flows \
                   (tenant_id,catalog_id,catalog_version,flow_id,flow_version, \
                    execution_bundle_hash) \
                 VALUES ($1,$2,$3,$4,$3, \
                   'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a') \
                 ON CONFLICT DO NOTHING",
                &[&TENANT, &CATALOG_ID, &FLOW_VERSION, &FLOW_ID],
            )
            .await?;
        transaction.commit().await?;

        anyhow::Ok((
            PublishedNodeInvoke {
                graph_json,
                artifact_hash,
                implementation_digest,
            },
            created_catalog,
        ))
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn teardown(admin_url: &str, created_catalog: bool) {
    if let Ok((client, conn)) = tokio_postgres::connect(admin_url, NoTls).await {
        let conn_task = tokio::spawn(conn);
        let _ = client
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"))
            .await;
        if created_catalog {
            let _ = client
                .batch_execute("DROP SCHEMA IF EXISTS catalog CASCADE")
                .await;
        }
        drop(client);
        let _ = conn_task.await;
    }
}

async fn connect_app(app_url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(app_url, NoTls)
        .await
        .context("app (wamn_app) connect")?;
    let handle = tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!(
            "SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';"
        ))
        .await
        .context("set search_path + tenant claim")?;
    Ok((client, handle))
}

/// Seed a run the way the dispatcher does: the write-ahead `dispatched` row +
/// the queue row, co-transacted. The trigger input is a JSON string the
/// custom node echoes back.
async fn seed_run(
    client: &mut Client,
    published: &PublishedNodeInvoke,
    run_id: &str,
    input_json: &str,
) -> anyhow::Result<()> {
    let tx = client.transaction().await?;
    tx.execute(
        &write_ahead_triggered_run_sql(),
        &[&run_id, &FLOW_ID, &FLOW_VERSION, &"manual", &input_json],
    )
    .await?;
    let invocation_context = json!({
        "version": 1,
        "principal": {
            "tenant-id": TENANT,
            "environment": ENVIRONMENT,
            "catalog-id": CATALOG_ID,
            "catalog-version": FLOW_VERSION,
            "run-id": run_id,
            "flow-id": FLOW_ID,
            "flow-version": FLOW_VERSION,
            "artifact-digest": published.artifact_hash,
        },
        "source": { "trigger": "manual" },
    });
    tx.execute(
        "UPDATE runs SET catalog_id=$2,catalog_version=$3,environment=$4, \
                invocation_context=$5,admission_context_version=1,platform_revision='nodeinvoke' \
          WHERE tenant_id=$6 AND run_id=$1",
        &[
            &run_id,
            &CATALOG_ID,
            &i64::from(FLOW_VERSION),
            &ENVIRONMENT,
            &invocation_context,
            &TENANT,
        ],
    )
    .await?;
    tx.execute(
        &enqueue_sql(),
        &[&run_id, &Option::<&str>::None, &0i32, &0i64],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn count(client: &Client, sql: &str) -> anyhow::Result<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}

pub async fn run(args: NodeInvokeArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let flowrunner = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read flowrunner {}", args.flowrunner.display()))?;
    let node_wasm = std::fs::read(&args.node_cred)
        .with_context(|| format!("read node-cred {}", args.node_cred.display()))?;
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let admin_url = args.admin_database_url.clone().context(
        "nodeinvoke needs a superuser url: pass --admin-database-url / WAMN_PG_ADMIN_URL",
    )?;
    let port = args.node_port;
    let n = args.iters;

    println!(
        "# wamn-gates nodeinvoke — trusted host custom-node invocation (schema {SCHEMA}, node port {port})"
    );
    let (published, created_catalog) = provision(&admin_url, &node_wasm)
        .await
        .context("provision admitted nodeinvoke release")?;

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    // The serve-node host: a warm node-cred instance whose vault has the granted
    // secret AND an ungranted sibling in the same project. The runner->node hop
    // is loopback; the node's OWN egress is deny-all (it makes none).
    let node_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([
                ("granted".to_string(), SECRET.to_string()),
                ("sibling".to_string(), SIBLING_SECRET.to_string()),
                // wamn-fqg.22: the serve-node reads its per-project-env signing key
                // from THIS vault (the shared runner-credentials Secret in prod) and
                // enforces verify-before-grant.
                (SIGNING_KEY_CREDENTIAL.to_string(), SIGNING_KEY.to_string()),
            ]),
        )]),
    ));
    let serve = Arc::new(
        ServeNode::new(
            &engine,
            &node_wasm,
            node_vault,
            serve_node::DEFAULT_NODE_ID,
            PROJECT,
            Arc::from([]),
            ServeNodeAuthn {
                require_signing_key: true,
                // wamn-fqg.32: replay-freshness OFF (default) for the E2E drain
                max_signature_age_secs: None,
            },
        )
        .await
        .context("build serve-node")?,
    );

    // Drive the gate while the serve-node accept loop runs concurrently on the
    // SAME task (select!): when the gate logic returns, the server future drops.
    let serve_loop = serve_node::serve(serve.clone(), port);
    let gate = gate_body(
        &engine,
        &flowrunner,
        &node_wasm,
        &app_url,
        &published,
        serve.clone(),
        port,
        n,
    );

    let outcome = tokio::select! {
        r = serve_loop => r.map(|_| false), // the server only ends on error
        r = gate => r,
    };

    ticker.abort();
    teardown(&admin_url, created_catalog).await;
    let pass = outcome?;

    println!("\nnodeinvoke complete — overall PASS: {pass}");
    if !pass {
        bail!("nodeinvoke gate failed");
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "live gate receives each independently provisioned fixture dependency"
)]
async fn gate_body(
    engine: &wash_runtime::engine::Engine,
    flowrunner: &[u8],
    node_wasm: &[u8],
    app_url: &str,
    published: &PublishedNodeInvoke,
    serve: Arc<ServeNode>,
    port: u16,
    n: usize,
) -> anyhow::Result<bool> {
    let (mut seed_conn, _h) = connect_app(app_url).await?;
    wamn_gate_harness::seed_flow_version(
        &seed_conn,
        TENANT,
        FLOW_ID,
        FLOW_VERSION,
        true,
        &published.graph_json,
        true,
    )
    .await?;

    // The production runner. The custom node's own credentials resolve at the
    // serve-node vault; this host vault carries only the transport signing key.
    let mut cfg = WamnPostgresConfig::from_env();
    cfg.database_url = Some(app_url.to_string());
    let plugin = Arc::new(WamnPostgres::new(cfg)?);
    // The trusted invocation plugin, not the guest, resolves this environment's
    // signing key and signs the admitted one-frame request.
    let runner_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([(
                SIGNING_KEY_CREDENTIAL.to_string(),
                SIGNING_KEY.to_string(),
            )]),
        )]),
    ));
    let allowed: Arc<[AllowedHost]> = Arc::from([]);
    let placements = NodePlacementMap::singleton(
        published.implementation_digest.clone(),
        format!("http://127.0.0.1:{port}"),
    )?;

    let mut worker = ExecutionHost::instantiate(
        engine,
        flowrunner,
        plugin,
        runner_vault,
        Arc::new(wamn_runtime::plugins::wamn_logging::WamnLogging::from_env()?),
        ExecutionIdentity {
            owner: OWNER,
            tenant: TENANT,
            schema: Some(SCHEMA),
            project: PROJECT,
        },
        production_capabilities(allowed.clone(), Arc::new(RunnerEgressPolicy::default()))
            .with_node_placements(placements.clone()),
        30_000,
    )
    .await?;

    // Seed N runs of the custom-node flow, each echoing the same input.
    for i in 0..n {
        seed_run(&mut seed_conn, published, &format!("ni-{i}"), "\"hello\"").await?;
    }
    let report = worker.drain().await?;

    let queued = count(
        &seed_conn,
        &format!("SELECT count(*) FROM {SCHEMA}.run_queue"),
    )
    .await?;
    let completed = count(
        &seed_conn,
        &format!("SELECT count(*) FROM {SCHEMA}.runs WHERE status = 'completed'"),
    )
    .await?;

    let mut ok = true;
    check(
        &mut ok,
        "DELIVERY: every seeded run drained to completed",
        report.claimed == n && report.completed == n && queued == 0 && completed as usize == n,
    );
    // Diagnostics: the drain tally + any failure verdict + the recorded node
    // trail, so a broken hop is legible in the gate output (not a silent fail).
    println!(
        "  drain: claimed={} completed={} parked={} failed={} (queue rows left={queued})",
        report.claimed, report.completed, report.parked, report.failed
    );
    for row in seed_conn
        .query(
            &format!(
                "SELECT run_id, status, fail_kind, fail_node, fail_reason FROM {SCHEMA}.runs ORDER BY run_id LIMIT 3"
            ),
            &[],
        )
        .await?
    {
        let rid: String = row.get(0);
        let status: String = row.get(1);
        let fk: Option<String> = row.get(2);
        let fnode: Option<String> = row.get(3);
        let fr: Option<String> = row.get(4);
        println!(
            "  run {rid}: status={status} fail_kind={:?} fail_node={:?} fail_reason={:?}",
            fk, fnode, fr
        );
    }
    for row in seed_conn
        .query(
            &format!("SELECT node_id, status, error_kind FROM {SCHEMA}.node_runs WHERE run_id = 'ni-0' ORDER BY seq"),
            &[],
        )
        .await?
    {
        let nid: String = row.get(0);
        let st: String = row.get(1);
        let ek: Option<String> = row.get(2);
        println!("  ni-0 node_run: {nid} status={st} error_kind={:?}", ek);
    }

    // Inspect the custom node's recorded output on one run (payload round-trip +
    // the credential probes at the real WIT boundary).
    let out_row = seed_conn
        .query_one(
            &format!(
                "SELECT output_json::text FROM {SCHEMA}.node_runs WHERE run_id = 'ni-0' AND node_id = 'call'"
            ),
            &[],
        )
        .await
        .context("custom node produced no node_runs row")?;
    let out_text: String = out_row.get(0);
    let out: serde_json::Value = serde_json::from_str(&out_text).context("node output not JSON")?;

    check(
        &mut ok,
        "DELIVERY: input payload round-tripped through the node (echo == input)",
        out.get("echo").and_then(|v| v.as_str()) == Some("hello"),
    );
    check(
        &mut ok,
        "GRANT: the DECLARED credential is readable inside the node (ok:<secret>)",
        out.get("probe").and_then(|v| v.as_str()) == Some(&format!("ok:{SECRET}")[..]),
    );
    check(
        &mut ok,
        "NOT-GRANTED: an UNDECLARED (sibling) credential is not-granted at the boundary",
        out.get("forbidden").and_then(|v| v.as_str()) == Some("err:not-granted"),
    );
    // Belt and braces: the leaked secret text never appears in the recorded output.
    check(
        &mut ok,
        "NOT-GRANTED: the ungranted sibling secret never leaks into run history",
        !out_text.contains(SIBLING_SECRET),
    );

    // Design-note 9b: N runs share ONE custom-node config identity, so the warm
    // serve-node parsed that config exactly once.
    let parses = serve.config_parse_count().await;
    check(
        &mut ok,
        "MEMOIZED: N runs of one config parsed exactly once on the serve-node (9b)",
        parses == 1,
    );
    println!(
        "  (config parses on the warm serve-node = {parses} across {n} invocations; drained {}/{n})",
        report.completed
    );

    // -------------------------------------------------------------------------
    // wamn-fqg.22 — runner→node authn (signed invocation envelope).
    // -------------------------------------------------------------------------
    // The drain above ALREADY proves the signed positive end-to-end: the
    // serve-node holds a key, so it REQUIRES a valid signature; every run
    // completing means the REAL trusted host signed the exact body correctly
    // (an unsigned or forged host would 401 → DELIVERY would have failed).
    check(
        &mut ok,
        "AUTHN-POSITIVE: the signed hop drained N runs (a keyed serve-node accepted the trusted host's real signature)",
        report.completed == n,
    );
    let grants_after_positive = serve.grant_install_count();
    check(
        &mut ok,
        "AUTHN-POSITIVE: each accepted invocation installed its grant (grant_install_count advanced by ≥N)",
        grants_after_positive >= n as u64,
    );
    // R31 (wamn-2jkm.49): the grant is per-invocation — after the drain returns,
    // none may remain installed. A mutant that stops arming the revoke guard in
    // `invoke` leaves the last grant live and fails here.
    check(
        &mut ok,
        "GRANT-REVOKED: after the drain no per-invocation grant remains installed (the grant does not outlive its invocation)",
        !serve.invocation_grant_active(),
    );

    // Drive the serve-node directly over raw HTTP to exercise the refusal arms
    // the happy path cannot forge — the exact envelope the trusted host sends.
    let body = canonical_request().to_json();
    // The host clock (unix seconds) the fqg.32 freshness asserts compare against.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let good_sig = sign_envelope(SIGNING_KEY.as_bytes(), body.as_bytes());
    let wrong_sig = sign_envelope(WRONG_KEY.as_bytes(), body.as_bytes());
    let grants_before_negatives = serve.grant_install_count();

    // (1) UNSIGNED — no x-wamn-signature header at all.
    let (status, rbody) = raw_post(port, &body, None).await?;
    check(
        &mut ok,
        "AUTHN-UNSIGNED: an unsigned envelope is refused 401 (missing-signature)",
        status == 401 && rbody.contains("missing-signature"),
    );

    // (2) TAMPERED — a valid signature over the ORIGINAL body, but a MUTATED body
    // (attacker-chosen input) posted under it.
    let tampered = tampered_request().to_json();
    assert_ne!(tampered, body, "the tamper must actually change the body");
    let (status, rbody) = raw_post(port, &tampered, Some(&good_sig)).await?;
    check(
        &mut ok,
        "AUTHN-TAMPERED: a body that does not match its signature is refused 401 (bad-signature)",
        status == 401 && rbody.contains("bad-signature"),
    );

    // (3) WRONG-KEY — a well-formed signature under a key the attacker does not
    // share with the project-env.
    let (status, rbody) = raw_post(port, &body, Some(&wrong_sig)).await?;
    check(
        &mut ok,
        "AUTHN-WRONG-KEY: a signature under the wrong key is refused 401 (bad-signature)",
        status == 401 && rbody.contains("bad-signature"),
    );
    check(
        &mut ok,
        "AUTHN-NO-ORACLE: a refusal body never carries the expected MAC",
        !rbody.contains(&good_sig),
    );

    // VERIFY-BEFORE-GRANT (the load-bearing property, wamn-fqg.22): none of the
    // three refusals reached `invoke`, so NOT ONE installed a grant. A mutant
    // that removes/moves the verification lets a refused request install its
    // grant here → this named check kills it.
    let grants_after_negatives = serve.grant_install_count();
    check(
        &mut ok,
        "VERIFY-BEFORE-GRANT: not one refused request installed a grant (verify precedes grant install)",
        grants_after_negatives == grants_before_negatives,
    );

    // (4) RAW-SIGNED positive — a correctly-signed raw POST IS accepted (200) and
    // DOES install its grant, so the refusals above are a real contrast (the
    // check is not vacuously passing on a serve-node that refuses everything).
    let (status, _rbody) = raw_post(port, &body, Some(&good_sig)).await?;
    check(
        &mut ok,
        "AUTHN-SIGNED: a correctly-signed raw envelope is accepted (200) and installs exactly one grant",
        status == 200 && serve.grant_install_count() == grants_after_negatives + 1,
    );
    check(
        &mut ok,
        "GRANT-REVOKED: the raw-signed invocation's grant was revoked on return (R31)",
        !serve.invocation_grant_active(),
    );
    println!(
        "  authn: grants(after positive drain)={grants_after_positive}; refusals installed 0 grants (before={grants_before_negatives} after={grants_after_negatives}); raw-signed accepted"
    );

    // -------------------------------------------------------------------------
    // wamn-fqg.31 — fail-closed toggle (BOTH postures). verify_signature is the
    // pure verify-before-grant decision the accept loop makes; drive it directly
    // on two KEYLESS serve-nodes (an empty vault, no reserved signing key):
    //   * --require-signing-key  ⇒ REFUSE ALL (Unconfigured / signing-key-required),
    //     signed or unsigned — no silent revert to network trust;
    //   * default                ⇒ ADMIT unsigned (legacy network-trust, warned).
    // A mutant that drops the fail-closed arm admits the unsigned POST → the
    // FAIL-CLOSED check flips.
    // -------------------------------------------------------------------------
    let keyless_failclosed = ServeNode::new(
        engine,
        node_wasm,
        Arc::new(WamnCredentials::empty()),
        serve_node::DEFAULT_NODE_ID,
        PROJECT,
        Arc::from([]),
        ServeNodeAuthn {
            require_signing_key: true, // fail-closed
            max_signature_age_secs: None,
        },
    )
    .await
    .context("build keyless fail-closed serve-node")?;
    check(
        &mut ok,
        "FAIL-CLOSED (fqg.31): a keyless require-signing-key host REFUSES an unsigned invocation (signing-key-required)",
        keyless_failclosed.verify_signature(body.as_bytes(), None, None, now)
            == Err(SignatureError::Unconfigured),
    );
    check(
        &mut ok,
        "FAIL-CLOSED (fqg.31): it also refuses a SIGNED invocation — no key to verify, so refuse ALL",
        keyless_failclosed.verify_signature(body.as_bytes(), Some(&good_sig), None, now)
            == Err(SignatureError::Unconfigured),
    );
    let keyless_default = ServeNode::new(
        engine,
        node_wasm,
        Arc::new(WamnCredentials::empty()),
        serve_node::DEFAULT_NODE_ID,
        PROJECT,
        Arc::from([]),
        ServeNodeAuthn {
            require_signing_key: false, // default: legacy network-trust
            max_signature_age_secs: None,
        },
    )
    .await
    .context("build keyless default serve-node")?;
    check(
        &mut ok,
        "NETWORK-TRUST (fqg.31): the DEFAULT keyless host admits an unsigned invocation (backward-compatible)",
        keyless_default
            .verify_signature(body.as_bytes(), None, None, now)
            .is_ok(),
    );

    // -------------------------------------------------------------------------
    // wamn-fqg.30 — dual-key acceptance (rotation window). A serve-node holding
    // the CURRENT + PREVIOUS reserved keys accepts an envelope signed with EITHER
    // (the runner host always signs with the current key; the previous key covers
    // the window while hosts pick up the new one). Garbage still 401s. A mutant
    // that only ever checks the current key rejects the previous-key signature →
    // the first check flips.
    // -------------------------------------------------------------------------
    let dual_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([
                (SIGNING_KEY_CREDENTIAL.to_string(), SIGNING_KEY.to_string()),
                (
                    SIGNING_KEY_CREDENTIAL_PREVIOUS.to_string(),
                    PREV_KEY.to_string(),
                ),
            ]),
        )]),
    ));
    let dual = ServeNode::new(
        engine,
        node_wasm,
        dual_vault,
        serve_node::DEFAULT_NODE_ID,
        PROJECT,
        Arc::from([]),
        ServeNodeAuthn {
            require_signing_key: false,
            max_signature_age_secs: None,
        },
    )
    .await
    .context("build dual-key serve-node")?;
    let prev_sig = sign_envelope(PREV_KEY.as_bytes(), body.as_bytes());
    check(
        &mut ok,
        "DUAL-KEY (fqg.30): an envelope signed with the PREVIOUS key verifies during the rotation window",
        dual.verify_signature(body.as_bytes(), Some(&prev_sig), None, now)
            .is_ok(),
    );
    check(
        &mut ok,
        "DUAL-KEY (fqg.30): the CURRENT key still verifies alongside the previous",
        dual.verify_signature(body.as_bytes(), Some(&good_sig), None, now)
            .is_ok(),
    );
    check(
        &mut ok,
        "DUAL-KEY (fqg.30): a signature under NEITHER key is still refused (bad-signature)",
        dual.verify_signature(body.as_bytes(), Some(&wrong_sig), None, now)
            == Err(SignatureError::Mismatch),
    );

    // -------------------------------------------------------------------------
    // wamn-fqg.32 — replay freshness (timestamp, OFF by default). A serve-node
    // with a max-age configured requires a SIGNED, in-window timestamp; a stale
    // one is refused (stale-timestamp), a fresh one accepted. With freshness OFF
    // (the main keyed `serve` above), a LEGACY timestamp-less envelope still
    // verifies. A mutant that drops the age check accepts the stale envelope →
    // FRESHNESS-STALE flips; one that always checks freshness rejects the legacy
    // envelope → FRESHNESS-LEGACY flips.
    // -------------------------------------------------------------------------
    let fresh_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([(
                SIGNING_KEY_CREDENTIAL.to_string(),
                SIGNING_KEY.to_string(),
            )]),
        )]),
    ));
    let fresh = ServeNode::new(
        engine,
        node_wasm,
        fresh_vault,
        serve_node::DEFAULT_NODE_ID,
        PROJECT,
        Arc::from([]),
        ServeNodeAuthn {
            require_signing_key: false,
            max_signature_age_secs: Some(60), // enforce a 60s freshness window
        },
    )
    .await
    .context("build freshness-enforcing serve-node")?;
    let fresh_ts = now.to_string();
    let fresh_sig =
        sign_envelope_with_timestamp(SIGNING_KEY.as_bytes(), body.as_bytes(), Some(&fresh_ts));
    check(
        &mut ok,
        "FRESHNESS-FRESH (fqg.32): a fresh timestamped envelope is accepted when max-age is enforced",
        fresh
            .verify_signature(body.as_bytes(), Some(&fresh_sig), Some(&fresh_ts), now)
            .is_ok(),
    );
    let stale_ts = now.saturating_sub(3600).to_string();
    let stale_sig =
        sign_envelope_with_timestamp(SIGNING_KEY.as_bytes(), body.as_bytes(), Some(&stale_ts));
    check(
        &mut ok,
        "FRESHNESS-STALE (fqg.32): a correctly-signed but STALE envelope is refused (stale-timestamp)",
        fresh.verify_signature(body.as_bytes(), Some(&stale_sig), Some(&stale_ts), now)
            == Err(SignatureError::Stale),
    );
    check(
        &mut ok,
        "FRESHNESS-LEGACY (fqg.32): a legacy timestamp-less envelope still verifies when freshness is OFF",
        serve
            .verify_signature(body.as_bytes(), Some(&good_sig), None, now)
            .is_ok(),
    );

    // -------------------------------------------------------------------------
    // A persistent key mismatch is a PLATFORM transport/authentication fault,
    // never a node-authored failure. A trusted host whose environment key is
    // wrong reaches the real signing-required serve-node and receives 401. The
    // guest surfaces the typed host refusal as an outer execution error, leaving
    // the started attempt + queue lease for infrastructure recovery; it must not
    // manufacture a terminal/retryable NodeError or blame the custom node.
    // -------------------------------------------------------------------------
    seed_run(&mut seed_conn, published, "ni-mismatch", "\"hello\"").await?;
    let mismatch_vault = Arc::new(WamnCredentials::from_projects(
        std::collections::HashMap::from([(
            PROJECT.to_string(),
            std::collections::HashMap::from([(
                SIGNING_KEY_CREDENTIAL.to_string(),
                WRONG_KEY.to_string(),
            )]),
        )]),
    ));
    let mut mismatch_cfg = WamnPostgresConfig::from_env();
    mismatch_cfg.database_url = Some(app_url.to_string());
    let mismatch_plugin = Arc::new(WamnPostgres::new(mismatch_cfg)?);
    let mut mismatch_worker = ExecutionHost::instantiate(
        engine,
        flowrunner,
        mismatch_plugin,
        mismatch_vault,
        Arc::new(wamn_runtime::plugins::wamn_logging::WamnLogging::from_env()?),
        ExecutionIdentity {
            owner: "nodeinvoke-mismatch",
            tenant: TENANT,
            schema: Some(SCHEMA),
            project: PROJECT,
        },
        production_capabilities(allowed.clone(), Arc::new(RunnerEgressPolicy::default()))
            .with_node_placements(placements),
        30_000,
    )
    .await?;
    let grants_before_mismatch = serve.grant_install_count();
    let mismatch_result = mismatch_worker.drain().await;
    let mismatch_error = mismatch_result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    check(
        &mut ok,
        "AUTHN-MISMATCH-INFRASTRUCTURE: the real wrong-key hop is a typed outer execution refusal",
        mismatch_result.is_err() && mismatch_error.contains("SigningRefused"),
    );
    let mrow = seed_conn
        .query_one(
            &format!(
                "SELECT r.status, r.fail_kind, r.fail_node, n.status, n.error_kind, q.lease_owner \
                   FROM {SCHEMA}.runs AS r \
                   LEFT JOIN {SCHEMA}.node_runs AS n \
                     ON n.tenant_id=r.tenant_id AND n.run_id=r.run_id AND n.node_id='call' \
                   LEFT JOIN {SCHEMA}.run_queue AS q \
                     ON q.tenant_id=r.tenant_id AND q.run_id=r.run_id \
                  WHERE r.run_id = 'ni-mismatch'"
            ),
            &[],
        )
        .await?;
    let mstatus: String = mrow.get(0);
    let mkind: Option<String> = mrow.get(1);
    let mnode: Option<String> = mrow.get(2);
    let mattempt_status: Option<String> = mrow.get(3);
    let mattempt_error: Option<String> = mrow.get(4);
    let mlease_owner: Option<String> = mrow.get(5);
    check(
        &mut ok,
        "AUTHN-MISMATCH-PLANE: signing refusal did not create a node failure verdict",
        mstatus == "running"
            && mkind.is_none()
            && mnode.is_none()
            && mattempt_status.as_deref() == Some("started")
            && mattempt_error.is_none(),
    );
    check(
        &mut ok,
        "AUTHN-MISMATCH-RECOVERY: the queue lease remains owned for infrastructure recovery",
        mlease_owner.as_deref() == Some("nodeinvoke-mismatch"),
    );
    check(
        &mut ok,
        "AUTHN-MISMATCH-VERIFY-BEFORE-GRANT: the wrong-key host installed no node grant",
        serve.grant_install_count() == grants_before_mismatch,
    );
    println!(
        "  authn mismatch: error={mismatch_error:?} status={mstatus} fail_kind={mkind:?} \
         fail_node={mnode:?} attempt_status={mattempt_status:?} lease_owner={mlease_owner:?}"
    );

    Ok(ok)
}

/// The exact envelope the trusted host's custom-node hop POSTs for this flow —
/// the substrate the raw authn checks sign, tamper, and mis-key.
fn canonical_request() -> NodeInvokeRequest {
    NodeInvokeRequest {
        ctx: WireRunContext {
            run_id: "authn-raw".into(),
            flow_id: FLOW_ID.into(),
            flow_version: 1,
            node_id: "call".into(),
            attempt: 0,
            idempotency_key: "authn-raw:call".into(),
            deadline_ms: Some(30_000),
            traceparent: None,
            tracestate: None,
            config: r#"{"probe":"granted","forbidden":"sibling"}"#.into(),
            context: "{}".into(),
        },
        input: WirePayload::Inline("\"hello\"".into()),
        grant: granted_credentials(Some("granted")),
    }
}

/// The canonical envelope with an attacker-chosen input — the "forged input"
/// tamper the signature must catch.
fn tampered_request() -> NodeInvokeRequest {
    let mut r = canonical_request();
    r.input = WirePayload::Inline("\"attacker-chosen-input\"".into());
    r
}

/// POST a raw `/run` body to the loopback serve-node with an OPTIONAL
/// `x-wamn-signature`, returning (status-code, full-response-text). Half-closes
/// the write side so the server's keep-alive read EOFs and the response drains.
async fn raw_post(port: u16, body: &str, signature: Option<&str>) -> anyhow::Result<(u16, String)> {
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let mut req = format!(
        "POST /run HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(sig) = signature {
        req.push_str(&format!("{SIGNATURE_HEADER}: {sig}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    sock.write_all(req.as_bytes()).await?;
    sock.flush().await?;
    sock.shutdown().await?; // half-close: the server's next read EOFs cleanly
    let mut resp = Vec::new();
    sock.read_to_end(&mut resp).await?;
    let text = String::from_utf8_lossy(&resp).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    Ok((status, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_drift::{Need, assert_stand_in};

    #[test]
    fn nodeinvoke_flow_uses_the_current_portable_schema() {
        admitted_artifact(b"nodeinvoke-test-component")
            .expect("nodeinvoke release graph and resolved interfaces validate");
    }

    /// wamn-9mg8 [GATE-DRIFT]: nodeinvoke's `run_queue` stand-in vs the schema of
    /// record, through the uniform guard. nodeinvoke drives the real runner over
    /// the per-partition claim path (`partition_owner` + the `run_queue_partition`
    /// index) and a guest that can settle terminally (`run_dead_letters`,
    /// wamn-v8cv), so all three tables are Required.
    #[test]
    fn nodeinvoke_stand_in_tracks_run_queue_schema_of_record() {
        let ddl = runner_ddl("wamn_run");
        assert_stand_in(
            "nodeinvoke",
            &ddl,
            &[
                ("run_queue", Need::Required),
                ("partition_owner", Need::Required),
                ("run_dead_letters", Need::Required),
            ],
        );
        assert!(ddl.contains("lease_generation bigint NOT NULL DEFAULT 0"));
    }
}
