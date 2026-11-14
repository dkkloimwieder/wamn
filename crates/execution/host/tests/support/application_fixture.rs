//! The fixture of the application host route tests: a project database with its
//! targeted logins and rows, a fixture release that serves the application set,
//! and a host that serves it to session callers
//! (docs/architecture/execution.md#host-run-routes).
//!
//! `host_route_live` and `crates/execution/host/tests`'s `control_client_live` include
//! it beside the shared session fixture, as `session_fixture`.

use sqlx::PgConnection as Client;
use sqlx::Row as _;
use std::sync::Arc;
use wamn_broker::Publisher;

use serde_json::json;

use wamn_authentication::route_authentication::PlatformRouteAuthenticator;
use wamn_catalog::{HostRouteSet, SERVING_MANIFEST_FORMAT_VERSION};
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_postgres_capability::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

use super::session_fixture::{self, AUDIENCE, ORG, Server, claims};

pub(super) const PROJECT: &str = "project";
pub(super) const PROJECT_ENV_KEY: &str = "org-a--dev--project";
pub(super) const READ: &str = "session-test:purchase/read@1.0.0";
pub(super) const WRITE: &str = "session-test:purchase/write@1.0.0";
pub(super) const MEMBER: &str = "34f2085c-19d8-474f-b87a-2a29a5357b9b";
pub(super) const PASSWORD: &str = "host-route-test-only";

pub(super) async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

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
    Ok(workload_login(
        family,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: "dev",
            database,
        },
    )?)
}

/// The project database, its two targeted logins, and the fixture rows: the
/// first principal holds `admin`, and the member holds one authored role.
pub(super) async fn install(
    admin: &mut Client,
    database: &str,
    admin_url: &str,
) -> anyhow::Result<(String, String)> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_app NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
              \
             CREATE SCHEMA wamn_run AUTHORIZATION postgres;")
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut *admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(include_str!("../../../../../deploy/sql/app-schema.sql"))
        .execute(&mut *admin)
        .await
        .map(drop)?;
    let admitter = generation(WorkloadRoleFamily::HttpAdmitter, database)?;
    let administration = generation(WorkloadRoleFamily::Administration, database)?;
    for (family, role) in [
        (WorkloadRoleFamily::HttpAdmitter, &admitter),
        (WorkloadRoleFamily::Administration, &administration),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::login_sql(family, database, role, PASSWORD))
                .to_string(),
        ))
        .execute(&mut *admin)
        .await
        .map(drop)?;
    }
    let first = claims()["sub"]
        .as_str()
        .expect("fixture principal")
        .to_owned();
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:seed-host-route-fixture', false)",
    )
    .bind(&first)
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())?;
    let mut writer = connect(&login(admin_url, &administration)?).await?;
    let identity =
        sqlx::query("SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname=current_user")
            .fetch_one(&mut writer)
            .await?;
    anyhow::ensure!(
        !identity.get::<bool, _>(0) && !identity.get::<bool, _>(1),
        "fixture configuration uses the administration login"
    );
    sqlx::query("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:seed-host-route-fixture',false)").bind(&first).execute(&mut writer).await.map(|result| result.rows_affected())?;
    for role in ["admin", "purchase-reader"] {
        sqlx::query("INSERT INTO platform.role (name) VALUES ($1)")
            .bind(&role)
            .execute(&mut writer)
            .await
            .map(|result| result.rows_affected())?;
    }
    sqlx::query(
        "INSERT INTO platform.permission (role_name, permission) \
             VALUES ('purchase-reader', 'session-test:purchase/read')",
    )
    .execute(&mut writer)
    .await
    .map(|result| result.rows_affected())?;
    for (principal, email, role) in [
        (first.as_str(), "first@example.test", "admin"),
        (MEMBER, "member@example.test", "purchase-reader"),
    ] {
        sqlx::query(
            "INSERT INTO platform.\"user\" (id, type, email) \
                 VALUES ($1::text::uuid, 'user', $2)",
        )
        .bind(&principal)
        .bind(&email)
        .execute(&mut writer)
        .await
        .map(|result| result.rows_affected())?;
        sqlx::query(
            "INSERT INTO platform.user_role (user_id, role_name) \
                 VALUES ($1::text::uuid, $2)",
        )
        .bind(&principal)
        .bind(&role)
        .execute(&mut writer)
        .await
        .map(|result| result.rows_affected())?;
    }
    Ok((admitter, administration))
}

/// The App generation of the fixture project_env_key, as provisioning prepares it:
/// the guest authority that `number_counter.set` runs under. Returns its
/// login name.
pub(super) async fn app_generation(admin: &mut Client, database: &str) -> anyhow::Result<String> {
    let role = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: PROJECT_ENV_KEY,
            database,
        },
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::App,
            database,
            &role,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    Ok(role)
}

/// The host's credentials: the admitter and the administration login, and
/// the guest login when the host serves `number_counter.set`.
pub(super) fn credentials(
    admitter: &str,
    administration: &str,
    guest: Option<&str>,
) -> anyhow::Result<Arc<WamnPostgres>> {
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2000,
        statement_timeout_ms: 5000,
        row_limit: 100,
    };
    let mut classes = json!({
        (AuthorityClass::CallableHttp.as_str()): admitter,
        (AuthorityClass::Administration.as_str()): administration,
    });
    if let Some(guest) = guest {
        classes[AuthorityClass::GuestSql.as_str()] = json!(guest);
    }
    let configuration = json!({PROJECT: {"credentials": classes}});
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
        "release": {"packages": [{"package-id": "session_test", "package-version": "1.0.0"}]},
        "components": [{"package-id": "session_test",
            "descriptor": {"component": "purchase", "interface-version": "0.1.0", "component-digest": format!("sha256:{}", "a".repeat(64)), "imports": [], "imports-fingerprint": "", "effects": [], "connections": [],
            "operations": {
                READ: {"registered-operation": READ, "input-ports": [], "output-ports": [], "parameters": []},
                WRITE: {"registered-operation": WRITE, "input-ports": [], "output-ports": [], "parameters": []}
            }}}],
        "routes": [
            {"package-id": "session_test", "component": "purchase", "operation": READ, "type": "get"},
            {"package-id": "session_test", "component": "purchase", "operation": WRITE, "type": "create"}
        ],
        "attachments": {},
        "host-routes": [HostRouteSet::Application],
    });
    // The manifest's own serialization leaves out its empty sections.
    let manifest: wamn_catalog::ServingManifest = serde_json::from_value(manifest)?;
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_digest::canonical_json_bytes(&serde_json::to_value(&manifest)?),
        "host route test",
        wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, "dev")
            .with_applications(serde_json::from_value(
                json!({"receiving": {"packages": ["session_test"]}}),
            )?),
    )?))
}

/// A host that serves the application set of the fixture release to session
/// callers, with the identity reader for the covering test. Its database
/// holds the two fixture users, and `first` holds `admin`.
pub(super) struct ApplicationHost {
    pub(super) admin: Client,
    pub(super) server: Server,
    pub(super) routing: Arc<FlowHttpRouting>,
    pub(super) delivery: Arc<HostRouteDelivery>,
    pub(super) first: String,
    _clocks: (
        wamn_identity_protocol::verifier::SessionTestClock,
        wamn_authentication::session_keys::TestClock,
    ),
    _database: wamn_test_postgres::OwnedDatabase,
    _postgres: wamn_test_postgres::OwnedPostgres,
}

pub(super) async fn application_host(name: &str) -> anyhow::Result<ApplicationHost> {
    Ok(application_host_with_release(name, load_release()?)
        .await?
        .0)
}

pub(super) async fn application_host_with_release(
    name: &str,
    release: Arc<LoadedRelease>,
) -> anyhow::Result<(ApplicationHost, Arc<WamnPostgres>, String)> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database(name)?;
    let admin_url = test_database.url();
    let mut admin = connect(admin_url).await?;
    let mut database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    let (admitter, administration) = install(&mut admin, &mut database, admin_url).await?;
    let guest = app_generation(&mut admin, &mut database).await?;
    let first = claims()["sub"].as_str().unwrap().to_owned();
    let server = Server::start().await;
    let (verifier, token_clock, key_clock) = server.verifier();
    session_fixture::install_authority(&mut admin, PROJECT, "dev", AUDIENCE, &[&first, MEMBER])
        .await?;
    // Application routes advertise PATs and sessions. Extend the session
    // authority fixture with the columns the real PAT reader prepares.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("ALTER TABLE identity.principals ADD COLUMN subject text, ADD COLUMN display_name text; \
         ALTER TABLE identity.pats ADD COLUMN token_prefix text, ADD COLUMN token_hash text")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::grant_identity_reader_surface_sql()).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE host_route_identity_reader LOGIN; \
             GRANT wamn_identity_reader TO host_route_identity_reader;")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    let reader = async || -> anyhow::Result<sqlx::PgPool> {
        let options = wamn_postgres::connect_options(admin_url).await?;
        Ok(sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::raw_sql("SET ROLE host_route_identity_reader")
                        .execute(connection)
                        .await
                        .map(drop)
                })
            })
            .connect_lazy_with(options))
    };
    let postgres_credentials = credentials(
        &login(admin_url, &admitter)?,
        &login(admin_url, &administration)?,
        Some(&login(admin_url, &guest)?),
    )?;
    let routing = FlowHttpRouting::for_test(Some(Arc::clone(&release))).with_authenticator(
        Arc::new(PlatformRouteAuthenticator::for_test(
            Arc::clone(&postgres_credentials),
            PROJECT_ENV_KEY,
            Some(reader().await?),
            Some(verifier),
        )),
    );
    let delivery = HostRouteDelivery::new(
        Arc::clone(&release),
        HostRouteHandlers::Application {
            postgres: Arc::clone(&postgres_credentials),
            project: PROJECT.to_owned(),
            identity: Some((reader().await?, ORG.to_owned())),
            changes: Publisher::default(),
        },
    );
    let administration_url = login(admin_url, &administration)?;
    Ok((
        ApplicationHost {
            admin,
            server,
            routing: Arc::new(routing),
            delivery: Arc::new(delivery),
            first,
            _clocks: (token_clock, key_clock),
            _database: test_database,
            _postgres: postgres,
        },
        postgres_credentials,
        administration_url,
    ))
}
