//! Real authorizeded permission reads and local HTTPS keys cross the route-authentication boundary.
//!
//! This test starts its own PostgreSQL 18 server with
//! `shared_preload_libraries=pg_stat_statements` and `pg_stat_statements.track=all`.
//! It does not start deployed hosts or show nested registered-operation guards.
#![cfg(feature = "test-util")]

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _, Statement as _};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use wamn_broker::auth_changes::AuthDrops;

use anyhow::Context as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType, FlowHttpRouting};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_runtime::plugins::route_authentication::{
    PlatformRouteAuthenticator, SessionRouteAuthentication,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

#[path = "support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also provides verifier-only fetch barriers."
)]
mod session_fixture;
use session_fixture::{AUDIENCE, ORG, Server, claims, header, signed};

const PROJECT: &str = "project";
const PROJECT_ENV_KEY: &str = "org-a";
const ATTACHMENT: &str = "purchase-http";
const READ_ROUTE: &str = "purchase-read-http";
const WRITE_ROUTE: &str = "purchase-write-http";
const READ: &str = "session-test:purchase/read@1.0.0";
const WRITE: &str = "session-test:purchase/write@1.0.0";
const OTHER_PROJECT_ENV_KEY: &str = "session-test:secret/read@1.0.0";
/// An operation the first release does not serve and a later one does.
const APPROVE: &str = "session-test:purchase/approve@1.0.0";
const SECOND_PRINCIPAL: &str = "34f2085c-19d8-474f-b87a-2a29a5357b9b";
const PASSWORD: &str = "session-route-test-only";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let mut client = wamn_postgres::connect(url).await?;

    Ok(client)
}

async fn install(admin: &mut Client, database: &str, generation: &str) -> anyhow::Result<()> {
    let version: i32 = sqlx::query("SHOW server_version_num")
        .fetch_one(&mut *admin)
        .await?
        .get::<String, _>(0)
        .parse()?;
    anyhow::ensure!(
        version >= 180_000,
        "the test requires PostgreSQL 18 or newer"
    );
    let occupied: bool = sqlx::query("SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname IN ('platform', 'catalog', 'identity', 'wamn_run')) \
         OR EXISTS (SELECT FROM pg_roles WHERE rolname IN ('wamn_app', 'wamn_http_admitter'))").fetch_one(&mut *admin).await?.get(0);
    anyhow::ensure!(
        !occupied,
        "refuse a populated server: arm only this test's fresh disposable PostgreSQL server"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE EXTENSION pg_stat_statements; \
         CREATE ROLE wamn_app NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
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
    sqlx::raw_sql(include_str!("../../../../deploy/sql/app-schema.sql"))
        .execute(&mut *admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::HttpAdmitter,
            database,
            generation,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    Ok(())
}

async fn seed(admin: &mut Client) -> anyhow::Result<()> {
    let first = claims()["sub"]
        .as_str()
        .expect("fixture principal")
        .to_owned();
    // The fixture writes as its first principal, whose row stamps itself.
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:seed-session-route-fixture', false)",
    )
    .bind(&first)
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())?;
    for role in ["purchase-reader", "purchase-writer"] {
        sqlx::query("INSERT INTO platform.role (name) VALUES ($1)")
            .bind(&role)
            .execute(&mut *admin)
            .await
            .map(|result| result.rows_affected())?;
    }
    for (role, permission) in [
        ("purchase-reader", READ),
        ("purchase-writer", READ),
        ("purchase-writer", WRITE),
    ] {
        sqlx::query(
            "INSERT INTO platform.permission (role_name, permission) \
             VALUES ($1, regexp_replace($2, '@[^@]*$', ''))",
        )
        .bind(&role)
        .bind(&permission)
        .execute(&mut *admin)
        .await
        .map(|result| result.rows_affected())?;
    }
    for (principal, email) in [
        (first.as_str(), "first@example.test"),
        (SECOND_PRINCIPAL, "second@example.test"),
    ] {
        sqlx::query(
            "INSERT INTO platform.\"user\" (id, type, email) VALUES ($1::text::uuid, 'user', $2)",
        )
        .bind(&principal)
        .bind(&email)
        .execute(&mut *admin)
        .await
        .map(|result| result.rows_affected())?;
        sqlx::query("INSERT INTO platform.user_role (user_id, role_name) VALUES ($1::text::uuid, 'purchase-reader')").bind(&principal).execute(&mut *admin).await.map(|result| result.rows_affected())?;
    }
    sqlx::query("INSERT INTO platform.user_role (user_id, role_name) VALUES ($1::text::uuid, 'purchase-writer')").bind(&first).execute(&mut *admin).await.map(|result| result.rows_affected())?;
    Ok(())
}

/// A permission reader over the callable-HTTP login at `url`.
fn permission_reader(url: &str) -> anyhow::Result<Arc<WamnPostgres>> {
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2000,
        statement_timeout_ms: 5000,
        row_limit: 100,
    };
    let configuration =
        json!({PROJECT: {"credentials": {(AuthorityClass::CallableHttp.as_str()): url}}});
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    Ok(Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(projects, None),
    ))))
}

/// One HTTP attachment that targets the route of `operation` directly.
fn route_attachment(id: &str, path: &str, operation: &str, modes: &[&str]) -> Value {
    let definition = json!({"id": id, "type": "http", "route": {
        "host": "purchase.example.test", "path": path, "method": "POST"
    }});
    json!({"type": "http", "package-id": "session_test", "component": "purchase",
        "operation": operation, "definition-hash": wamn_digest::canonical_json_sha256(&definition),
        "definition": definition, "auth-policy": {"modes": modes}, "registered-operation": operation})
}

fn load_release(modes: &[&str]) -> anyhow::Result<Arc<LoadedRelease>> {
    load_release_serving(modes, &[READ, WRITE])
}

/// The fixture release, whose component registers each of `operations`.
fn load_release_serving(modes: &[&str], operations: &[&str]) -> anyhow::Result<Arc<LoadedRelease>> {
    let operations: serde_json::Map<String, Value> = operations
        .iter()
        .map(|operation| {
            (
                (*operation).to_owned(),
                json!({"registered-operation": operation, "input-ports": [], "output-ports": [], "parameters": []}),
            )
        })
        .collect();
    let definition = json!({"id": ATTACHMENT, "type": "http", "route": {
        "host": "purchase.example.test", "path": "/purchase", "method": "POST"
    }});
    let manifest = json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [{"package-id": "session_test", "package-version": "1.0.0"}]},
        "components": [{"package-id": "session_test",
            "descriptor": {"component": "purchase", "interface-version": "0.1.0", "component-digest": format!("sha256:{}", "a".repeat(64)), "operations": operations, "imports": [], "imports-fingerprint": "", "effects": [], "connections": []}}],
        "routes": [
            {"package-id": "session_test", "component": "purchase", "operation": READ, "type": "get"},
            {"package-id": "session_test", "component": "purchase", "operation": WRITE, "type": "create"}
        ],
        "attachments": {
            READ_ROUTE: route_attachment(READ_ROUTE, "/purchase/read", READ, modes),
            WRITE_ROUTE: route_attachment(WRITE_ROUTE, "/purchase/write", WRITE, modes),
            ATTACHMENT: {"type":"http","package-id":"session_test","component":"purchase","operation":READ,
                "definition-hash":wamn_digest::canonical_json_sha256(&definition),
                "definition":definition,"auth-policy":{"modes":modes},"registered-operation":READ}}

    });
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_digest::canonical_json_bytes(&manifest),
        "session route test",
        wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, "dev")
            .with_applications(
                [(
                    "purchasing".to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        connections: std::collections::BTreeMap::new(),
                        packages: ["session_test".to_owned()].into(),
                    },
                )]
                .into(),
            ),
    )?))
}

fn routing(
    authentication: Arc<SessionRouteAuthentication>,
    modes: &[&str],
) -> anyhow::Result<FlowHttpRouting> {
    Ok(
        FlowHttpRouting::new(Some(load_release(modes)?)).with_authenticator(Arc::new(
            PlatformRouteAuthenticator::default().with_session_authentication(authentication),
        )),
    )
}

async fn accepted(route: &FlowHttpRouting, body: &Value) -> AuthenticatedCaller {
    route
        .authenticate_authorization_for_test(
            ATTACHMENT,
            Some(&format!("Bearer {}", signed(&header(), body))),
        )
        .await
        .expect("session route authentication")
        .expect("host-owned caller")
}

async fn statements(admin: &mut Client, generation: &str) -> anyhow::Result<BTreeMap<String, i64>> {
    sqlx::query("SELECT query, calls::bigint FROM pg_stat_statements \
         WHERE userid = (SELECT oid FROM pg_roles WHERE rolname = $1) \
         AND dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY query").bind(&generation).fetch_all(&mut *admin).await?.into_iter().map(|row| Ok((row.try_get(0)?, row.try_get(1)?))).collect()
}

fn assert_permission_reads(
    before: &BTreeMap<String, i64>,
    after: &BTreeMap<String, i64>,
    expected: i64,
) {
    let changes = after
        .iter()
        .filter_map(|(query, calls)| {
            let delta = calls - before.get(query).copied().unwrap_or(0);
            (delta != 0).then_some((query, delta))
        })
        .collect::<Vec<_>>();
    if expected == 0 {
        assert!(
            changes.is_empty(),
            "refused credentials must perform no project SQL: {changes:?}"
        );
        return;
    }
    assert_eq!(
        changes.len(),
        1,
        "only the single permission statement executes: {changes:?}"
    );
    let (_, calls) = changes[0];
    assert_eq!(calls, expected, "one permission read per admitted request");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_read_permissions_per_request_and_preserve_the_signed_identity()
-> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[
        ("shared_preload_libraries", "pg_stat_statements"),
        ("pg_stat_statements.track", "all"),
    ])?;
    let test_database = postgres.create_database("session_route")?;
    let admin_url = test_database.url();
    let mut admin = connect(admin_url).await?;
    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    let generation = workload_login(
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: "dev",
            database: &database,
        },
    )?;
    install(&mut admin, &database, &generation).await?;
    seed(&mut admin).await?;
    let other_database = postgres.create_database("session_route_other")?;
    let mut other_admin = connect(other_database.url()).await?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut other_admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(include_str!("../../../../deploy/sql/app-schema.sql"))
        .execute(&mut other_admin)
        .await
        .map(drop)?;
    seed(&mut other_admin).await?;
    sqlx::query("INSERT INTO platform.permission (role_name, permission) VALUES ('purchase-reader', regexp_replace($1, '@[^@]*$', ''))").bind(&OTHER_PROJECT_ENV_KEY).execute(&mut other_admin).await.map(|result| result.rows_affected())?;

    let mut url = url::Url::parse(admin_url)?;
    url.set_username(&generation)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    let mut authorizeded = connect(url.as_str()).await?;
    let identity = sqlx::query(
        "SELECT session_user::text, current_user::text, rolsuper, rolbypassrls, \
         pg_has_role(current_user, 'wamn_http_admitter', 'USAGE'), \
         has_table_privilege(current_user, 'platform.permission', 'UPDATE') \
         FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&mut authorizeded)
    .await?;
    assert_eq!(identity.get::<String, _>(0), generation);
    assert_eq!(identity.get::<String, _>(1), generation);
    assert!(!identity.get::<bool, _>(2) && !identity.get::<bool, _>(3));
    assert!(identity.get::<bool, _>(4));
    assert!(
        !identity.get::<bool, _>(5),
        "runtime receives no permission writes"
    );
    drop(authorizeded);

    let mut server = Server::start().await;
    let (verifier, token_clock, key_clock) = server.verifier();
    session_fixture::install_authority(
        &mut admin,
        PROJECT,
        "dev",
        AUDIENCE,
        &[claims()["sub"].as_str().unwrap(), SECOND_PRINCIPAL],
    )
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::grant_identity_reader_surface_sql()).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql("CREATE ROLE session_identity_reader LOGIN; GRANT wamn_identity_reader TO session_identity_reader;").execute(&mut admin).await.map(drop)?;
    let identity_reader = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE session_identity_reader")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_lazy_with(wamn_postgres::connect_options(admin_url).await?);
    // This reader watches no auth bucket, so it keeps no auth answer and
    // reads every request (wamn-f2qud).
    let reader = permission_reader(url.as_str())?;
    let authentication = Arc::new(SessionRouteAuthentication::new(
        verifier.clone(),
        identity_reader.clone(),
        Arc::clone(&reader),
        PROJECT,
    ));
    let route = routing(
        Arc::<SessionRouteAuthentication>::clone(&authentication),
        &["session"],
    )?;
    let mixed = routing(authentication, &["pat", "session"])?;
    let mut first = claims();
    first["roles"] = json!(["purchase-reader", "purchase-writer"]);
    let mut second = claims();
    second["sub"] = json!(SECOND_PRINCIPAL);
    second["authority"] = json!({"login": SECOND_PRINCIPAL});
    accepted(&route, &first).await;
    let before = statements(&mut admin, &generation).await?;
    let admitted = accepted(&route, &first).await;
    assert_eq!(admitted.principal_id(), first["sub"].as_str().unwrap());
    assert_eq!(admitted.credential_type(), CredentialType::Session);
    assert_eq!(admitted.clone().credential_type(), CredentialType::Session);
    assert!(admitted.permits(READ) && admitted.permits(WRITE));
    assert!(
        !admitted.permits(OTHER_PROJECT_ENV_KEY),
        "same role name cannot cross the environment database"
    );
    let other = accepted(&route, &second).await;
    assert_eq!(other.principal_id(), SECOND_PRINCIPAL);
    assert!(other.permits(READ) && !other.permits(WRITE));
    assert_eq!(
        accepted(&mixed, &first).await.credential_type(),
        CredentialType::Session
    );
    let mut empty = first.clone();
    empty["roles"] = json!([]);
    let caller = accepted(&route, &empty).await;
    assert!(
        !caller.permits(READ) && !caller.permits(WRITE) && !caller.permits(OTHER_PROJECT_ENV_KEY)
    );
    empty["roles"] = json!(["unknown-role"]);
    assert_eq!(
        route
            .authenticate_authorization_for_test(
                ATTACHMENT,
                Some(&format!("Bearer {}", signed(&header(), &empty)))
            )
            .await
            .unwrap_err(),
        (401, "unauthorized".into())
    );
    // Each admitted or refused request reads its current grants once.
    assert_permission_reads(&before, &statements(&mut admin, &generation).await?, 5);

    // A grant stores a reference the loaded release does not serve. It holds
    // on a host whose release serves it, with no write in between.
    let granted = wamn_platform_identity::application::mutate_grant(
        &mut admin,
        PROJECT_ENV_KEY,
        wamn_platform_identity::application::GrantAuthority::Provisioning,
        wamn_platform_identity::application::GrantChange::Permission {
            role: "purchase-writer",
            reference: "session-test:purchase/approve",
            grant: true,
        },
        &mut AuthDrops::default(),
    )
    .await?;
    assert_eq!(granted.rows_added, 1, "the unserved reference is stored");
    let serving = load_release_serving(&["session"], &[READ, WRITE, APPROVE])?;
    let later = FlowHttpRouting::new(Some(Arc::clone(&serving))).with_authenticator(Arc::new(
        PlatformRouteAuthenticator::default().with_session_authentication(Arc::new(
            SessionRouteAuthentication::new(
                verifier,
                identity_reader,
                permission_reader(url.as_str())?,
                PROJECT,
            ),
        )),
    ));
    assert!(
        accepted(&later, &first).await.permits(APPROVE),
        "the stored reference holds once a release that serves it loads"
    );
    assert!(
        !accepted(&later, &second).await.permits(APPROVE),
        "the reference holds only for the roles that hold it"
    );

    // A browser sends the signed token in its cookie and the CSRF token in a header.
    // The admitted request reads once, and the refused one reads nothing.
    let before = statements(&mut admin, &generation).await?;
    let csrf = "fixture-csrf-token";
    let mut carried = first.clone();
    carried["csrf"] = json!(hex::encode(Sha256::digest(csrf.as_bytes())));
    let cookie = format!(
        "theme=dark; __Host-wamn-session={}",
        signed(&header(), &carried)
    );
    let by_cookie = route
        .authenticate_headers_for_test(WRITE_ROUTE, &[("cookie", &cookie), ("x-wamn-csrf", csrf)])
        .await
        .expect("a cookie with its CSRF header is admitted")
        .expect("host-owned caller");
    assert_eq!(by_cookie.principal_id(), first["sub"].as_str().unwrap());
    assert_eq!(by_cookie.credential_type(), CredentialType::Session);
    assert!(by_cookie.permits(READ) && by_cookie.permits(WRITE));
    assert_eq!(
        route
            .authenticate_headers_for_test(WRITE_ROUTE, &[("cookie", &cookie)])
            .await
            .unwrap_err(),
        (401, "unauthorized".into()),
        "a cookie without its CSRF header refuses"
    );
    assert_permission_reads(&before, &statements(&mut admin, &generation).await?, 1);
    assert_eq!(
        server.count(),
        1,
        "warm sessions perform no additional JWKS request"
    );

    // A read route skips the CSRF header, because a read changes nothing. A
    // write route still requires it.
    let read = route
        .authenticate_headers_for_test(READ_ROUTE, &[("cookie", &cookie)])
        .await
        .expect("a cookie read without its CSRF header is admitted")
        .expect("host-owned caller");
    assert_eq!(read.credential_type(), CredentialType::Session);
    assert_eq!(
        route
            .authenticate_headers_for_test(WRITE_ROUTE, &[("cookie", &cookie)])
            .await
            .unwrap_err(),
        (401, "unauthorized".into()),
        "a cookie write without its CSRF header refuses"
    );

    let before = statements(&mut admin, &generation).await?;
    sqlx::query(
        "DELETE FROM platform.permission \
             WHERE permission = regexp_replace($1, '@[^@]*$', '')",
    )
    .bind(&WRITE)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    let changed = accepted(&route, &first).await;
    assert!(
        changed.permits(READ) && !changed.permits(WRITE),
        "permission removal applies on the next request"
    );
    assert!(
        admitted.permits(WRITE),
        "permission change does not cancel already-admitted work"
    );
    sqlx::query(
        "INSERT INTO platform.permission (role_name, permission) \
         VALUES ('purchase-writer', regexp_replace($1, '@[^@]*$', ''))",
    )
    .bind(&WRITE)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query("DELETE FROM platform.user_role ")
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    sqlx::query("UPDATE platform.\"user\" SET status = 'disabled'")
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    assert_eq!(
        route
            .authenticate_authorization_for_test(
                ATTACHMENT,
                Some(&format!("Bearer {}", signed(&header(), &first)))
            )
            .await
            .unwrap_err(),
        (401, "unauthorized".into()),
        "removed roles and disabled users refuse the next admission"
    );
    assert_permission_reads(&before, &statements(&mut admin, &generation).await?, 2);
    sqlx::raw_sql("UPDATE platform.\"user\" SET status = 'active'")
        .execute(&mut admin)
        .await
        .map(drop)?;
    for principal in [first["sub"].as_str().unwrap(), SECOND_PRINCIPAL] {
        sqlx::query("INSERT INTO platform.user_role(user_id,role_name) VALUES ($1::text::uuid,'purchase-reader')").bind(&principal).execute(&mut admin).await.map(|result| result.rows_affected())?;
    }
    sqlx::query("INSERT INTO platform.user_role(user_id,role_name) VALUES ($1::text::uuid,'purchase-writer')").bind(&first["sub"].as_str().unwrap()).execute(&mut admin).await.map(|result| result.rows_affected())?;

    // Each request reuses the same unexpired signed token. No kept approval
    // survives the drop of a revoke.
    for (revoke, restore) in [
        (
            "UPDATE identity.password_logins SET revoked_at=clock_timestamp()",
            "UPDATE identity.password_logins SET revoked_at=NULL",
        ),
        (
            "UPDATE identity.principals SET status='disabled'",
            "UPDATE identity.principals SET status='active'",
        ),
        (
            "UPDATE identity.project_env_memberships SET env='removed'",
            "UPDATE identity.project_env_memberships SET env='dev'",
        ),
        (
            "UPDATE identity.password_logins SET audience='wrong'",
            "UPDATE identity.password_logins SET audience='urn:wamn:project-env:org-a:project:dev:instance-one'",
        ),
        (
            "UPDATE identity.password_logins SET expires_at=clock_timestamp()-interval '1 second'",
            "UPDATE identity.password_logins SET expires_at=clock_timestamp()+interval '8 hours'",
        ),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe((revoke).to_string()))
            .execute(&mut admin)
            .await
            .map(drop)?;
        assert_eq!(
            route
                .authenticate_authorization_for_test(
                    ATTACHMENT,
                    Some(&format!("Bearer {}", signed(&header(), &first)))
                )
                .await
                .unwrap_err(),
            (401, "unauthorized".into())
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe((restore).to_string()))
            .execute(&mut admin)
            .await
            .map(drop)?;
        accepted(&route, &first).await;
    }
    let mut wrong_login = first.clone();
    wrong_login["authority"] = json!({"login": SECOND_PRINCIPAL});
    assert_eq!(
        route
            .authenticate_authorization_for_test(
                ATTACHMENT,
                Some(&format!("Bearer {}", signed(&header(), &wrong_login)))
            )
            .await
            .unwrap_err(),
        (401, "unauthorized".into())
    );
    sqlx::raw_sql("REVOKE SELECT ON identity.password_logins FROM wamn_identity_reader")
        .execute(&mut admin)
        .await
        .map(drop)?;
    assert_eq!(
        route
            .authenticate_authorization_for_test(
                ATTACHMENT,
                Some(&format!("Bearer {}", signed(&header(), &first)))
            )
            .await
            .unwrap_err(),
        (503, "authentication-unavailable".into())
    );
    sqlx::raw_sql("GRANT SELECT ON identity.password_logins TO wamn_identity_reader")
        .execute(&mut admin)
        .await
        .map(drop)?;

    let before = statements(&mut admin, &generation).await?;
    for (field, value) in [
        ("iss", json!("https://untrusted.invalid")),
        ("org", json!("other-org")),
        ("aud", json!(AUDIENCE.replace(":dev:", ":prod:"))),
        (
            "aud",
            json!(AUDIENCE.replace("instance-one", "instance-two")),
        ),
        ("sub", json!("not-a-principal")),
        ("exp", json!(1901)),
        ("iat", json!(1031)),
    ] {
        let mut bad = first.clone();
        bad[field] = value;
        assert_eq!(
            route
                .authenticate_authorization_for_test(
                    ATTACHMENT,
                    Some(&format!("Bearer {}", signed(&header(), &bad)))
                )
                .await
                .unwrap_err(),
            (401, "unauthorized".into()),
            "{field}"
        );
    }
    for (field, value) in [("alg", "Ed25519"), ("typ", "JWT"), ("kid", "unknown-key")] {
        let mut bad = header();
        bad[field] = json!(value);
        assert_eq!(
            mixed
                .authenticate_authorization_for_test(
                    ATTACHMENT,
                    Some(&format!("Bearer {}", signed(&bad, &first)))
                )
                .await
                .unwrap_err(),
            (401, "unauthorized".into())
        );
    }
    token_clock.set(1930);
    assert_eq!(
        route
            .authenticate_authorization_for_test(
                ATTACHMENT,
                Some(&format!("Bearer {}", signed(&header(), &first)))
            )
            .await
            .unwrap_err(),
        (401, "unauthorized".into())
    );
    assert_permission_reads(&before, &statements(&mut admin, &generation).await?, 0);
    token_clock.set(1000);

    // A real blocked permission SELECT crosses token expiry before final admission.
    let before = statements(&mut admin, &generation).await?;
    sqlx::raw_sql("BEGIN; LOCK TABLE platform.permission IN ACCESS EXCLUSIVE MODE")
        .execute(&mut admin)
        .await
        .map(drop)?;
    let authorization = format!("Bearer {}", signed(&header(), &first));
    let (result, released) = tokio::join!(
        route.authenticate_authorization_for_test(ATTACHMENT, Some(&authorization)),
        async {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    sqlx::raw_sql("SELECT pg_stat_clear_snapshot()")
                        .execute(&mut admin)
                        .await
                        .map(drop)?;
                    let waiting: bool = sqlx::query(
                        "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE usename = $1 \
                         AND wait_event_type = 'Lock' AND query LIKE '%platform.permission%')",
                    )
                    .bind(&generation)
                    .fetch_one(&mut admin)
                    .await?
                    .get(0);
                    if waiting {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await
            .context("permission query reached lock barrier")??;
            token_clock.set(1930);
            sqlx::raw_sql("COMMIT")
                .execute(&mut admin)
                .await
                .map(drop)?;
            Ok::<_, anyhow::Error>(())
        }
    );
    released?;
    assert_eq!(result.unwrap_err(), (401, "unauthorized".into()));
    assert_permission_reads(&before, &statements(&mut admin, &generation).await?, 1);
    token_clock.set(1000);

    // A real blocked permission SELECT crosses the key cache deadline before
    // final admission. The router verifies the same token once more on fresh
    // keys and admits it, because the issuer signed it (wamn-co0p).
    let before = statements(&mut admin, &generation).await?;
    let fetches = server.count();
    sqlx::raw_sql("BEGIN; LOCK TABLE platform.permission IN ACCESS EXCLUSIVE MODE")
        .execute(&mut admin)
        .await
        .map(drop)?;
    let (result, released) = tokio::join!(
        route.authenticate_authorization_for_test(ATTACHMENT, Some(&authorization)),
        async {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    sqlx::raw_sql("SELECT pg_stat_clear_snapshot()")
                        .execute(&mut admin)
                        .await
                        .map(drop)?;
                    let waiting: bool = sqlx::query(
                        "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE usename = $1 \
                         AND wait_event_type = 'Lock' AND query LIKE '%platform.permission%')",
                    )
                    .bind(&generation)
                    .fetch_one(&mut admin)
                    .await?
                    .get(0);
                    if waiting {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await
            .context("permission query reached lock barrier")??;
            key_clock.advance(Duration::from_secs(300));
            sqlx::raw_sql("COMMIT")
                .execute(&mut admin)
                .await
                .map(drop)?;
            Ok::<_, anyhow::Error>(())
        }
    );
    released?;
    result
        .expect("a token the issuer signed is admitted across the key deadline")
        .expect("host-owned caller");
    assert_permission_reads(&before, &statements(&mut admin, &generation).await?, 1);
    assert_eq!(server.count(), fetches + 1, "one fetch of fresh keys");

    let before = statements(&mut admin, &generation).await?;
    key_clock.advance(Duration::from_secs(300));
    server.remove_keys();
    assert_eq!(
        route
            .authenticate_authorization_for_test(ATTACHMENT, Some(&authorization))
            .await
            .unwrap_err(),
        (401, "unauthorized".into())
    );
    assert_permission_reads(&before, &statements(&mut admin, &generation).await?, 0);
    assert_eq!(
        server.count(),
        3,
        "expired known key refreshes the configured endpoint"
    );
    server.stop().await;
    Ok(())
}
