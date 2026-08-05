//! f3proof — the POC F3 `escalate-stale-holds` end-to-end proof (wamn-24i).
//!
//! F3 is the nightly cron escalation flow: query quality holds open past 48h →
//! mark them `escalated` → notify the manager over a CREDENTIALED webhook, under
//! a fail-closed egress allowlist, in a project that idles to zero between
//! nights. This gate proves the whole chain on the LIVE runner + `wamn:postgres`
//! plugin + vault + egress guard, exercising the pieces F3 exists to validate:
//!
//! * **time-shift + structural cycle** — the flow computes `cutoff = scheduled-at
//!   − 48h` (seconds-scale here, virtual time) with the `time-shift` node JMESPath
//!   cannot express, lists the stale open holds ONCE, and drains them through a
//!   `conditional`/`transform` cycle (`gate → advance → gate`), `escalate`/`notify`
//!   on a dead-end branch. The proof asserts BOTH stale holds end `escalated`, the
//!   FRESH hold is untouched, and the cycle ran once per hold plus the empty tail
//!   (the `gate` node's per-visit occurrences — R24).
//! * **credential vault (5.9)** — each `notify` targets serve-echo, which reflects
//!   a one-way FNV-1a digest of the `authorization` header it received. The proof
//!   matches every notify's recorded digest against `fnv1a(secret)` (delivery) and
//!   scans every recorded row for the raw secret (containment) — the credproof
//!   pattern, once per escalated hold.
//! * **portable HTTP connection** — the artifact declares one manager-notification
//!   requirement; the gate binds it to an environment-owned serve-echo generation
//!   and credential handle. Completing proves the trusted adapter admitted that
//!   exact binding and target.
//!
//! Two modes, one preamble (provision the holds catalog + table + seed 2 stale +
//! 1 fresh + register the gate flow):
//!   * LOCAL (default): seed a cron-shaped run directly; a separately-started
//!     run-worker (its vault from a credentials file, its host allowlist admitting
//!     the local echo) drains it. The `--setup` self-contained path.
//!   * IN-CLUSTER (`--deployment`): PARK the runner to 0 (scale-to-zero proof),
//!     let the LIVE dispatcher fire the registered CRON flow, the waker wake it
//!     0→1, and the runner drain — then assert, teardown, and restore scale
//!     floored at 1 (the wakeproof shape).

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};

use wamn_catalog::{Artifact, NodeImplementation};
use wamn_gate_harness::{check, seed_flow_version};
use wamn_node_manifest::{ConnectionTypeDescriptor, PortableConnectionRequirement};

use wamn_test_fixtures::runner::{
    connect_app, fnv1a_64, ladder_ddl, poll_to_terminal, seed_run, valid_ident,
};
use wamn_test_infrastructure::kubernetes::{DeploymentScale, KubeScale};

const FLOW_ID: &str = "escalate-stale-holds";
const TENANT_DEFAULT: &str = "demo-tenant";
const CATALOG_ID: &str = "f3proof";
const ENVIRONMENT: &str = "gate";
const SOURCE_ID: &str = "f3proof-schedule";
const ATTACHMENT_ID: &str = "f3proof-cron";
const SOURCE_HASH: &str = "sha256:f3proof-schedule";
const DEFINITION_HASH: &str = "sha256:f3proof-cron";
const CONNECTION_NAME: &str = "manager-notifications";
const CONNECTION_INSTANCE_ID: &str = "f3proof-manager-notifications";
const CONNECTION_CREDENTIAL_HANDLE: &str = "notify-webhook";
const CONNECTION_GENERATION_HASH: &str = "sha256:f3proof-manager-notifications-generation-v1";
const CONNECTION_BINDING_HASH: &str = "sha256:f3proof-manager-notifications-binding-v1";

const REGISTER_CATALOG_SQL: &str = "INSERT INTO catalog.catalogs \
   (tenant_id,catalog_id,version,environment,schema_version,state) \
 VALUES ($1,$2,$3,$4,'0.1','staged') \
 ON CONFLICT (tenant_id,catalog_id,version) DO NOTHING";
const REGISTER_ARTIFACT_SQL: &str = "SELECT catalog.register_flow_artifact( \
   $1,$2,$3,'0.1',$4::text::jsonb,$5,$6,$7,$8,$9,$10,$11)";
const REGISTER_MANIFEST_SQL: &str = "SELECT catalog.register_release_manifest($1,$2,$3,$4)";
const REGISTER_FLOW_SQL: &str = "INSERT INTO catalog.release_flows \
   (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
 VALUES ($1,$2,$3,$4,$3) ON CONFLICT DO NOTHING";
const REGISTER_EXPOSURE_SQL: &str =
    "SELECT catalog.register_release_exposure_manifest($1,$2,$3,'{}')";
const REGISTER_SOURCE_SQL: &str = "INSERT INTO catalog.release_sources \
   (tenant_id,catalog_id,catalog_version,source_id,source_kind,definition_json,source_hash) \
 VALUES ($1,$2,$3,$4,'schedule',$5,$6) ON CONFLICT DO NOTHING";
const REGISTER_ATTACHMENT_SQL: &str = "INSERT INTO catalog.release_attachments \
   (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id, \
    source_id,definition_hash,definition_json) \
 VALUES ($1,$2,$3,$4,'cron',$5,$6,$7,$8) ON CONFLICT DO NOTHING";
const UPSERT_HEAD_SQL: &str = "INSERT INTO catalog.catalog_heads \
   (tenant_id,catalog_id,environment,applied_catalog_version) \
 VALUES ($1,$2,$3,$4) \
 ON CONFLICT (tenant_id,catalog_id,environment) DO UPDATE \
 SET applied_catalog_version=EXCLUDED.applied_catalog_version,updated_at=now()";
const UPSERT_ACTIVATION_SQL: &str = "INSERT INTO catalog.attachment_activation \
   (tenant_id,catalog_id,environment,attachment_id,confirmed_definition_hash,enabled) \
 VALUES ($1,$2,$3,$4,$5,true) \
 ON CONFLICT (tenant_id,catalog_id,environment,attachment_id) DO UPDATE \
 SET confirmed_definition_hash=EXCLUDED.confirmed_definition_hash,enabled=true";
const DELETE_ACTIVATION_SQL: &str = "DELETE FROM catalog.attachment_activation \
 WHERE tenant_id=$1 AND catalog_id=$2 AND environment=$3 AND attachment_id=$4";
const DELETE_HEAD_SQL: &str = "DELETE FROM catalog.catalog_heads \
 WHERE tenant_id=$1 AND catalog_id=$2 AND environment=$3 AND applied_catalog_version=$4";

/// The demo secret the runner's credentials file maps `notify-webhook` to — the
/// value the delivery assert expects reflected and the containment scan hunts.
/// Distinct from credproof's so a shared substrate can carry both.
pub const DEMO_SECRET: &str = "wamn-f3-proof-1c4e77a90b2d5f83";

#[derive(Debug, Args)]
pub struct F3ProofArgs {
    /// App (wamn_app, NOSUPERUSER) Postgres URL. Overrides WAMN_PG_URL / DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL — required for --setup / --teardown.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The schema the deployed runner claims from (matches the runner's --schema).
    #[arg(long, default_value = "wamn_runner_demo")]
    pub schema: String,

    /// The tenant the seeded holds + the runner share (matches --tenant).
    #[arg(long, default_value = TENANT_DEFAULT)]
    pub tenant: String,

    /// The serve-echo authority used by the gate's environment-owned connection
    /// generation. In-cluster the Service `serve-echo:8091`; locally a
    /// `wamn-gates serve-echo` port like `127.0.0.1:8097`.
    #[arg(long, default_value = "serve-echo:8091")]
    pub echo_host: String,

    /// The secret the runner's credentials file maps `notify-webhook` to.
    #[arg(long, default_value = DEMO_SECRET)]
    pub secret: String,

    /// The `time-shift` offset (ms, signed): `cutoff = fire-at-ms + offset`. The
    /// 48h wall-clock maps to a seconds-scale offset under the gate's virtual
    /// time (default −60s). Stale holds are seeded 1h old, the fresh one now, so
    /// any offset between a second and an hour separates them.
    #[arg(long, default_value_t = -60_000)]
    pub offset_ms: i64,

    /// Immutable flow version for this exact graph. Changed graph variants must
    /// publish a distinct version rather than rewriting catalog history.
    #[arg(long, default_value_t = 1)]
    pub flow_version: u32,

    /// IN-CLUSTER: the runner Deployment to park→0 and wake. Present ⇒ the
    /// park→dispatcher-fires→wake path; absent ⇒ the LOCAL directly-seeded path.
    #[arg(long)]
    pub deployment: Option<String>,

    /// Provision schema + catalog + holds + register the gate flow (admin+app).
    #[arg(long)]
    pub setup: bool,

    /// Drop the schema at the end (admin) — LOCAL cleanup.
    #[arg(long)]
    pub teardown: bool,

    /// How long to wait for the run to reach a terminal status.
    #[arg(long, default_value_t = 90)]
    pub timeout_secs: u64,
}

/// The gate flow: the committed F3 shape (`cron → time-shift → list → gate →
/// {escalate → notify (dead-end), advance → gate}`), with its portable manager
/// notification connection and a seconds-scale offset.
fn gate_flow_json(_echo_host: &str, offset_ms: i64, flow_version: u32) -> String {
    let connection_requirement = serde_json::to_string(
        &PortableConnectionRequirement::never_replay(ConnectionTypeDescriptor::http_v1()),
    )
    .expect("HTTP connection requirement serializes");
    format!(
        r#"{{
  "schema-version": "0.1",
  "flow-id": "{FLOW_ID}",
  "version": {flow_version},
  "name": "F3 escalate-stale-holds (gate)",
  "connection-requirements": [
    {{ "name": "manager-notifications", "requirement": {connection_requirement} }}
  ],
  "nodes": [
    {{ "id": "cron", "type": "cron" }},
    {{ "id": "shift", "type": "time-shift",
       "config": {{ "base": "\"scheduled-at\"", "offset-ms": {offset_ms}, "format": "iso", "key": "cutoff" }} }},
    {{ "id": "list-stale", "type": "postgres",
       "config": {{ "entity": "quality_holds", "op": "list",
                    "filters": {{ "status": "eq.open", "opened_at": "lt.{{{{cutoff}}}}" }},
                    "sort": "opened_at", "limit": 500 }} }},
    {{ "id": "gate", "type": "conditional", "config": {{ "expression": "length(@) > `0`" }} }},
    {{ "id": "escalate", "type": "postgres",
       "config": {{ "entity": "quality_holds", "op": "update", "id": "[0].id", "body": "{{status: 'escalated'}}" }} }},
    {{ "id": "notify", "type": "http-request", "connection": "manager-notifications",
       "config": {{ "method": "POST", "path-and-query": "/holds",
                    "body": "{{hold: id, status: status, opened_at: opened_at}}" }} }},
    {{ "id": "advance", "type": "transform", "config": {{ "expression": "[1:]" }} }}
  ],
  "edges": [
    {{ "from": "cron", "to": "shift" }},
    {{ "from": "shift", "to": "list-stale" }},
    {{ "from": "list-stale", "to": "gate" }},
    {{ "from": "gate", "from-port": "true", "to": "escalate" }},
    {{ "from": "gate", "from-port": "true", "to": "advance" }},
    {{ "from": "escalate", "to": "notify" }},
    {{ "from": "advance", "to": "gate" }}
  ]
}}"#
    )
}

/// The minimal quality_holds catalog the `postgres` node compiles against —
/// `status` (enum) + `opened_at` (timestamptz); `id`/`tenant_id` are managed.
fn holds_catalog_json() -> String {
    json!({
        "schema-version": "0.1",
        "catalog-id": "poc-f3",
        "version": 1,
        "entities": [
            { "id": "quality_holds", "name": "quality_holds", "fields": [
                { "id": "status", "name": "status",
                  "type": { "kind": "enum", "variants": ["open", "disposed", "escalated"] } },
                { "id": "opened_at", "name": "opened_at", "type": { "kind": "timestamptz" } }
            ]}
        ]
    })
    .to_string()
}

/// The entity table the node reads/writes, under the tenant RLS floor (the 3.2
/// pattern: `id` uuid pk + `tenant_id` + the declared fields).
fn holds_ddl(schema: &str) -> String {
    format!(
        "DROP TABLE IF EXISTS {schema}.quality_holds CASCADE; \
         DROP TABLE IF EXISTS {schema}.wamn_catalog CASCADE; \
         CREATE TABLE {schema}.quality_holds ( \
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
           tenant_id text NOT NULL, \
           status text NOT NULL DEFAULT 'open' CHECK (status IN ('open','disposed','escalated')), \
           opened_at timestamptz NOT NULL DEFAULT now()); \
         ALTER TABLE {schema}.quality_holds ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {schema}.quality_holds FORCE ROW LEVEL SECURITY; \
         CREATE POLICY quality_holds_tenant ON {schema}.quality_holds \
           USING (tenant_id = current_setting('app.tenant', true)) \
           WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
         GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.quality_holds TO wamn_app; \
         CREATE TABLE {schema}.wamn_catalog ( \
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
           tenant_id text NOT NULL, document jsonb NOT NULL); \
         ALTER TABLE {schema}.wamn_catalog ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {schema}.wamn_catalog FORCE ROW LEVEL SECURITY; \
         CREATE POLICY wamn_catalog_tenant ON {schema}.wamn_catalog \
           USING (tenant_id = current_setting('app.tenant', true)) \
           WITH CHECK (tenant_id = current_setting('app.tenant', true)); \
         GRANT SELECT ON {schema}.wamn_catalog TO wamn_app;"
    )
}

fn f3_implementations() -> anyhow::Result<Vec<NodeImplementation>> {
    let mut implementations = [
        "cron",
        "time-shift",
        "postgres",
        "conditional",
        "http-request",
        "transform",
    ]
    .into_iter()
    .map(|node_type| {
        let descriptor = wamn_standard_nodes::describe(node_type)
            .with_context(|| format!("missing standard-node descriptor for {node_type}"))?;
        let contract =
            wamn_standard_nodes::resolve_descriptor(descriptor).map_err(anyhow::Error::new)?;
        NodeImplementation::from_resolved_platform_contract(contract).map_err(anyhow::Error::new)
    })
    .collect::<anyhow::Result<Vec<_>>>()?;
    implementations
        .sort_by(|left, right| left.interface().node_type.cmp(&right.interface().node_type));
    Ok(implementations)
}

fn f3_artifact(tenant: &str, graph: &str) -> anyhow::Result<(wamn_flow::Flow, Artifact)> {
    let flow = wamn_flow::Flow::from_json(graph)
        .map_err(|error| anyhow::anyhow!("parse F3 release graph: {error}"))?;
    let artifact = Artifact::new(tenant, &flow, f3_implementations()?)
        .map_err(|error| anyhow::anyhow!("build F3 release artifact: {error}"))?;
    Ok((flow, artifact))
}

async fn seed_authoritative_cron_release(
    admin: &mut Client,
    tenant: &str,
    graph: &str,
    echo_host: &str,
    flow_version: u32,
) -> anyhow::Result<()> {
    let flow_version = i32::try_from(flow_version).context("F3 flow version exceeds i32")?;
    let (flow, artifact) = f3_artifact(tenant, graph)?;
    let canonical_graph =
        String::from_utf8(flow.canonical_bytes()).expect("canonical F3 graph is UTF-8");
    let interfaces = String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec())
        .expect("canonical F3 interfaces are UTF-8");
    let components = serde_json::to_value(artifact.supplied_components())?;
    let occurrence_recovery = String::from_utf8(artifact.occurrence_recovery_bytes().to_vec())
        .expect("canonical F3 occurrence recovery is UTF-8");
    let artifact_hash = artifact.identity().artifact_hash().as_str();
    let connection = flow
        .connection_requirements
        .iter()
        .find(|connection| connection.name == CONNECTION_NAME)
        .context("F3 manager notification connection requirement")?;
    let requirement_json = serde_json::to_string(&connection.requirement)?;
    let requirement_hash = wamn_schema_control::connections::ArtifactConnectionRequirement::new(
        artifact_hash,
        CONNECTION_NAME,
        connection.requirement.clone(),
    )
    .requirement_hash();
    let connection_definition = serde_json::to_string(&json!({
        "primary-authority": format!("http://{}/", echo_host.trim_end_matches('/')),
        "failover-authorities": [],
        "tls-verification": "disabled",
        "tls-names": [],
        "redirect-policy": "same-authority",
        "proxy-transport": null,
        "credential-set-handle": CONNECTION_CREDENTIAL_HANDLE,
    }))?;
    let members = json!([{
        "flow-id": FLOW_ID,
        "flow-version": flow_version,
        "artifact-hash": artifact_hash,
    }]);
    let source = json!({
        "schedule": "* * * * * *",
        "timezone": "UTC",
        "catch-up": "skip",
    });
    let attachment = json!({
        "id": ATTACHMENT_ID,
        "kind": "cron",
        "flow-id": FLOW_ID,
        "source-id": SOURCE_ID,
        "run-deadline-ms": 120_000,
    });

    let transaction = admin.transaction().await?;
    transaction
        .execute(
            REGISTER_CATALOG_SQL,
            &[&tenant, &CATALOG_ID, &flow_version, &ENVIRONMENT],
        )
        .await?;
    transaction
        .execute(
            REGISTER_ARTIFACT_SQL,
            &[
                &tenant,
                &FLOW_ID,
                &flow_version,
                &canonical_graph,
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
    transaction
        .execute(
            wamn_schema_control::connections::insert_connection_requirement_sql(),
            &[
                &tenant,
                &artifact_hash,
                &CONNECTION_NAME,
                &requirement_json,
                &requirement_hash,
            ],
        )
        .await?;
    transaction
        .execute(
            REGISTER_MANIFEST_SQL,
            &[&tenant, &CATALOG_ID, &flow_version, &members],
        )
        .await?;
    transaction
        .execute(
            REGISTER_FLOW_SQL,
            &[&tenant, &CATALOG_ID, &flow_version, &FLOW_ID],
        )
        .await?;
    transaction
        .execute(
            REGISTER_EXPOSURE_SQL,
            &[&tenant, &CATALOG_ID, &flow_version],
        )
        .await?;
    transaction
        .execute(
            REGISTER_SOURCE_SQL,
            &[
                &tenant,
                &CATALOG_ID,
                &flow_version,
                &SOURCE_ID,
                &source,
                &SOURCE_HASH,
            ],
        )
        .await?;
    transaction
        .execute(
            REGISTER_ATTACHMENT_SQL,
            &[
                &tenant,
                &CATALOG_ID,
                &flow_version,
                &ATTACHMENT_ID,
                &FLOW_ID,
                &SOURCE_ID,
                &DEFINITION_HASH,
                &attachment,
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.connection_instances \
               (tenant_id,environment,instance_id,requirement_type,contract) \
             VALUES ($1,$2,$3,'http','wamn:connection/http@0.1.0') \
             ON CONFLICT (tenant_id,environment,instance_id) DO NOTHING",
            &[&tenant, &ENVIRONMENT, &CONNECTION_INSTANCE_ID],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.connection_generations \
               (tenant_id,environment,instance_id,generation,definition_json, \
                definition_hash,credential_set_handle) \
             VALUES ($1,$2,$3,1,$4::text::jsonb,$5,$6) \
             ON CONFLICT (tenant_id,environment,instance_id,generation) DO NOTHING",
            &[
                &tenant,
                &ENVIRONMENT,
                &CONNECTION_INSTANCE_ID,
                &connection_definition,
                &CONNECTION_GENERATION_HASH,
                &CONNECTION_CREDENTIAL_HANDLE,
            ],
        )
        .await?;
    let generation_matches: bool = transaction
        .query_one(
            "SELECT definition_json=$4::text::jsonb AND definition_hash=$5 \
                    AND credential_set_handle=$6 \
               FROM catalog.connection_generations \
              WHERE tenant_id=$1 AND environment=$2 AND instance_id=$3 AND generation=1",
            &[
                &tenant,
                &ENVIRONMENT,
                &CONNECTION_INSTANCE_ID,
                &connection_definition,
                &CONNECTION_GENERATION_HASH,
                &CONNECTION_CREDENTIAL_HANDLE,
            ],
        )
        .await?
        .get(0);
    if !generation_matches {
        bail!("existing F3 connection generation differs from the requested target");
    }
    transaction
        .execute(
            "UPDATE catalog.connection_instances \
                SET lifecycle_status='enabled',active_generation=1,revision=revision+1, \
                    updated_at=GREATEST(clock_timestamp(),updated_at+interval '1 microsecond') \
              WHERE tenant_id=$1 AND environment=$2 AND instance_id=$3 \
                AND (lifecycle_status<>'enabled' OR active_generation IS DISTINCT FROM 1)",
            &[&tenant, &ENVIRONMENT, &CONNECTION_INSTANCE_ID],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.connection_bindings \
               (tenant_id,catalog_id,catalog_version,artifact_hash,requirement_name, \
                environment,instance_id,binding_status,validation_status,validation_hash) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,'active','valid',$8) \
             ON CONFLICT (tenant_id,catalog_id,catalog_version,artifact_hash,requirement_name) \
             DO NOTHING",
            &[
                &tenant,
                &CATALOG_ID,
                &flow_version,
                &artifact_hash,
                &CONNECTION_NAME,
                &ENVIRONMENT,
                &CONNECTION_INSTANCE_ID,
                &CONNECTION_BINDING_HASH,
            ],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.catalogs SET state='superseded' \
             WHERE tenant_id=$1 AND catalog_id=$2 AND environment=$3 \
               AND version<>$4 AND state='applied'",
            &[&tenant, &CATALOG_ID, &ENVIRONMENT, &flow_version],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.catalogs SET state='applied' \
             WHERE tenant_id=$1 AND catalog_id=$2 AND version=$3",
            &[&tenant, &CATALOG_ID, &flow_version],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn activate_authoritative_cron_release(
    admin: &mut Client,
    tenant: &str,
    flow_version: u32,
) -> anyhow::Result<()> {
    let flow_version = i32::try_from(flow_version).context("F3 flow version exceeds i32")?;
    let transaction = admin.transaction().await?;
    transaction
        .execute(
            UPSERT_HEAD_SQL,
            &[&tenant, &CATALOG_ID, &ENVIRONMENT, &flow_version],
        )
        .await?;
    transaction
        .execute(
            UPSERT_ACTIVATION_SQL,
            &[
                &tenant,
                &CATALOG_ID,
                &ENVIRONMENT,
                &ATTACHMENT_ID,
                &DEFINITION_HASH,
            ],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn deactivate_authoritative_cron_release(
    admin: &mut Client,
    tenant: &str,
    flow_version: u32,
) -> anyhow::Result<()> {
    let flow_version = i32::try_from(flow_version).context("F3 flow version exceeds i32")?;
    let transaction = admin.transaction().await?;
    transaction
        .execute(
            DELETE_ACTIVATION_SQL,
            &[&tenant, &CATALOG_ID, &ENVIRONMENT, &ATTACHMENT_ID],
        )
        .await?;
    transaction
        .execute(
            DELETE_HEAD_SQL,
            &[&tenant, &CATALOG_ID, &ENVIRONMENT, &flow_version],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn clear_in_cluster_runtime(
    admin: &Client,
    schema: &str,
    tenant: &str,
) -> anyhow::Result<()> {
    admin
        .execute(
            &format!("DELETE FROM {schema}.cron_anchor WHERE tenant_id=$1 AND flow_id=$2"),
            &[&tenant, &FLOW_ID],
        )
        .await?;
    admin
        .batch_execute(&format!(
            "DELETE FROM {schema}.node_runs WHERE run_id IN \
               (SELECT run_id FROM {schema}.runs WHERE flow_id = '{FLOW_ID}'); \
             DELETE FROM {schema}.run_queue WHERE run_id IN \
               (SELECT run_id FROM {schema}.runs WHERE flow_id = '{FLOW_ID}'); \
             DELETE FROM {schema}.runs WHERE flow_id = '{FLOW_ID}'; \
             DELETE FROM {schema}.flows WHERE flow_id = '{FLOW_ID}';"
        ))
        .await?;
    Ok(())
}

/// Provision the runner tables + the holds catalog/table (superuser), then
/// register the gate flow + seed 2 stale + 1 fresh hold (app, under the claim).
async fn setup(args: &F3ProofArgs, admin_url: &str, app_url: &str) -> anyhow::Result<()> {
    let schema = &args.schema;
    let (mut admin, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for --setup")?;
    let conn_task = tokio::spawn(conn);
    // LOCAL (no --deployment) provisions a FRESH throwaway schema + the runner
    // tables. IN-CLUSTER adds only the (idempotent) holds/catalog tables to the
    // runner's EXISTING wamn_runner_demo — never dropping the live run-state.
    let fresh_schema = args.deployment.is_none();
    let result = async {
        if fresh_schema {
            admin
                .batch_execute(
                    "DO $$ BEGIN \
                       IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                         CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
                       END IF; \
                     END $$;",
                )
                .await
                .context("ensure wamn_app role")?;
            admin
                .batch_execute(&format!(
                    "DROP SCHEMA IF EXISTS {schema} CASCADE; \
                     CREATE SCHEMA {schema} AUTHORIZATION postgres; \
                     GRANT USAGE ON SCHEMA {schema} TO wamn_app;"
                ))
                .await
                .context("create ephemeral schema")?;
            admin
                .batch_execute(&ladder_ddl(schema))
                .await
                .context("apply runner-table DDL")?;
        }
        if !fresh_schema {
            deactivate_authoritative_cron_release(
                &mut admin,
                &args.tenant,
                args.flow_version,
            )
            .await
            .context("deactivate any prior F3 cron release")?;
            clear_in_cluster_runtime(&admin, schema, &args.tenant)
                .await
                .context("clear any prior F3 runtime state")?;
        }
        admin
            .batch_execute(&holds_ddl(schema))
            .await
            .context("apply holds + catalog DDL")?;
        // The catalog snapshot the node resolves through `catalog_json` — written
        // by the SUPERUSER (bypasses RLS), like the f1bench precedent; `wamn_app`
        // holds only SELECT on wamn_catalog (it is read-only for the runtime).
        admin
            .execute(
                &format!(
                    "INSERT INTO {schema}.wamn_catalog (tenant_id, document) VALUES ($1, $2::text::jsonb)"
                ),
                &[&args.tenant, &holds_catalog_json()],
            )
            .await
            .context("write catalog snapshot")?;
        if !fresh_schema {
            let graph = gate_flow_json(&args.echo_host, args.offset_ms, args.flow_version);
            seed_authoritative_cron_release(
                &mut admin,
                &args.tenant,
                &graph,
                &args.echo_host,
                args.flow_version,
            )
            .await
            .context("register authoritative F3 cron release")?;
        }
        anyhow::Ok(())
    }
    .await;
    drop(admin);
    let _ = conn_task.await;
    result?;

    let app = connect_app(app_url, schema, &args.tenant).await?;
    seed_holds(&app, &args.tenant).await?;
    if fresh_schema {
        seed_flow_version(
            &app,
            &args.tenant,
            FLOW_ID,
            1,
            true,
            &gate_flow_json(&args.echo_host, args.offset_ms, 1),
            true,
        )
        .await
        .context("register the local gate flow")?;
    }
    Ok(())
}

/// Seed the discriminating hold set:
///   * 2 STALE OPEN holds (opened 1h ago) — the drain MUST escalate exactly these;
///   * 1 FRESH OPEN hold (opened now) — newer than the seconds-scale cutoff, so
///     the `opened_at < cutoff` filter must leave it open;
///   * 1 STALE DISPOSED hold (opened 1h ago, already `disposed`) — old enough to
///     match the cutoff, so ONLY the `status = open` predicate keeps it out. A
///     list filter that dropped `status = open` would escalate + notify it too,
///     breaking the "2 escalated / 2 notifies / disposed untouched" asserts
///     (mutant ii).
async fn seed_holds(app: &Client, tenant: &str) -> anyhow::Result<()> {
    app.execute(
        "INSERT INTO quality_holds (tenant_id, status, opened_at) VALUES \
           ($1, 'open', now() - interval '1 hour'), \
           ($1, 'open', now() - interval '1 hour'), \
           ($1, 'open', now()), \
           ($1, 'disposed', now() - interval '1 hour')",
        &[&tenant],
    )
    .await
    .context("seed holds")?;
    Ok(())
}

/// Zero-residue teardown. LOCAL drops the throwaway schema; IN-CLUSTER removes
/// only what the gate added to the live runner schema — the holds/catalog tables
/// and the gate flow's runtime rows — leaving the runner's own state.
async fn teardown(
    admin_url: &str,
    schema: &str,
    tenant: &str,
    flow_version: u32,
    fresh_schema: bool,
) -> anyhow::Result<()> {
    let (admin, conn) = tokio_postgres::connect(admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let r = async {
        if fresh_schema {
            admin
                .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))
                .await?;
            return anyhow::Ok(());
        }
        let flow_version = i32::try_from(flow_version).context("F3 flow version exceeds i32")?;
        clear_in_cluster_runtime(&admin, schema, tenant).await?;
        admin
            .batch_execute(&format!(
                "DROP TABLE IF EXISTS {schema}.quality_holds CASCADE; \
                 DROP TABLE IF EXISTS {schema}.wamn_catalog CASCADE;"
            ))
            .await?;
        admin
            .execute(
                DELETE_ACTIVATION_SQL,
                &[&tenant, &CATALOG_ID, &ENVIRONMENT, &ATTACHMENT_ID],
            )
            .await?;
        admin
            .execute(
                DELETE_HEAD_SQL,
                &[&tenant, &CATALOG_ID, &ENVIRONMENT, &flow_version],
            )
            .await?;
        admin
            .execute(
                "UPDATE catalog.catalogs SET state='superseded' \
                 WHERE tenant_id=$1 AND catalog_id=$2 AND version=$3 AND state='applied'",
                &[&tenant, &CATALOG_ID, &flow_version],
            )
            .await?;
        admin
            .execute(
                "UPDATE catalog.connection_instances \
                    SET lifecycle_status='disabled',active_generation=NULL,revision=revision+1, \
                        updated_at=GREATEST(clock_timestamp(),updated_at+interval '1 microsecond') \
                  WHERE tenant_id=$1 AND environment=$2 AND instance_id=$3 \
                    AND (lifecycle_status<>'disabled' OR active_generation IS NOT NULL)",
                &[&tenant, &ENVIRONMENT, &CONNECTION_INSTANCE_ID],
            )
            .await?;
        Ok(())
    }
    .await
    .map_err(|error: anyhow::Error| anyhow::anyhow!("teardown: {error}"));
    drop(admin);
    let _ = conn_task.await;
    r.map(|_| ())
}

pub async fn run(args: F3ProofArgs) -> anyhow::Result<()> {
    if !valid_ident(&args.schema) {
        bail!("invalid schema {:?}", args.schema);
    }
    if args.flow_version == 0 {
        bail!("--flow-version must be positive");
    }
    if args.deployment.is_none() && args.flow_version != 1 {
        bail!("local f3proof supports only --flow-version 1");
    }
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;

    println!(
        "# wamn-gates f3proof — cron escalation + vault + egress (schema {}, tenant {}, echo {}, offset {}ms)",
        args.schema, args.tenant, args.echo_host, args.offset_ms
    );

    if args.setup {
        let admin_url = args
            .admin_database_url
            .clone()
            .context("--setup needs a superuser url: --admin-database-url / WAMN_PG_ADMIN_URL")?;
        setup(&args, &admin_url, &app_url)
            .await
            .context("setup: provision schema + catalog + holds + register flow")?;
        println!("## setup — schema + catalog + 2 stale/1 fresh holds + gate flow (registered)");
    }

    let mut client = connect_app(&app_url, &args.schema, &args.tenant).await?;

    // Park-and-wake (in-cluster) or direct-seed (local) — either way the LIVE
    // runner drains the run, and the assertions read the same DB outcome.
    let (run_id, mut ok, scale_restore) = if let Some(deployment) = args.deployment.clone() {
        drive_in_cluster(&client, &args, &deployment).await?
    } else {
        (drive_local(&mut client, &args).await?, true, None)
    };

    ok &= assert_f3(&client, &run_id, &args.secret).await?;

    // Restore scale floored at 1 (in-cluster only).
    if let (Some(scale), Some(deployment)) = (scale_restore, args.deployment.clone()) {
        let kube = KubeScale::in_cluster()?;
        kube.set_replicas(&deployment, scale).await?;
        println!("## restore — {deployment} scaled back to {scale}");
    }

    if args.teardown
        && let Some(admin_url) = args.admin_database_url.clone()
    {
        teardown(
            &admin_url,
            &args.schema,
            &args.tenant,
            args.flow_version,
            args.deployment.is_none(),
        )
        .await?;
    }

    println!("\nf3proof complete — overall PASS: {ok}");
    if !ok {
        bail!("f3proof failed");
    }
    Ok(())
}

/// LOCAL: seed a normative cron input directly and let the separately-started
/// run-worker drain it. `scheduled-at` = now, so the seconds-scale cutoff lands
/// between the stale (1h old) and fresh (now) holds.
async fn drive_local(client: &mut Client, args: &F3ProofArgs) -> anyhow::Result<String> {
    let now = SystemTime::now();
    let now_ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let scheduled_at = chrono::DateTime::<chrono::Utc>::from(now).to_rfc3339();
    let input = json!({
        "scheduled-at": scheduled_at,
        "fired-at": scheduled_at,
    });
    let run_id = format!("f3-{now_ms}");
    seed_run(client, FLOW_ID, &run_id, &serde_json::to_string(&input)?).await?;
    println!(
        "## seed — cron-shaped run {run_id} (scheduled-at {scheduled_at}); awaiting the runner"
    );
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let status = poll_to_terminal(client, &run_id, deadline).await?;
    println!("## drained — run reached {status}");
    Ok(run_id)
}

/// IN-CLUSTER: park the runner to 0 (scale-to-zero proof), let the LIVE
/// dispatcher fire the registered CRON flow (a DISTINCT phase — isolating a
/// projects-config failure from a wake failure, the wakeproof precedent), then
/// the waker wakes 0→1 and the runner drains. Returns the fired run id, the
/// running verdict, and the replica count to restore (floored at 1).
async fn drive_in_cluster(
    client: &Client,
    args: &F3ProofArgs,
    deployment: &str,
) -> anyhow::Result<(String, bool, Option<i32>)> {
    let mut ok = true;
    let kube = KubeScale::in_cluster()?;

    // --- park ---
    let original = kube.get_scale(deployment).await?;
    let restore_to = original.spec_replicas.max(1);
    kube.set_replicas(deployment, 0).await?;
    let park_deadline = Instant::now() + Duration::from_secs(60);
    let parked = wait_scale(&kube, deployment, park_deadline, |s| s.status_replicas == 0).await?;
    check(&mut ok, "PARK: runner scaled to 0 replicas", parked);

    // --- activate only after PARK, capture one fire, then deactivate. ---
    let admin_url = args
        .admin_database_url
        .as_deref()
        .context("in-cluster F3 activation needs WAMN_PG_ADMIN_URL")?;
    let (mut admin, connection) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect for F3 activation")?;
    let connection_task = tokio::spawn(connection);
    let activated_after: SystemTime = client
        .query_one("SELECT clock_timestamp()", &[])
        .await?
        .get(0);
    activate_authoritative_cron_release(&mut admin, &args.tenant, args.flow_version).await?;

    let fire_deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let run_id_result: anyhow::Result<Option<String>> = async {
        loop {
            if let Some(id) = first_cron_run_after(client, activated_after).await? {
                break Ok(Some(id));
            }
            if Instant::now() > fire_deadline {
                break Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    .await;
    let deactivate_result =
        deactivate_authoritative_cron_release(&mut admin, &args.tenant, args.flow_version).await;
    drop(admin);
    let _ = connection_task.await;
    deactivate_result.context("deactivate F3 cron release after dispatch")?;
    let run_id = run_id_result?;
    let Some(run_id) = run_id else {
        check(
            &mut ok,
            "DISPATCH: a cron run was written by the dispatcher",
            false,
        );
        return Ok((String::new(), ok, Some(restore_to)));
    };
    let dispatched_count: i64 = client
        .query_one(
            "SELECT count(*) FROM runs WHERE flow_id = $1 AND trigger_source = 'cron' \
               AND created_at >= $2",
            &[&FLOW_ID, &activated_after],
        )
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("DISPATCH: exactly one cron run fired (got {dispatched_count})"),
        dispatched_count == 1,
    );

    // --- wake 0→1 + drain ---
    let wake_deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let woke = wait_scale(&kube, deployment, wake_deadline, |s| s.spec_replicas > 0).await?;
    check(&mut ok, "WAKE: the waker scaled the runner 0→1", woke);
    let drain_deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let status = poll_to_terminal(client, &run_id, drain_deadline).await?;
    check(
        &mut ok,
        &format!("DRAIN: cron run completed (status {status})"),
        status == "completed",
    );

    Ok((run_id, ok, Some(restore_to)))
}

async fn wait_scale(
    kube: &KubeScale,
    deployment: &str,
    deadline: Instant,
    pred: impl Fn(&DeploymentScale) -> bool,
) -> anyhow::Result<bool> {
    loop {
        if pred(&kube.get_scale(deployment).await?) {
            return Ok(true);
        }
        if Instant::now() > deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn first_cron_run_after(
    client: &Client,
    activated_after: SystemTime,
) -> anyhow::Result<Option<String>> {
    Ok(client
        .query_opt(
            "SELECT run_id FROM runs WHERE flow_id = $1 AND trigger_source = 'cron' \
               AND created_at >= $2 ORDER BY created_at, run_id LIMIT 1",
            &[&FLOW_ID, &activated_after],
        )
        .await?
        .map(|r| r.get(0)))
}

/// The F3 acceptance over the drained run: DB escalation state, credential
/// delivery per notify (fnv1a digest), containment (no raw secret recorded), and
/// the cycle proof (the `gate` node visited once per hold + the empty tail).
async fn assert_f3(client: &Client, run_id: &str, secret: &str) -> anyhow::Result<bool> {
    println!("## assert — escalation + vault delivery + containment + cycle drain");
    let mut ok = true;

    // --- DB: exactly the 2 stale holds escalated; the fresh one untouched. ---
    let escalated: i64 = client
        .query_one(
            "SELECT count(*) FROM quality_holds WHERE status = 'escalated'",
            &[],
        )
        .await?
        .get(0);
    let still_open: i64 = client
        .query_one(
            "SELECT count(*) FROM quality_holds WHERE status = 'open'",
            &[],
        )
        .await?
        .get(0);
    let still_disposed: i64 = client
        .query_one(
            "SELECT count(*) FROM quality_holds WHERE status = 'disposed'",
            &[],
        )
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("DB: 2 stale OPEN holds escalated (got {escalated})"),
        escalated == 2,
    );
    check(
        &mut ok,
        &format!("DB: the fresh hold is untouched — still open (got {still_open})"),
        still_open == 1,
    );
    check(
        &mut ok,
        &format!(
            "DB: the stale DISPOSED hold is untouched — status=open filter held (got {still_disposed})"
        ),
        still_disposed == 1,
    );

    // --- run completed; the cycle drained (gate visited 3x: 2 holds + empty). ---
    let run_status: String = client
        .query_one("SELECT status FROM runs WHERE run_id = $1", &[&run_id])
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("CYCLE: run completed (status {run_status})"),
        run_status == "completed",
    );
    let gate_visits: i64 = client
        .query_one(
            "SELECT count(*) FROM node_runs WHERE run_id = $1 AND node_id = 'gate'",
            &[&run_id],
        )
        .await?
        .get(0);
    check(
        &mut ok,
        &format!("CYCLE: the gate node was visited 3x (2 holds + empty tail; got {gate_visits})"),
        gate_visits == 3,
    );

    // --- delivery: every notify visit reflected fnv1a(secret) from serve-echo. ---
    let notify_rows = client
        .query(
            "SELECT occurrence, output_json::text FROM node_runs \
             WHERE run_id = $1 AND node_id = 'notify' ORDER BY occurrence",
            &[&run_id],
        )
        .await?;
    let expected = format!("{:016x}", fnv1a_64(secret.as_bytes()));
    let delivered = notify_rows.len() == 2
        && notify_rows.iter().all(|r| {
            r.get::<_, Option<String>>(1)
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .as_ref()
                .and_then(|v| v.get("body"))
                .and_then(|b| b.get("authorization-fnv1a"))
                .and_then(Value::as_str)
                == Some(expected.as_str())
        });
    check(
        &mut ok,
        &format!(
            "DELIVERY: 2 credentialed notifies, each digest == fnv1a(secret) (got {} rows)",
            notify_rows.len()
        ),
        delivered,
    );

    // --- containment: the raw secret appears NOWHERE the platform recorded. ---
    let run = client
        .query_one(
            "SELECT input_json::text, result_json::text, state_json::text, fail_reason \
             FROM runs WHERE run_id = $1",
            &[&run_id],
        )
        .await?;
    let pinned_graph: Option<String> = client
        .query_opt(
            "SELECT artifact.graph_json::text \
             FROM runs AS run \
             JOIN catalog.flow_artifacts AS artifact \
               ON artifact.tenant_id=run.tenant_id \
              AND artifact.flow_id=run.flow_id \
              AND artifact.flow_version=run.flow_version \
             WHERE run.run_id=$1",
            &[&run_id],
        )
        .await?
        .map(|row| row.get(0));
    let graph: Option<String> = if pinned_graph.is_some() {
        pinned_graph
    } else {
        client
            .query_opt(
                "SELECT graph_json::text FROM flows WHERE flow_id = $1 AND active",
                &[&FLOW_ID],
            )
            .await?
            .and_then(|row| row.get(0))
    };
    let nodes = client
        .query(
            "SELECT node_id, output_json::text, input_json::text, error_detail::text \
             FROM node_runs WHERE run_id = $1",
            &[&run_id],
        )
        .await?;
    let clean = |label: &str, text: &Option<String>, ok: &mut bool| {
        let leaked = text.as_deref().is_some_and(|t| t.contains(secret));
        check(ok, &format!("CONTAINMENT: no secret in {label}"), !leaked);
    };
    clean("flows.graph_json", &graph, &mut ok);
    for (i, label) in ["input_json", "result_json", "state_json", "fail_reason"]
        .iter()
        .enumerate()
    {
        clean(
            &format!("runs.{label}"),
            &run.get::<_, Option<String>>(i),
            &mut ok,
        );
    }
    for row in &nodes {
        let node: String = row.get(0);
        clean(
            &format!("node_runs[{node}].output_json"),
            &row.get::<_, Option<String>>(1),
            &mut ok,
        );
        clean(
            &format!("node_runs[{node}].input_json"),
            &row.get::<_, Option<String>>(2),
            &mut ok,
        );
        clean(
            &format!("node_runs[{node}].error_detail"),
            &row.get::<_, Option<String>>(3),
            &mut ok,
        );
    }

    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate flow the proof registers is a real, valid F3 flow: the cron
    /// trigger, the declared portable HTTP connection, and the structural cycle
    /// (advance loops to the gate; notify is a dead-end). A malformed builder
    /// (e.g. a broken JMESPath in a config) fails here, not only in-cluster.
    #[test]
    fn gate_flow_is_a_valid_f3_flow() {
        let json = gate_flow_json("serve-echo:8091", -60_000, 1);
        let flow = wamn_flow::Flow::from_json(&json).expect("gate flow parses");
        let interfaces = std::collections::BTreeMap::from([
            ("conditional".into(), vec!["true".into(), "false".into()]),
            ("http-request".into(), vec!["main".into()]),
            ("postgres".into(), vec!["main".into()]),
            ("time-shift".into(), vec!["main".into()]),
            ("transform".into(), vec!["main".into()]),
        ]);
        flow.validate(&interfaces).expect("gate flow validates");
        assert_eq!(flow.flow_id, FLOW_ID);
        assert_eq!(
            flow.entry_node().map(|node| node.node_type.as_str()),
            Some("cron")
        );
        assert_eq!(flow.connection_requirements.len(), 1);
        assert_eq!(
            flow.connection_requirements[0].name,
            "manager-notifications"
        );
        let notify = flow
            .nodes
            .iter()
            .find(|node| node.id == "notify")
            .expect("notify node");
        assert_eq!(notify.connection.as_deref(), Some("manager-notifications"));
        assert_eq!(notify.config["path-and-query"], "/holds");
        assert!(notify.credential.is_none());
        assert!(notify.config.get("url").is_none());
        assert!(notify.config.get("idempotency-key").is_none());
        assert!(flow.allowed_hosts.is_empty());
        assert!(flow.credentials.is_empty());
        assert!(
            flow.edges
                .iter()
                .any(|e| e.from == "advance" && e.to == "gate"),
            "the structural cycle closes back to the gate"
        );
        assert!(
            !flow.edges.iter().any(|e| e.from == "notify"),
            "notify is a dead-end — it carries no loop state"
        );
        assert!(flow.nodes.iter().all(|node| node.node_type != "respond"));
        let shift = flow
            .nodes
            .iter()
            .find(|node| node.id == "shift")
            .expect("time-shift node");
        assert_eq!(shift.config["base"], "\"scheduled-at\"");
        // The catalog document the node compiles against is well-formed JSON.
        let cat: Value = serde_json::from_str(&holds_catalog_json()).expect("catalog json");
        assert_eq!(cat["entities"][0]["name"], "quality_holds");
    }

    /// The example runner Secret carries the mapping the in-cluster gate resolves:
    /// `notify-webhook` -> the demo secret, under the default project. Keeps the
    /// manifest and the gate's expected secret from drifting apart.
    #[test]
    fn example_runner_secret_carries_the_notify_webhook() {
        let manifest = include_str!("../../../deploy/platform/runner-credentials.example.yaml");
        assert!(
            manifest.contains("notify-webhook"),
            "credential name present"
        );
        assert!(
            manifest.contains(r#""notify-webhook": "{\"headers\":{\"authorization\":\""#),
            "portable HTTP credential is a header map"
        );
        assert!(manifest.contains(DEMO_SECRET), "f3 demo secret present");
        assert!(
            manifest.contains("\"default\""),
            "keyed by the default project"
        );
    }

    #[test]
    fn authoritative_fixture_builds_a_verifiable_pinned_artifact() {
        let graph = gate_flow_json("serve-echo:8091", -60_000, 7);
        let (flow, artifact) = f3_artifact(TENANT_DEFAULT, &graph).expect("artifact builds");
        assert_eq!(flow.version, 7);
        assert_eq!(artifact.identity().id().flow_id(), FLOW_ID);
        assert!(!artifact.interface_bundle().interfaces().is_empty());
        assert!(!artifact.occurrence_recovery().is_empty());
    }

    #[test]
    fn in_cluster_registration_uses_authoritative_release_tables() {
        assert!(REGISTER_ARTIFACT_SQL.contains("catalog.register_flow_artifact"));
        assert!(REGISTER_MANIFEST_SQL.contains("catalog.register_release_manifest"));
        assert!(REGISTER_FLOW_SQL.contains("INSERT INTO catalog.release_flows"));
        assert!(REGISTER_EXPOSURE_SQL.contains("catalog.register_release_exposure_manifest"));
        assert!(REGISTER_SOURCE_SQL.contains("INSERT INTO catalog.release_sources"));
        assert!(REGISTER_ATTACHMENT_SQL.contains("INSERT INTO catalog.release_attachments"));
        assert!(UPSERT_HEAD_SQL.contains("INSERT INTO catalog.catalog_heads"));
        assert!(UPSERT_ACTIVATION_SQL.contains("INSERT INTO catalog.attachment_activation"));
        assert!(DELETE_ACTIVATION_SQL.starts_with("DELETE FROM catalog.attachment_activation"));
        assert!(DELETE_HEAD_SQL.starts_with("DELETE FROM catalog.catalog_heads"));
    }
}
