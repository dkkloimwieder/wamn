//! Real scoped permission reads and local HTTPS keys cross the route-authentication boundary.
//!
//! This ignored test requires its own fresh PostgreSQL 18 server with
//! `shared_preload_libraries=pg_stat_statements` and `pg_stat_statements.track=all`.
//! It does not start deployed hosts or show nested registered-operation guards.
#![cfg(feature = "test-util")]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_runtime::plugins::flow_http_routing::{
    AuthenticatedCaller, CredentialKind, FlowHttpRouting, RouteInFlightLimit,
    SessionRouteAuthentication,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use wamn_runtime::release_manifest::LoadedRelease;

#[path = "support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also provides verifier-only fetch barriers."
)]
mod session_fixture;
use session_fixture::{AUDIENCE, ORG, Server, claims, header, signed};

const URL_ENV: &str = "WAMN_SESSION_ROUTE_PG18_URL";
const PROJECT: &str = "project";
const TENANT: &str = "tenant-a";
const ATTACHMENT: &str = "purchase-http";
const READ: &str = "session-test:purchase/read@1.0.0";
const WRITE: &str = "session-test:purchase/write@1.0.0";
const OTHER_TENANT: &str = "session-test:secret/read@1.0.0";
const SECOND_PRINCIPAL: &str = "34f2085c-19d8-474f-b87a-2a29a5357b9b";
const PASSWORD: &str = "session-route-test-only";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("fixture database connection") });
    Ok(client)
}

async fn install(admin: &Client, database: &str, generation: &str) -> anyhow::Result<()> {
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await?
        .get::<_, String>(0)
        .parse()?;
    anyhow::ensure!(
        version >= 180_000,
        "the test requires PostgreSQL 18 or newer"
    );
    let occupied: bool = admin.query_one(
        "SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname IN ('app_system', 'catalog', 'identity', 'wamn_run')) \
         OR EXISTS (SELECT FROM pg_roles WHERE rolname IN ('wamn_app', 'wamn_scenario_author', 'wamn_http_admitter'))",
        &[],
    ).await?.get(0);
    anyhow::ensure!(
        !occupied,
        "refuse a populated server: arm only this test's fresh disposable PostgreSQL server"
    );
    admin
        .batch_execute(
            "CREATE EXTENSION pg_stat_statements; \
         CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
         CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
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
            WorkloadRoleFamily::HttpAdmitter,
            database,
            generation,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    Ok(())
}

async fn seed(admin: &Client) -> anyhow::Result<()> {
    let first = claims()["sub"]
        .as_str()
        .expect("fixture principal")
        .to_owned();
    for tenant in [TENANT, "tenant-b"] {
        for role in ["purchase-reader", "purchase-writer"] {
            admin
                .execute(
                    "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, $2)",
                    &[&tenant, &role],
                )
                .await?;
        }
    }
    for (tenant, role, permission) in [
        (TENANT, "purchase-reader", READ),
        (TENANT, "purchase-writer", READ),
        (TENANT, "purchase-writer", WRITE),
        ("tenant-b", "purchase-reader", OTHER_TENANT),
    ] {
        admin.execute("INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ($1, $2, $3)",
            &[&tenant, &role, &permission]).await?;
    }
    for (principal, email) in [
        (first.as_str(), "first@example.test"),
        (SECOND_PRINCIPAL, "second@example.test"),
    ] {
        admin.execute("INSERT INTO app_system.users (tenant_id, id, email) VALUES ($1, $2::text::uuid, $3)",
            &[&TENANT, &principal, &email]).await?;
        admin.execute("INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ($1, $2::text::uuid, 'purchase-reader')",
            &[&TENANT, &principal]).await?;
    }
    admin.execute("INSERT INTO app_system.user_roles (tenant_id, user_id, role_name) VALUES ($1, $2::text::uuid, 'purchase-writer')",
        &[&TENANT, &first]).await?;
    Ok(())
}

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

fn load_release(modes: &[&str]) -> anyhow::Result<Arc<LoadedRelease>> {
    let definition = json!({"id": ATTACHMENT, "kind": "http", "route": {
        "host": "purchase.example.test", "path": "/purchase", "method": "POST"
    }});
    let manifest = json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"tenant-id": TENANT, "effective-release-id": 1, "environment": "dev",
            "packages": [{"package-id": "session_test", "package-version": "1.0.0"}]},
        "components": [{"package-id": "session_test", "component": "purchase", "interface-version": "0.1.0",
            "digest": format!("sha256:{}", "a".repeat(64)), "operations": {
                READ: {"registered-operation": READ}, WRITE: {"registered-operation": WRITE}
            }}],
        "wirings": [{"package-id": "session_test", "wiring-id": "purchase", "wiring-version": 1,
            "graph-hash": format!("sha256:{}", "b".repeat(64))}],
        "attachments": {ATTACHMENT: {"kind": "http", "package-id": "session_test", "wiring-id": "purchase",
            "wiring-version": 1, "definition-hash": wamn_execution_contract::canonical_json_sha256(&definition),
            "definition": definition, "auth-policy": {"modes": modes}, "registered-operation": READ}},
        "registrations": {}
    });
    Ok(Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_execution_contract::canonical_json_bytes(&manifest),
        "session route test",
    )?))
}

fn routing(
    authentication: Arc<SessionRouteAuthentication>,
    modes: &[&str],
) -> anyhow::Result<FlowHttpRouting> {
    Ok(
        FlowHttpRouting::new(Some(load_release(modes)?), RouteInFlightLimit::default())
            .with_session_authentication(authentication),
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

async fn statements(admin: &Client, generation: &str) -> anyhow::Result<BTreeMap<String, i64>> {
    admin.query(
        "SELECT query, calls::bigint FROM pg_stat_statements \
         WHERE userid = (SELECT oid FROM pg_roles WHERE rolname = $1) \
         AND dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY query",
        &[&generation],
    ).await?.into_iter().map(|row| Ok((row.try_get(0)?, row.try_get(1)?))).collect()
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
            "refused credentials must perform no tenant SQL: {changes:?}"
        );
        return;
    }
    assert_eq!(
        changes.len(),
        1,
        "only the single permission statement executes: {changes:?}"
    );
    let (query, calls) = changes[0];
    assert!(
        query.starts_with("SELECT DISTINCT permission FROM app_system.permissions"),
        "{query}"
    );
    assert!(
        query.contains("tenant_id = $1 AND role_name = ANY($2::text[])"),
        "{query}"
    );
    assert_eq!(calls, expected, "one fresh permission read per request");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires fresh PG18 with pg_stat_statements in WAMN_SESSION_ROUTE_PG18_URL"]
async fn sessions_use_one_fresh_scoped_permission_union_and_preserve_the_signed_identity()
-> anyhow::Result<()> {
    let admin_url = std::env::var(URL_ENV)
        .context("set WAMN_SESSION_ROUTE_PG18_URL to this test's fresh disposable PG18 server")?;
    let admin = connect(&admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let generation = workload_generation_role(
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: "dev",
            database: &database,
        },
        CredentialGeneration::A,
    )?;
    install(&admin, &database, &generation).await?;
    seed(&admin).await?;
    let mut url = url::Url::parse(&admin_url)?;
    url.set_username(&generation)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    let scoped = connect(url.as_str()).await?;
    let identity = scoped
        .query_one(
            "SELECT session_user::text, current_user::text, rolsuper, rolbypassrls, \
         pg_has_role(current_user, 'wamn_http_admitter', 'USAGE'), \
         has_table_privilege(current_user, 'app_system.permissions', 'UPDATE') \
         FROM pg_roles WHERE rolname = current_user",
            &[],
        )
        .await?;
    assert_eq!(identity.get::<_, String>(0), generation);
    assert_eq!(identity.get::<_, String>(1), generation);
    assert!(!identity.get::<_, bool>(2) && !identity.get::<_, bool>(3));
    assert!(identity.get::<_, bool>(4));
    assert!(
        !identity.get::<_, bool>(5),
        "runtime receives no permission writes"
    );
    drop(scoped);

    let mut server = Server::start().await;
    let (verifier, token_clock, key_clock) = server.verifier();
    let authentication = Arc::new(SessionRouteAuthentication::new(
        verifier,
        permission_reader(url.as_str())?,
        PROJECT,
    ));
    let route = routing(authentication.clone(), &["session"])?;
    let mixed = routing(authentication, &["pat", "session"])?;
    let mut first = claims();
    first["roles"] = json!(["purchase-reader", "purchase-writer"]);
    let mut second = claims();
    second["sub"] = json!(SECOND_PRINCIPAL);
    accepted(&route, &first).await;
    let before = statements(&admin, &generation).await?;
    let admitted = accepted(&route, &first).await;
    assert_eq!(admitted.principal_id(), first["sub"].as_str().unwrap());
    assert_eq!(admitted.credential_kind(), CredentialKind::Session);
    assert_eq!(admitted.clone().credential_kind(), CredentialKind::Session);
    assert!(admitted.permits(READ) && admitted.permits(WRITE));
    assert!(
        !admitted.permits(OTHER_TENANT),
        "same role name cannot cross the tenant predicate"
    );
    let other = accepted(&route, &second).await;
    assert_eq!(other.principal_id(), SECOND_PRINCIPAL);
    assert!(other.permits(READ) && !other.permits(WRITE));
    assert_eq!(
        accepted(&mixed, &first).await.credential_kind(),
        CredentialKind::Session
    );
    for roles in [json!([]), json!(["unknown-role"])] {
        let mut empty = first.clone();
        empty["roles"] = roles;
        let caller = accepted(&route, &empty).await;
        assert!(!caller.permits(READ) && !caller.permits(WRITE) && !caller.permits(OTHER_TENANT));
    }
    assert_permission_reads(&before, &statements(&admin, &generation).await?, 5);
    assert_eq!(
        server.count(),
        1,
        "warm sessions perform no additional JWKS request"
    );

    let before = statements(&admin, &generation).await?;
    admin
        .execute(
            "DELETE FROM app_system.permissions WHERE tenant_id = $1 AND permission = $2",
            &[&TENANT, &WRITE],
        )
        .await?;
    let changed = accepted(&route, &first).await;
    assert!(
        changed.permits(READ) && !changed.permits(WRITE),
        "permission removal applies on the next request"
    );
    assert!(
        admitted.permits(WRITE),
        "permission change does not cancel already-admitted work"
    );
    admin.execute("INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ($1, 'purchase-writer', $2)", &[&TENANT, &WRITE]).await?;
    admin
        .execute(
            "DELETE FROM app_system.user_roles WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?;
    admin
        .execute(
            "UPDATE app_system.users SET status = 'disabled' WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?;
    let snapshot = accepted(&route, &first).await;
    assert!(
        snapshot.permits(READ) && snapshot.permits(WRITE),
        "signed roles remain the accepted identity snapshot"
    );
    assert_permission_reads(&before, &statements(&admin, &generation).await?, 2);

    let before = statements(&admin, &generation).await?;
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
    for (field, value) in [("alg", "EdDSA"), ("typ", "JWT"), ("kid", "unknown-key")] {
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
    assert_permission_reads(&before, &statements(&admin, &generation).await?, 0);
    token_clock.set(1000);

    // A real blocked permission SELECT crosses token expiry before final admission.
    let before = statements(&admin, &generation).await?;
    admin
        .batch_execute("BEGIN; LOCK TABLE app_system.permissions IN ACCESS EXCLUSIVE MODE")
        .await?;
    let authorization = format!("Bearer {}", signed(&header(), &first));
    let (result, released) = tokio::join!(
        route.authenticate_authorization_for_test(ATTACHMENT, Some(&authorization)),
        async {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    admin
                        .batch_execute("SELECT pg_stat_clear_snapshot()")
                        .await?;
                    let waiting: bool = admin
                        .query_one(
                            "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE usename = $1 \
                         AND wait_event_type = 'Lock' AND query LIKE '%app_system.permissions%')",
                            &[&generation],
                        )
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
            admin.batch_execute("COMMIT").await?;
            Ok::<_, anyhow::Error>(())
        }
    );
    released?;
    assert_eq!(result.unwrap_err(), (401, "unauthorized".into()));
    assert_permission_reads(&before, &statements(&admin, &generation).await?, 1);
    token_clock.set(1000);

    let before = statements(&admin, &generation).await?;
    key_clock.advance(Duration::from_secs(300));
    server.remove_keys();
    assert_eq!(
        route
            .authenticate_authorization_for_test(ATTACHMENT, Some(&authorization))
            .await
            .unwrap_err(),
        (401, "unauthorized".into())
    );
    assert_permission_reads(&before, &statements(&admin, &generation).await?, 0);
    assert_eq!(
        server.count(),
        2,
        "expired known key refreshes the configured endpoint"
    );
    server.stop().await;
    Ok(())
}
