//! The fixture of the application host route tests: a project database with
//! its scoped logins and rows, a fixture release that serves the
//! application set, and a host that serves it to session callers
//! (docs/plan/platform-ui.md §4.6).
//!
//! `host_route_live` and `tests/integration`'s `control_client_live` include
//! it beside the shared session fixture, as `session_fixture`.

use std::sync::Arc;

use serde_json::json;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{HostRouteSet, SERVING_MANIFEST_FORMAT_VERSION};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_runtime::plugins::route_authentication::{
    PlatformRouteAuthenticator, SessionRouteAuthentication,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

use super::session_fixture::{self, AUDIENCE, ORG, Server, claims};

pub(super) const PROJECT: &str = "project";
pub(super) const TENANT: &str = "tenant-a";
pub(super) const READ: &str = "session-test:purchase/read@1.0.0";
pub(super) const WRITE: &str = "session-test:purchase/write@1.0.0";
pub(super) const MEMBER: &str = "34f2085c-19d8-474f-b87a-2a29a5357b9b";
pub(super) const PASSWORD: &str = "host-route-test-only";

pub(super) async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("fixture database connection") });
    Ok(client)
}

pub(super) fn login(admin_url: &str, role: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(admin_url)?;
    url.set_username(role)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    Ok(url.into())
}

pub(super) fn generation(family: WorkloadRoleFamily, database: &str) -> anyhow::Result<String> {
    Ok(workload_generation_role(
        family,
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: "dev",
            database,
        },
        CredentialGeneration::A,
    )?)
}

/// The project database, its two scoped logins, and the fixture rows: the
/// first principal holds `admin`, and the member holds one authored role.
pub(super) async fn install(admin: &Client, database: &str) -> anyhow::Result<(String, String)> {
    admin
        .batch_execute(
            "CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
             CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
             CREATE SCHEMA wamn_run AUTHORIZATION postgres;",
        )
        .await?;
    admin
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await?;
    admin
        .batch_execute(include_str!("../../../../../deploy/sql/app-schema.sql"))
        .await?;
    let admitter = generation(WorkloadRoleFamily::HttpAdmitter, database)?;
    let administration = generation(WorkloadRoleFamily::Administration, database)?;
    for (family, role) in [
        (WorkloadRoleFamily::HttpAdmitter, &admitter),
        (WorkloadRoleFamily::Administration, &administration),
    ] {
        admin
            .batch_execute(&sql::prepare_workload_generation_sql(
                family,
                database,
                role,
                PASSWORD,
                "2099-01-01T00:00:00Z",
            ))
            .await?;
    }
    let first = claims()["sub"]
        .as_str()
        .expect("fixture principal")
        .to_owned();
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:seed-host-route-fixture', false)",
            &[&first],
        )
        .await?;
    for role in ["admin", "purchase-reader"] {
        admin
            .execute(
                "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, $2)",
                &[&TENANT, &role],
            )
            .await?;
    }
    admin
        .execute(
            "INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) \
             VALUES ($1, 'purchase-reader', 'session-test:purchase/read', 'session-test:purchase/read')",
            &[&TENANT],
        )
        .await?;
    for (principal, email, role) in [
        (first.as_str(), "first@example.test", "admin"),
        (MEMBER, "member@example.test", "purchase-reader"),
    ] {
        admin
            .execute(
                "INSERT INTO app_system.users (tenant_id, id, type, email) \
                 VALUES ($1, $2::text::uuid, 'user', $3)",
                &[&TENANT, &principal, &email],
            )
            .await?;
        admin
            .execute(
                "INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) \
                 VALUES ($1, $2::text::uuid, $3)",
                &[&TENANT, &principal, &role],
            )
            .await?;
    }
    Ok((admitter, administration))
}

pub(super) fn credentials(
    admitter: &str,
    administration: &str,
) -> anyhow::Result<Arc<WamnPostgres>> {
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2000,
        statement_timeout_ms: 5000,
        row_limit: 100,
    };
    let configuration = json!({PROJECT: {"credentials": {
        (AuthorityClass::CallableHttp.as_str()): admitter,
        (AuthorityClass::Administration.as_str()): administration,
    }}});
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    Ok(Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(projects, None),
    ))))
}

/// A release of one component with two registered operations that serves
/// the application host route set.
pub(super) fn load_release() -> anyhow::Result<Arc<LoadedRelease>> {
    let manifest = json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"tenant-id": TENANT, "effective-release-id": 1, "environment": "dev",
            "packages": [{"package-id": "session_test", "package-version": "1.0.0"}]},
        "components": [{"package-id": "session_test", "component": "purchase", "interface-version": "0.1.0",
            "digest": format!("sha256:{}", "a".repeat(64)), "operations": {
                READ: {"registered-operation": READ, "permissions": [READ]},
                WRITE: {"registered-operation": WRITE, "permissions": [WRITE, READ]}
            }}],
        "routes": [
            {"package-id": "session_test", "component": "purchase", "operation": READ, "type": "get"},
            {"package-id": "session_test", "component": "purchase", "operation": WRITE, "type": "create"}
        ],
        "attachments": {},
        "workflow": {"wirings": [], "attachments": {}},
        "host-routes": [HostRouteSet::Application],
    });
    // The manifest's own serialization leaves out its empty sections.
    let manifest: wamn_catalog::ServingManifest = serde_json::from_value(manifest)?;
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_execution_contract::canonical_json_bytes(&serde_json::to_value(&manifest)?),
        "host route test",
    )?))
}

/// A host that serves the application set of the fixture release to session
/// callers, with the identity reader for the covering check. Its database
/// holds the two fixture users, and `first` holds `admin`.
pub(super) struct ApplicationHost {
    pub(super) admin: Client,
    pub(super) server: Server,
    pub(super) routing: Arc<FlowHttpRouting>,
    pub(super) delivery: Arc<HostRouteDelivery>,
    pub(super) first: String,
    _clocks: (
        wamn_session::verifier::SessionTestClock,
        wamn_runtime::session_keys::TestClock,
    ),
    _database: wamn_test_postgres::OwnedDatabase,
    _postgres: wamn_test_postgres::OwnedPostgres,
}

pub(super) async fn application_host(name: &str) -> anyhow::Result<ApplicationHost> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database(name)?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let (admitter, administration) = install(&admin, &database).await?;
    let first = claims()["sub"].as_str().unwrap().to_owned();
    let server = Server::start().await;
    let (verifier, token_clock, key_clock) = server.verifier();
    session_fixture::install_authority(&admin, PROJECT, "dev", AUDIENCE, &[&first, MEMBER]).await?;
    admin
        .batch_execute(&sql::grant_identity_reader_surface_sql())
        .await?;
    admin
        .batch_execute(
            "CREATE ROLE host_route_identity_reader LOGIN; \
             GRANT wamn_identity_reader TO host_route_identity_reader;",
        )
        .await?;
    let reader = async || -> anyhow::Result<Arc<Client>> {
        let client = connect(admin_url).await?;
        client
            .batch_execute("SET ROLE host_route_identity_reader")
            .await?;
        Ok(Arc::new(client))
    };
    let postgres_credentials = credentials(
        &login(admin_url, &admitter)?,
        &login(admin_url, &administration)?,
    )?;
    let release = load_release()?;
    let routing = FlowHttpRouting::new(Some(Arc::clone(&release)), RouteInFlightLimit::default())
        .with_authenticator(Arc::new(
            PlatformRouteAuthenticator::default().with_session_authentication(Arc::new(
                SessionRouteAuthentication::new(
                    verifier,
                    reader().await?,
                    Arc::clone(&postgres_credentials),
                    PROJECT,
                ),
            )),
        ));
    let delivery = HostRouteDelivery::new(
        Arc::clone(&release),
        HostRouteHandlers::Application {
            postgres: postgres_credentials,
            project: PROJECT.to_owned(),
            identity: Some((reader().await?, ORG.to_owned())),
        },
        None,
    );
    Ok(ApplicationHost {
        admin,
        server,
        routing: Arc::new(routing),
        delivery: Arc::new(delivery),
        first,
        _clocks: (token_clock, key_clock),
        _database: test_database,
        _postgres: postgres,
    })
}
