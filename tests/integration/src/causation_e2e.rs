//! Deployed-runner causation proof from production invocation admission to the R3 event stream.

use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use clap::Args;
use tokio_postgres::{Client, NoTls};

use crate::cdc_reader_process::{ReaderArgs, ReaderProcess};
use crate::readerbench::{self, ReaderBenchArgs};
use wamn_control_provision::sql as provision_sql;
use wamn_control_registry::sql as registry_sql;
use wamn_gate_harness::{check, seed_flow_version};
use wamn_run_state::admission::{
    AdmissionResult, AdmissionTransition, RunStateSchema, admission_transaction,
};
use wamn_test_fixtures::runner::connect_app;

const ORG: &str = "ec7j";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const TENANT: &str = "demo-tenant";
const SCHEMA: &str = "wamn_runner_demo";
const FLOW_ID: &str = "causation-e2e";
const CATALOG_ID: &str = "causation-e2e-catalog";
const CATALOG_VERSION: i32 = 2;
const FLOW_VERSION: i32 = 2;
const SOURCE_ID: &str = "causation-e2e-auth";
const ATTACHMENT_ID: &str = "causation-e2e-invoke";
const RUN_ID: &str = "causation-e2e-run";
const ENTITY_ID: &str = "causation-e2e-sink";
const CDC_NAME: &str = "wamn_cdc_ec7j__app__dev";
const CDC_PASSWORD: &str = "ec7j_cdc_password";
const STREAM: &str = "EVT_ec7j_dev";
const GRAPH_HASH: &str = "fixture-graph:causation-e2e:v2";
const ARTIFACT_HASH: &str = "fixture-artifact:causation-e2e:v2";
const SOURCE_HASH: &str = "fixture-source:causation-e2e:v2";
const DEFINITION_HASH: &str = "fixture-definition:causation-e2e:v2";

#[derive(Debug, Args)]
pub struct CausationE2eArgs {
    /// Application-role URL used by the deployed runner fixture.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// Superuser URL for gate-scoped PostgreSQL objects in the runner fixture.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Superuser URL for gate-scoped registry rows in `wamn_system`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Data-plane R3 JetStream URL.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Bound for deployed execution and CDC delivery.
    #[arg(long, default_value_t = 120)]
    pub timeout_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Baseline {
    flows: i64,
    runs: i64,
    node_runs: i64,
    queue: i64,
}

#[derive(Debug, Default)]
struct GateState {
    reader: Option<ReaderProcess>,
}

fn flow_json() -> String {
    serde_json::json!({
        "schema-version": "0.1",
        "flow-id": FLOW_ID,
        "version": FLOW_VERSION,
        "name": "deployed runner causation e2e",
        "trigger": { "type": "manual" },
        "entry": "request",
        "nodes": [
            { "id": "request", "type": "webhook-in" },
            { "id": "write", "type": "pg-write" }
        ],
        "edges": [{ "from": "request", "to": "write" }]
    })
    .to_string()
}

fn role_url(admin_url: &str) -> anyhow::Result<String> {
    let plain = admin_url
        .split('?')
        .next()
        .context("PostgreSQL URL is empty")?;
    let after_scheme = plain
        .strip_prefix("postgres://")
        .context("PostgreSQL URL must use postgres://")?;
    let (_, host_and_path) = after_scheme
        .rsplit_once('@')
        .context("PostgreSQL URL must carry userinfo")?;
    Ok(format!(
        "postgres://{CDC_NAME}:{CDC_PASSWORD}@{host_and_path}"
    ))
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("connect PostgreSQL at {url}"))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

async fn baseline(admin: &Client) -> anyhow::Result<Baseline> {
    let row = admin
        .query_one(
            &format!(
                "SELECT \
                   (SELECT count(*) FROM {SCHEMA}.flows), \
                   (SELECT count(*) FROM {SCHEMA}.runs), \
                   (SELECT count(*) FROM {SCHEMA}.node_runs), \
                   (SELECT count(*) FROM {SCHEMA}.run_queue)"
            ),
            &[],
        )
        .await?;
    Ok(Baseline {
        flows: row.get(0),
        runs: row.get(1),
        node_runs: row.get(2),
        queue: row.get(3),
    })
}

async fn setup_registry(system: &mut Client) -> anyhow::Result<()> {
    let transaction = system.transaction().await?;
    transaction
        .execute(
            registry_sql::upsert_org_sql(),
            &[&ORG, &"dedicated", &Option::<&str>::None],
        )
        .await?;
    transaction
        .execute(
            registry_sql::stamp_env_policy_sql(),
            &[
                &ORG,
                &ENV,
                &r#""own""#,
                &0i32,
                &1i32,
                &"1Gi",
                &"100m",
                &"128Mi",
                &"postgres:18",
                &"",
                &"",
                &"off",
            ],
        )
        .await?;
    transaction
        .execute(registry_sql::upsert_project_sql(), &[&ORG, &PROJECT])
        .await?;
    transaction
        .execute(
            registry_sql::upsert_project_env_sql(),
            &[
                &ORG,
                &PROJECT,
                &ENV,
                &"causation-e2e-app",
                &Option::<&str>::None,
            ],
        )
        .await?;
    transaction
        .execute(
            registry_sql::upsert_event_reader_sql(),
            &[
                &ORG,
                &PROJECT,
                &ENV,
                &CDC_NAME,
                &CDC_NAME,
                &STREAM,
                &"causation-e2e-cdc",
                &Option::<&str>::None,
                &true,
            ],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn setup_catalog(admin: &mut Client) -> anyhow::Result<()> {
    let graph = flow_json();
    let members = serde_json::json!([{
        "flow-id": FLOW_ID,
        "flow-version": FLOW_VERSION,
        "artifact-hash": ARTIFACT_HASH,
    }]);
    let source = serde_json::json!({
        "mode": "none",
    });
    let attachment = serde_json::json!({
        "id": ATTACHMENT_ID,
        "kind": "http",
        "flow-id": FLOW_ID,
        "source-id": SOURCE_ID,
        "route-host": "causation-e2e.invalid",
        "route-path": "/invoke",
        "route-template": "/invoke",
        "route-method": "POST",
    });
    let definitions = serde_json::json!({
        "sources": [{ "id": SOURCE_ID, "kind": "auth", "definition": source }],
        "attachments": [attachment],
    });

    let transaction = admin.transaction().await?;
    transaction
        .execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ($1,$2,$3,'dev','0.1','staged') \
             ON CONFLICT (tenant_id,catalog_id,version) DO NOTHING",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.catalogs SET state='superseded' \
             WHERE tenant_id=$1 AND catalog_id=$2 AND environment='dev' \
               AND state='applied' AND version<>$3",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.catalogs SET state='applied' \
             WHERE tenant_id=$1 AND catalog_id=$2 AND version=$3",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                artifact_hash) \
             VALUES ($1,$2,$3,'0.1',$4::text::jsonb,$5,$6) \
             ON CONFLICT (tenant_id,flow_id,flow_version) DO NOTHING",
            &[
                &TENANT,
                &FLOW_ID,
                &FLOW_VERSION,
                &graph,
                &GRAPH_HASH,
                &ARTIFACT_HASH,
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version,members_json) VALUES ($1,$2,$3,$4) \
             ON CONFLICT (tenant_id,catalog_id,catalog_version) DO NOTHING",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION, &members],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ($1,'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a', \
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
             VALUES ($1,$2,$3,$4,$5, \
               'sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a') \
             ON CONFLICT (tenant_id,catalog_id,catalog_version,flow_id) DO NOTHING",
            &[
                &TENANT,
                &CATALOG_ID,
                &CATALOG_VERSION,
                &FLOW_ID,
                &FLOW_VERSION,
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_exposure_manifests \
               (tenant_id,catalog_id,catalog_version,definitions_json) VALUES ($1,$2,$3,$4) \
             ON CONFLICT (tenant_id,catalog_id,catalog_version) DO NOTHING",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION, &definitions],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_sources \
               (tenant_id,catalog_id,catalog_version,source_id,source_kind,definition_json,source_hash) \
             VALUES ($1,$2,$3,$4,'auth',$5,$6) \
             ON CONFLICT (tenant_id,catalog_id,catalog_version,source_id) DO NOTHING",
            &[
                &TENANT,
                &CATALOG_ID,
                &CATALOG_VERSION,
                &SOURCE_ID,
                &source,
                &SOURCE_HASH,
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.release_attachments \
               (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id, \
                source_id,definition_hash,definition_json,route_host,route_path,route_template,route_method) \
             VALUES ($1,$2,$3,$4,'http',$5,$6,$7,$8, \
                     'causation-e2e.invalid','/invoke','/invoke','POST') \
             ON CONFLICT (tenant_id,catalog_id,catalog_version,attachment_id) DO NOTHING",
            &[
                &TENANT,
                &CATALOG_ID,
                &CATALOG_VERSION,
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
            "INSERT INTO catalog.catalog_heads \
               (tenant_id,catalog_id,environment,applied_catalog_version) VALUES ($1,$2,'dev',$3)",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.attachment_activation \
               (tenant_id,catalog_id,environment,attachment_id,confirmed_definition_hash,enabled) \
             VALUES ($1,$2,'dev',$3,$4,true)",
            &[&TENANT, &CATALOG_ID, &ATTACHMENT_ID, &DEFINITION_HASH],
        )
        .await?;
    transaction.commit().await?;

    let visible: i64 = admin
        .query_one(
            "SELECT count(*) FROM catalog.http_routes attachment \
             JOIN catalog.release_flows member \
               USING (tenant_id,catalog_id,catalog_version,flow_id) \
             WHERE attachment.tenant_id=$1 AND attachment.catalog_id=$2 \
               AND attachment.attachment_id=$3 AND attachment.definition_hash=$4 \
               AND attachment.flow_id=$5 AND attachment.catalog_version=$6 \
               AND member.flow_version=$7",
            &[
                &TENANT,
                &CATALOG_ID,
                &ATTACHMENT_ID,
                &DEFINITION_HASH,
                &FLOW_ID,
                &CATALOG_VERSION,
                &FLOW_VERSION,
            ],
        )
        .await?
        .get(0);
    ensure!(
        visible == 1,
        "dormant immutable causation fixture drifted from its exact release"
    );
    Ok(())
}

async fn setup_project(args: &CausationE2eArgs) -> anyhow::Result<()> {
    let mut admin = connect(&args.admin_database_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()", &[])
        .await?
        .get(0);
    admin
        .batch_execute(&format!(
            "CREATE TABLE {SCHEMA}.sink ( \
               tenant_id text NOT NULL, run_id text NOT NULL, step int NOT NULL, payload text NOT NULL, \
               PRIMARY KEY (tenant_id,run_id,step)); \
             ALTER TABLE {SCHEMA}.sink ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.sink FORCE ROW LEVEL SECURITY; \
             CREATE POLICY sink_tenant ON {SCHEMA}.sink \
               USING (tenant_id=current_setting('app.tenant',true)) \
               WITH CHECK (tenant_id=current_setting('app.tenant',true)); \
             GRANT SELECT,INSERT,UPDATE,DELETE ON {SCHEMA}.sink TO wamn_app; \
             CREATE TABLE {SCHEMA}.wamn_entities ( \
               relation_oid oid PRIMARY KEY, entity_id text NOT NULL, table_name text NOT NULL); \
             INSERT INTO {SCHEMA}.wamn_entities (relation_oid,entity_id,table_name) \
               SELECT '{SCHEMA}.sink'::regclass::oid, '{ENTITY_ID}', 'sink';"
        ))
        .await
        .context("create gate sink and entity map")?;
    admin
        .batch_execute(&provision_sql::ensure_replication_role_sql(
            CDC_NAME,
            CDC_PASSWORD,
        ))
        .await?;
    admin
        .batch_execute(&provision_sql::create_publication_sql(CDC_NAME, SCHEMA))
        .await?;
    admin
        .batch_execute(&provision_sql::grant_replication_access_sql(
            &database, CDC_NAME, SCHEMA,
        ))
        .await?;
    setup_catalog(&mut admin).await?;

    let app = connect_app(&args.database_url, SCHEMA, TENANT).await?;
    seed_flow_version(
        &app,
        TENANT,
        FLOW_ID,
        FLOW_VERSION,
        true,
        &flow_json(),
        true,
    )
    .await?;

    // Capture begins only after every setup write, so the stream contains only the real run.
    admin
        .batch_execute(&provision_sql::create_failover_slot_sql(CDC_NAME))
        .await?;
    Ok(())
}

fn reader_args(args: &CausationE2eArgs) -> anyhow::Result<ReaderArgs> {
    Ok(ReaderArgs {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENV.into(),
        system_database_url: args.system_database_url.clone(),
        cdc_url: role_url(&args.admin_database_url)?,
        nats_url: args.nats_url.clone(),
        stream_replicas: 3,
    })
}

fn readerbench_args(args: &CausationE2eArgs, run_id: &str) -> ReaderBenchArgs {
    ReaderBenchArgs {
        nats_url: args.nats_url.clone(),
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENV.into(),
        stream: Some(STREAM.into()),
        entity: "sink".into(),
        expect_entity_id: Some(ENTITY_ID.into()),
        id_field: "run_id".into(),
        filter_entity: Some(ENTITY_ID.into()),
        expect_causation_run: Some(run_id.into()),
        expect_ids: vec![run_id.into()],
        wait_secs: args.timeout_secs,
        delete_stream: false,
    }
}

async fn wait_for_stream(args: &CausationE2eArgs) -> anyhow::Result<()> {
    let client = async_nats::connect(&args.nats_url).await?;
    let js = async_nats::jetstream::new(client);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if js.get_stream(STREAM).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("reader did not create R3 stream {STREAM} within 10s");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn admit_run(args: &CausationE2eArgs) -> anyhow::Result<String> {
    let mut app = connect_app(&args.database_url, SCHEMA, TENANT).await?;
    let transaction = app.transaction().await?;
    let schema = RunStateSchema::new(SCHEMA)?;
    let recipe = admission_transaction(AdmissionTransition::CallableFlow { schema: &schema });
    transaction
        .query_one(recipe.lock_head(), &[&CATALOG_ID, &ENV])
        .await?;

    let producer = "http";
    let input = serde_json::Value::String("causation-e2e-payload".into()).to_string();
    let invocation_context = "{}";
    let platform_revision = "causation-e2e";
    let response_deadline: Option<chrono::DateTime<chrono::Utc>> = None;
    let run_deadline: Option<chrono::DateTime<chrono::Utc>> = None;
    let principal_digest = "causation-e2e-principal";
    let client_key_digest = "causation-e2e-key";
    let request_fingerprint = "causation-e2e-request";
    let none_i64: Option<i64> = None;
    let none_i32: Option<i32> = None;
    let none_text: Option<&str> = None;
    let row = transaction
        .query_one(
            recipe.admit(),
            &[
                &producer,
                &CATALOG_ID,
                &ENV,
                &CATALOG_VERSION,
                &ATTACHMENT_ID,
                &DEFINITION_HASH,
                &FLOW_ID,
                &FLOW_VERSION,
                &RUN_ID,
                &input,
                &invocation_context,
                &platform_revision,
                &response_deadline,
                &run_deadline,
                &principal_digest,
                &client_key_digest,
                &request_fingerprint,
                &none_text,
                &none_i64,
                &none_text,
                &none_text,
                &none_text,
                &none_text,
                &none_i32,
            ],
        )
        .await?;
    let result = AdmissionResult::from_parts(row.get(0), row.get(1))
        .context("production admission returned an unknown result")?;
    ensure!(
        result
            == (AdmissionResult::Admitted {
                run_id: RUN_ID.into(),
            }),
        "production invocation admission did not admit the gate run: {result:?}"
    );
    transaction.commit().await?;
    Ok(RUN_ID.into())
}

async fn wait_for_run(args: &CausationE2eArgs) -> anyhow::Result<String> {
    let app = connect_app(&args.database_url, SCHEMA, TENANT).await?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
        if let Some(row) = app
            .query_opt(
                "SELECT r.run_id FROM runs AS r JOIN sink AS s USING (tenant_id,run_id) \
                 WHERE r.run_id=$1 AND r.trigger_source='http' AND r.status='completed'",
                &[&RUN_ID],
            )
            .await?
        {
            let run_id: String = row.get(0);
            let count: i64 = app
                .query_one("SELECT count(*) FROM sink", &[])
                .await?
                .get(0);
            ensure!(
                count == 1,
                "expected exactly one deployed sink write, found {count}"
            );
            return Ok(run_id);
        }
        if Instant::now() >= deadline {
            bail!(
                "deployed runner did not complete the admitted causation flow within {}s",
                args.timeout_secs
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn cleanup(args: &CausationE2eArgs, state: &mut GateState) -> anyhow::Result<()> {
    if let Some(reader) = state.reader.take() {
        let _ = reader.shutdown(Duration::from_secs(15)).await;
    }

    if let Ok(admin) = connect(&args.admin_database_url).await {
        let _ = admin
            .execute(
                "SELECT pg_terminate_backend(active_pid, 5000) \
                 FROM pg_replication_slots WHERE slot_name=$1 AND active",
                &[&CDC_NAME],
            )
            .await;
        let _ = admin
            .batch_execute(&provision_sql::drop_replication_slot_sql(CDC_NAME))
            .await;
        let _ = admin
            .batch_execute(&provision_sql::drop_publication_sql(CDC_NAME))
            .await;
        let _ = admin
            .batch_execute(&format!(
                "UPDATE catalog.attachment_activation SET enabled=false \
                   WHERE tenant_id='{TENANT}' AND catalog_id='{CATALOG_ID}'; \
                 DELETE FROM {SCHEMA}.runs WHERE flow_id='{FLOW_ID}'; \
                 DELETE FROM {SCHEMA}.flows WHERE flow_id='{FLOW_ID}'; \
                 DELETE FROM catalog.attachment_activation WHERE tenant_id='{TENANT}' AND catalog_id='{CATALOG_ID}'; \
                 DELETE FROM catalog.catalog_heads WHERE tenant_id='{TENANT}' AND catalog_id='{CATALOG_ID}'; \
                 UPDATE catalog.catalogs SET state='superseded' \
                   WHERE tenant_id='{TENANT}' AND catalog_id='{CATALOG_ID}' AND version={CATALOG_VERSION}; \
                 UPDATE catalog.catalogs SET state='applied' \
                   WHERE tenant_id='{TENANT}' AND catalog_id='{CATALOG_ID}' AND version=1; \
                 DROP TABLE IF EXISTS {SCHEMA}.sink; \
                 DROP TABLE IF EXISTS {SCHEMA}.wamn_entities;"
            ))
            .await;
        let _ = admin
            .batch_execute(&format!(
                "DROP OWNED BY {CDC_NAME}; DROP ROLE IF EXISTS {CDC_NAME}"
            ))
            .await;
    }
    if let Ok(system) = connect(&args.system_database_url).await {
        let _ = system
            .execute("DELETE FROM registry.orgs WHERE id=$1", &[&ORG])
            .await;
    }
    if let Ok(nats) = async_nats::connect(&args.nats_url).await {
        let _ = async_nats::jetstream::new(nats).delete_stream(STREAM).await;
    }
    Ok(())
}

async fn assert_zero_residue(args: &CausationE2eArgs, before: Baseline) -> anyhow::Result<()> {
    let admin = connect(&args.admin_database_url).await?;
    ensure!(
        baseline(&admin).await? == before,
        "shared run-plane baseline changed"
    );
    let project_residue: i64 = admin
        .query_one(
            &format!(
                "SELECT \
                  (SELECT count(*) FROM pg_replication_slots WHERE slot_name='{CDC_NAME}') + \
                  (SELECT count(*) FROM pg_publication WHERE pubname='{CDC_NAME}') + \
                  (SELECT count(*) FROM pg_roles WHERE rolname='{CDC_NAME}') + \
                  (SELECT count(*) FROM information_schema.tables \
                    WHERE table_schema='{SCHEMA}' AND table_name IN ('sink','wamn_entities'))"
            ),
            &[],
        )
        .await?
        .get(0);
    ensure!(
        project_residue == 0,
        "project database residue count is {project_residue}"
    );

    let system = connect(&args.system_database_url).await?;
    let registry_residue: i64 = system
        .query_one("SELECT count(*) FROM registry.orgs WHERE id=$1", &[&ORG])
        .await?
        .get(0);
    ensure!(
        registry_residue == 0,
        "registry residue count is {registry_residue}"
    );

    let fixture: i64 = admin
        .query_one(
            "SELECT count(*) FROM catalog.release_attachments \
             WHERE tenant_id=$1 AND catalog_id=$2 AND catalog_version=$3 \
               AND attachment_id=$4 AND attachment_kind='http' \
               AND definition_hash=$5 AND flow_id=$6",
            &[
                &TENANT,
                &CATALOG_ID,
                &CATALOG_VERSION,
                &ATTACHMENT_ID,
                &DEFINITION_HASH,
                &FLOW_ID,
            ],
        )
        .await?
        .get(0);
    ensure!(
        fixture == 1,
        "dormant immutable fixture is absent or drifted"
    );
    let active_fixture: i64 = admin
        .query_one(
            "SELECT count(*) FROM catalog.http_routes \
             WHERE tenant_id=$1 AND catalog_id=$2",
            &[&TENANT, &CATALOG_ID],
        )
        .await?
        .get(0);
    ensure!(active_fixture == 0, "dormant fixture remains activated");
    let release_states: i64 = admin
        .query_one(
            "SELECT count(*) FROM catalog.catalogs \
             WHERE tenant_id=$1 AND catalog_id=$2 \
               AND ((version=1 AND state='applied') OR (version=$3 AND state='superseded'))",
            &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
        )
        .await?
        .get(0);
    ensure!(
        release_states == 2,
        "catalog release states were not restored"
    );

    let nats = async_nats::connect(&args.nats_url).await?;
    ensure!(
        async_nats::jetstream::new(nats)
            .get_stream(STREAM)
            .await
            .is_err(),
        "stream {STREAM} remains after teardown"
    );
    Ok(())
}

pub async fn run(args: CausationE2eArgs) -> anyhow::Result<()> {
    println!("# wamn-gates causation-e2e — admitted deployed run -> WAL reader -> evt-nats R3");
    let mut state = GateState::default();
    cleanup(&args, &mut state).await?;
    let before = baseline(&connect(&args.admin_database_url).await?).await?;

    let result = async {
        let mut system = connect(&args.system_database_url).await?;
        setup_registry(&mut system).await?;
        setup_project(&args).await?;
        state.reader = Some(ReaderProcess::spawn(reader_args(&args)?)?);
        wait_for_stream(&args).await?;

        let admitted_run_id = admit_run(&args).await?;
        let run_id = wait_for_run(&args).await?;
        ensure!(run_id == admitted_run_id, "runner completed the wrong run");
        println!("deployed run completed with sink write: {run_id}");
        readerbench::run(readerbench_args(&args, &run_id)).await?;
        anyhow::Ok(run_id)
    }
    .await;

    cleanup(&args, &mut state).await?;
    let residue = assert_zero_residue(&args, before).await;
    let run_id = result?;
    residue?;

    let mut pass = true;
    check(
        &mut pass,
        "deployed run id was asserted on every sink envelope",
        !run_id.is_empty(),
    );
    check(
        &mut pass,
        "all gate-scoped resources removed and shared fixture restored",
        true,
    );
    ensure!(pass, "causation e2e gate failed");
    println!("causation-e2e complete — overall PASS: true");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> CausationE2eArgs {
        CausationE2eArgs {
            database_url: "postgres://wamn_app:wamn_app@postgres/wamn".into(),
            admin_database_url: "postgres://postgres:postgres@postgres/wamn".into(),
            system_database_url: "postgres://postgres:postgres@sysdb/wamn_system".into(),
            nats_url: "nats://evt-nats:4222".into(),
            timeout_secs: 120,
        }
    }

    #[test]
    fn invocation_fixture_drives_one_gate_scoped_pg_write() {
        let graph: serde_json::Value = serde_json::from_str(&flow_json()).unwrap();
        assert_eq!(graph["trigger"]["type"], "manual");
        assert_eq!(graph["entry"], "request");
        assert_eq!(graph["nodes"][0]["type"], "webhook-in");
        assert_eq!(graph["nodes"][1]["type"], "pg-write");
        assert_eq!(graph["edges"].as_array().unwrap().len(), 1);
        let schema = RunStateSchema::new(SCHEMA).unwrap();
        let recipe = admission_transaction(AdmissionTransition::CallableFlow { schema: &schema });
        assert!(
            recipe
                .admit()
                .contains("INSERT INTO \"wamn_runner_demo\".runs")
        );
        assert!(
            recipe
                .admit()
                .contains("INSERT INTO \"wamn_runner_demo\".invocation_admissions")
        );
    }

    #[test]
    fn proof_arguments_require_r3_and_the_exact_run_id() {
        let args = args();
        assert_eq!(reader_args(&args).unwrap().stream_replicas, 3);
        let bench = readerbench_args(&args, "run-exact");
        assert_eq!(bench.filter_entity.as_deref(), Some(ENTITY_ID));
        assert_eq!(bench.expect_causation_run.as_deref(), Some("run-exact"));
        assert_eq!(bench.expect_ids, ["run-exact"]);
        assert!(
            !bench.delete_stream,
            "always-run teardown owns stream deletion"
        );
    }
}
