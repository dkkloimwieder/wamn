//! Exercise the cluster effect snapshot with the provisioned HTTP admitter grants (wamn-degmr).

use std::collections::BTreeMap;

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_catalog::{WiringDocument, WiringNode, WiringTerminal};
use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_run_state::AuthorityClass;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ConnectionEffectLookup, ConnectionEntryLookup, DEFAULT_PROJECT, WamnPostgres,
    WamnPostgresConfig, WiringLookup,
};

const ENV_KEY: &str = "effect-snapshot-live";
const PACKAGE: &str = "snapshot_fixture";
const COMPONENT: &str = "http-request";
const GENERATION: &str = "wamn_http_admitter_snapshot_live_a";
const PASSWORD: &str = "snapshot-test-password";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("effect snapshot live connection failed: {error}");
        }
    });
    Ok(client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_effect_snapshot_as_http_admitter() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let admin = connect(database.url()).await?;
    admin.batch_execute(&sql::ensure_app_acl_role_sql()).await?;
    admin
        .batch_execute(wamn_schema_control::ensure_scenario_author_role_sql())
        .await?;
    admin
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .context("install the production catalog schema")?;
    admin
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await?;
    admin.batch_execute("CREATE SCHEMA wamn_run").await?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::HttpAdmitter,
            database.name(),
            GENERATION,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await
        .context("provision the production HTTP admitter surface and login")?;

    admin.batch_execute("SELECT set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', false), set_config('app.operation', 'admin:seed-catalog-fixture', false)").await?;
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
    let manifest_digest: String = admin
        .query_one(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, 'sha256:' || encode(sha256($2::bytea), 'hex'), $2) \
             RETURNING manifest_digest",
            &[&ENV_KEY, &bytes],
        )
        .await?
        .get(0);
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
        Vec::new(),
    )?;
    admin
        .execute(
            "INSERT INTO catalog.workflows \
             (env_key, environment, application, workflow_id, version, graph_json, \
              wiring_hash, trigger, execution_type, created_by) \
             VALUES ($1, 'dev', $2, 'snapshot', 1, $3::text::jsonb, $4, 'manual', \
                     'invoker', '00000000-0000-0000-0000-000000000001')",
            &[
                &ENV_KEY,
                &PACKAGE,
                &serde_json::to_string(&graph)?,
                &graph.wiring_hash().as_str(),
            ],
        )
        .await?;

    let mut credential = Url::parse(database.url())?;
    credential
        .set_username(GENERATION)
        .map_err(|()| anyhow::anyhow!("set the test generation username"))?;
    credential
        .set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set the test generation password"))?;
    let principal = connect(credential.as_str()).await?;
    let identity = principal
        .query_one(
            "SELECT current_user::text, rolsuper, rolbypassrls, \
                    pg_has_role(current_user, 'wamn_http_admitter', 'USAGE') \
             FROM pg_roles WHERE rolname = current_user",
            &[],
        )
        .await?;
    assert_eq!(identity.get::<_, String>(0), GENERATION);
    assert!(!identity.get::<_, bool>(1));
    assert!(!identity.get::<_, bool>(2));
    assert!(identity.get::<_, bool>(3));

    // No LocalApplication: this is the same SQL path that the cluster host uses.
    let postgres = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(
            ClassCredentials::default().with_class(AuthorityClass::CallableHttp, credential),
        ),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
        row_limit: 10_000,
    })?;
    postgres.set_env_key(COMPONENT, ENV_KEY)?;
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
    let snapshot = postgres
        .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, ENV_KEY, &lookup)
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
    Ok(())
}
