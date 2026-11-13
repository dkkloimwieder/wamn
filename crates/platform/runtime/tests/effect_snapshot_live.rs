//! Exercise the cluster effect snapshot with the provisioned HTTP admitter grants (wamn-degmr).

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeMap;

use anyhow::Context as _;

use url::Url;
use wamn_catalog::{WiringDocument, WiringNode, WiringTerminal};
use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_run_state::AuthorityClass;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ConnectionEffectLookup, ConnectionEntryLookup, DEFAULT_PROJECT, WamnPostgres,
    WamnPostgresConfig, WiringLookup,
};

const PROJECT_ENV_KEY: &str = "effect-snapshot-live";
const PACKAGE: &str = "snapshot_fixture";
const COMPONENT: &str = "http-request";
const GENERATION: &str = "wamn_http_admitter_snapshot_live";
const PASSWORD: &str = "snapshot-test-password";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

async fn reads(admin: &mut Client) -> anyhow::Result<i64> {
    Ok(sqlx::query(
        "SELECT COALESCE(sum(calls),0)::bigint FROM pg_stat_statements \
         WHERE userid=(SELECT oid FROM pg_roles WHERE rolname=$1) \
         AND dbid=(SELECT oid FROM pg_database WHERE datname=current_database())",
    )
    .bind(&GENERATION)
    .fetch_one(&mut *admin)
    .await?
    .get(0))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY"]
async fn cluster_effect_snapshot_as_http_admitter() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
    );
    let _lock = wamn_test_postgres::lock();
    let mut server =
        wamn_test_postgres::start(&[("shared_preload_libraries", "pg_stat_statements")])?;
    let database = server.create_database("effect_snapshot")?;
    let mut admin = connect(database.url()).await?;
    sqlx::raw_sql("CREATE EXTENSION pg_stat_statements")
        .execute(&mut admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::ensure_app_acl_role_sql()).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut admin)
        .await
        .map(drop)
        .context("install the production catalog schema")?;
    sqlx::raw_sql(wamn_control_provision::APP_SCHEMA_SQL)
        .execute(&mut admin)
        .await
        .map(drop)?;
    sqlx::raw_sql("CREATE SCHEMA wamn_run")
        .execute(&mut admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::HttpAdmitter,
            "effect_snapshot",
            GENERATION,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .context("provision the production HTTP admitter surface and login")?;

    sqlx::raw_sql("SELECT set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', false), set_config('app.operation', 'admin:seed-catalog-fixture', false)").execute(&mut admin).await.map(drop)?;
    let component_digest = format!("sha256:{}", "a".repeat(64));
    let manifest = serde_json::json!({
        "release": {"packages": [{"package-id": PACKAGE, "package-version": "1.0.0"}]},
        "components": [{
            "package-id": PACKAGE,
            "descriptor": {
                "component": COMPONENT,
                "interface-version": "0.1.0",
                "component-digest": component_digest,
                "operations": {"request": {"registered-operation": "snapshot_fixture/request"}}
            }
        }]
    });
    let bytes = serde_json::to_vec(&manifest)?;
    let manifest_digest: String = sqlx::query(
        "INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, 'sha256:' || encode(sha256($2::bytea), 'hex'), $2) \
             RETURNING manifest_digest",
    )
    .bind(&PROJECT_ENV_KEY)
    .bind(&bytes)
    .fetch_one(&mut admin)
    .await?
    .get(0);
    sqlx::query("INSERT INTO catalog.connection_requirement(project_env_key,component_digest,package_id,name,interface,mode) \
         VALUES($1,$2,$3,'erp','wamn:connection/http@0.1.0','write')").bind(&PROJECT_ENV_KEY).bind(&component_digest).bind(&PACKAGE).execute(&mut admin).await.map(|result| result.rows_affected())?;
    let graph = WiringDocument::new(
        "snapshot",
        1,
        "request",
        BTreeMap::from([(
            "request".to_owned(),
            WiringNode {
                component: COMPONENT.to_owned(),
                interface_version: "0.1.0".to_owned(),
                operation: "request".to_owned(),
                operation_dependency: None,
                params: BTreeMap::new(),
                terminal: Some(WiringTerminal::Respond),
            },
        )]),
        Vec::new(),
    )?;
    sqlx::query(
        "INSERT INTO catalog.workflows \
             (project_env_key, environment, application, workflow_id, version, graph_json, \
              wiring_hash, trigger, execution_type, created_by) \
             VALUES ($1, 'dev', $2, 'snapshot', 1, $3::text::jsonb, $4, 'manual', \
                     'invoker', '00000000-0000-0000-0000-000000000001')",
    )
    .bind(&PROJECT_ENV_KEY)
    .bind(&PACKAGE)
    .bind(&serde_json::to_string(&graph)?)
    .bind(&graph.wiring_hash().as_str())
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;

    let mut credential = Url::parse(database.url())?;
    credential
        .set_username(GENERATION)
        .map_err(|()| anyhow::anyhow!("set the test generation username"))?;
    credential
        .set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set the test generation password"))?;
    let mut principal = connect(credential.as_str()).await?;
    let identity = sqlx::query(
        "SELECT current_user::text, rolsuper, rolbypassrls, \
                    pg_has_role(current_user, 'wamn_http_admitter', 'USAGE') \
             FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&mut principal)
    .await?;
    assert_eq!(identity.get::<String, _>(0), GENERATION);
    assert!(!identity.get::<bool, _>(1));
    assert!(!identity.get::<bool, _>(2));
    assert!(identity.get::<bool, _>(3));

    // No LocalApplication: this is the same SQL path that the cluster host uses.
    let postgres = std::sync::Arc::new(WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(
            ClassCredentials::default().with_class(AuthorityClass::CallableHttp, credential),
        ),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
        row_limit: 10_000,
    })?);
    postgres.set_project_env_key(COMPONENT, PROJECT_ENV_KEY)?;
    let lookup = ConnectionEffectLookup {
        entry: ConnectionEntryLookup {
            package_id: PACKAGE,
            wiring: Some(WiringLookup {
                wiring_id: "snapshot",
                wiring_version: 1,
                node_id: "request",
            }),
        },
        origin_package_id: PACKAGE,
        origin_component_digest: &component_digest,
        origin_component: COMPONENT,
        origin_interface_version: "0.1.0",
        origin_operation: "request",
        package_id: PACKAGE,
        operation: "request",
        manifest_digest: &manifest_digest,
        environment: "dev",
        component_digest: &component_digest,
        store_alias: "erp",
        candidate_binding: None,
    };
    let memory = std::sync::Arc::new(wamn_runtime::connection_memory::ConnectionMemory::default());
    let broker = wamn_broker::nats::PlatformNats::from_env()?
        .connect()
        .await?;
    wamn_broker::kv_memory::create_buckets(&broker).await?;
    let changes = wamn_runtime::connection_memory::replicate(
        &broker,
        Some(PROJECT_ENV_KEY),
        std::sync::Arc::clone(&memory),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !memory.watched() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .context("the snapshot connection watch starts")?;
    let facts = wamn_runtime::connection_facts::ConnectionFacts::new(
        std::sync::Arc::clone(&memory),
        changes,
        std::sync::Arc::new(wamn_runtime::connection_credentials::ConnectionCredentials::empty()),
        std::sync::Arc::clone(&postgres),
        PROJECT_ENV_KEY,
        DEFAULT_PROJECT,
        "dev",
    );
    let snapshot = postgres
        .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, PROJECT_ENV_KEY, &lookup, &facts)
        .await
        .context("run the cluster effect snapshot as the HTTP admitter")?
        .context("the provisioned role must read the workflow and release")?;
    assert_eq!(
        snapshot.wiring_hash.as_deref(),
        Some(graph.wiring_hash().as_str())
    );
    assert_eq!(snapshot.component.as_deref(), Some(COMPONENT));
    assert_eq!(snapshot.interface_version.as_deref(), Some("0.1.0"));
    assert_eq!(snapshot.operation.as_deref(), Some("request"));
    assert_eq!(
        snapshot.registered_operation.as_deref(),
        Some("snapshot_fixture/request")
    );
    assert!(snapshot.node_permitted);
    assert!(memory.release(PROJECT_ENV_KEY, &manifest_digest).is_ok());
    let first_reads = reads(&mut admin).await?;
    assert!(first_reads > 0, "the cold snapshot reads PostgreSQL");
    let repeated = postgres
        .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, PROJECT_ENV_KEY, &lookup, &facts)
        .await?
        .context("the repeated snapshot resolves")?;
    assert_eq!(repeated.wiring_hash, snapshot.wiring_hash);
    assert_eq!(repeated.registered_operation, snapshot.registered_operation);
    assert_eq!(repeated.node_permitted, snapshot.node_permitted);
    let repeated_reads = reads(&mut admin).await?;
    assert_eq!(
        repeated_reads, first_reads,
        "the warm snapshot reads no PostgreSQL statement"
    );
    Ok(())
}
