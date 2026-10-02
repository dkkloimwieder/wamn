//! The route plugin refuses every route of an inactive environment
//! (docs/plan/platform-ui.md §5.4, wamn-zua8.2). The host reads its row of
//! `app_system.environment` as a tenant's `wamn_app` generation on each
//! request, before the route's policy, so an anonymous route is refused too.
//!
//! This test starts its own PostgreSQL 18 server.
#![cfg(feature = "test-util")]

use std::sync::Arc;

use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_runtime::plugins::route_authentication::{
    EnvironmentStatusReader, PlatformRouteAuthenticator,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

const PROJECT: &str = "project";
const TENANT: &str = "tenant-a";
const ROUTE: &str = "purchase-read-http";
const READ: &str = "status-test:purchase/read@1.0.0";
const PASSWORD: &str = "environment-status-test-only";
const ACTOR: &str = "11111111-1111-4111-8111-111111111111";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("fixture database connection") });
    Ok(client)
}

/// One anonymous route of the tenant's release.
fn release() -> anyhow::Result<Arc<LoadedRelease>> {
    let definition = json!({"id": ROUTE, "kind": "http", "route": {
        "host": "purchase.example.test", "path": "/purchase/read", "method": "POST"
    }});
    let wiring_route = json!({"id": "purchase-http", "kind": "http", "route": {
        "host": "purchase.example.test", "path": "/purchase", "method": "POST"
    }});
    let attachment: Value = json!({"type": "http", "package-id": "status_test",
        "component": "purchase", "operation": READ,
        "definition-hash": wamn_execution_contract::canonical_json_sha256(&definition),
        "definition": definition, "auth-policy": {"modes": ["none"]}});
    let manifest = json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"tenant-id": TENANT, "effective-release-id": 1, "environment": "dev",
            "packages": [{"package-id": "status_test", "package-version": "1.0.0"}]},
        "components": [{"package-id": "status_test", "component": "purchase",
            "interface-version": "0.1.0", "digest": format!("sha256:{}", "a".repeat(64)),
            "operations": {READ: {"registered-operation": READ, "permissions": [READ]}}}],
        "routes": [{"package-id": "status_test", "component": "purchase", "operation": READ,
            "type": "get"}],
        "attachments": {ROUTE: attachment},
        "workflow": {
            "wirings": [{"package-id": "status_test", "wiring-id": "purchase", "wiring-version": 1,
                "graph-hash": format!("sha256:{}", "b".repeat(64))}],
            "attachments": {"purchase-http": {"type": "http", "package-id": "status_test",
                "wiring-id": "purchase", "wiring-version": 1,
                "definition-hash": wamn_execution_contract::canonical_json_sha256(&wiring_route),
                "definition": wiring_route, "auth-policy": {"modes": ["none"]}}}
        }
    });
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_execution_contract::canonical_json_bytes(&manifest),
        "environment status test",
    )?))
}

/// The answer of the route plugin to one anonymous request: whether it
/// admitted a caller, or its refusal.
async fn admitted(route: &FlowHttpRouting) -> Result<bool, (u16, String)> {
    route
        .authenticate_authorization_for_test(ROUTE, None)
        .await
        .map(|caller| caller.is_some())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inactive_environment_refuses_every_route() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("environment_status")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let generation = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: &database,
        },
        CredentialGeneration::A,
    )?;
    admin
        .batch_execute(
            "CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOBYPASSRLS; \
             CREATE SCHEMA wamn_run AUTHORIZATION postgres;",
        )
        .await?;
    admin
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await?;
    admin
        .batch_execute(include_str!("../../../../deploy/sql/app-schema.sql"))
        .await?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::App,
            &database,
            &generation,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    let mut url = url::Url::parse(admin_url)?;
    url.set_username(&generation)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2000,
        statement_timeout_ms: 5000,
        row_limit: 100,
    };
    let configuration =
        json!({PROJECT: {"credentials": {(AuthorityClass::GuestSql.as_str()): url.as_str()}}});
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    let reader = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(projects, None),
    )));
    let route = FlowHttpRouting::new(Some(release()?), RouteInFlightLimit::default())
        .with_authenticator(Arc::new(PlatformRouteAuthenticator::default()))
        .with_environment_status(Arc::new(EnvironmentStatusReader::new(
            reader,
            PROJECT.to_owned(),
        )));

    // No row means active, and an anonymous route admits no caller.
    assert_eq!(admitted(&route).await, Ok(false));
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:environment-status-fixture', false)",
            &[&ACTOR],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO app_system.environment (tenant_id, status) VALUES ($1, 'inactive')",
            &[&TENANT],
        )
        .await?;
    assert_eq!(
        admitted(&route).await,
        Err((503, "environment-inactive".to_owned()))
    );
    admin
        .execute("UPDATE app_system.environment SET status = 'active'", &[])
        .await?;
    assert_eq!(
        admitted(&route).await,
        Ok(false),
        "activation is reversible"
    );
    Ok(())
}
