//! The route plugin refuses every route of an inactive environment
//! (docs/architecture/execution.md#routing-and-availability, wamn-zua8.2). The
//! host reads its row of `platform.environment` as a project's `wamn_app`
//! generation, before the route's policy, so an anonymous route is refused too.
//! The host keeps the answer only while it watches the auth bucket
//! (wamn-stxy9.2, wamn-f2qud).
//!
//! This test starts its own PostgreSQL 18 server.
#![cfg(feature = "test-util")]

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _, Statement as _};
use std::sync::Arc;

use serde_json::{Value, json};

use wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_runtime::plugins::route_authentication::{
    EnvironmentStatusReader, PlatformRouteAuthenticator,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

const PROJECT: &str = "project";
const PROJECT_ENV_KEY: &str = "org-a--dev--project";
const ROUTE: &str = "purchase-read-http";
const READ: &str = "status-test:purchase/read@1.0.0";
const PASSWORD: &str = "environment-status-test-only";
const ACTOR: &str = "11111111-1111-4111-8111-111111111111";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let mut client = wamn_postgres::connect(url).await?;

    Ok(client)
}

/// One anonymous route of the project's release.
fn release() -> anyhow::Result<Arc<LoadedRelease>> {
    let definition = json!({"id": ROUTE, "type": "http", "route": {
        "host": "purchase.example.test", "path": "/purchase/read", "method": "POST"
    }});
    let wiring_route = json!({"id": "purchase-http", "type": "http", "route": {
        "host": "purchase.example.test", "path": "/purchase", "method": "POST"
    }});
    let attachment: Value = json!({"type": "http", "package-id": "status_test",
        "component": "purchase", "operation": READ,
        "definition-hash": wamn_digest::canonical_json_sha256(&definition),
        "definition": definition, "auth-policy": {"modes": ["none"]}});
    let manifest = json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [{"package-id": "status_test", "package-version": "1.0.0"}]},
        "components": [{"package-id": "status_test",
            "descriptor": {"component": "purchase", "interface-version": "0.1.0",
                "component-digest": format!("sha256:{}", "a".repeat(64)), "operations": {READ: {"registered-operation": READ, "input-ports": [], "output-ports": [], "parameters": []}},
                "imports": [], "imports-fingerprint": "", "effects": [], "connections": []}}],
        "routes": [{"package-id": "status_test", "component": "purchase", "operation": READ,
            "type": "get"}],
        "attachments": {ROUTE: attachment, "purchase-http": {"type":"http","package-id":"status_test",
            "component":"purchase","operation":READ,
            "definition-hash":wamn_digest::canonical_json_sha256(&wiring_route),
            "definition":wiring_route,"auth-policy":{"modes":["none"]}}}

    });
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_digest::canonical_json_bytes(&manifest),
        "environment status test",
        wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, "dev")
            .with_applications(
                [(
                    "purchasing".to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        connections: std::collections::BTreeMap::new(),
                        packages: ["status_test".to_owned()].into(),
                    },
                )]
                .into(),
            ),
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
    let mut admin = connect(admin_url).await?;
    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    let generation = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: PROJECT_ENV_KEY,
            database: &database,
        },
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_app NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
             \
             CREATE SCHEMA wamn_run AUTHORIZATION postgres;")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(include_str!("../../../../deploy/sql/app-schema.sql"))
        .execute(&mut admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::App,
            &database,
            &generation,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
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
    let route = FlowHttpRouting::for_test(Some(release()?))
        .with_authenticator(Arc::new(PlatformRouteAuthenticator::for_test(
            Arc::clone(&reader),
            "host-project",
            None,
            None,
        )))
        .with_environment_status(Arc::new(EnvironmentStatusReader::new(Arc::clone(&reader))));

    // No row means active, and an anonymous route admits no caller.
    assert_eq!(admitted(&route).await, Ok(false));
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:environment-status-fixture', false)",
    )
    .bind(&ACTOR)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO platform.environment (status) VALUES ('inactive')")
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    // This host watches no auth bucket, so it keeps no answer and reads the
    // row on every request (wamn-f2qud).
    assert_eq!(
        admitted(&route).await,
        Err((503, "environment-inactive".to_owned()))
    );
    sqlx::query("UPDATE platform.environment SET status = 'active'")
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    assert_eq!(
        admitted(&route).await,
        Ok(false),
        "activation is reversible"
    );
    Ok(())
}
