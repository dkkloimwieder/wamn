//! The application host routes through a fixture serving release
//! (docs/plan/platform-ui.md §4.2 and §4.6, wamn-a40n.2 and wamn-a40n.7).
//!
//! The test starts its own PostgreSQL 18 server. A real session token is
//! verified against local HTTPS keys, the scoped session permission reader
//! admits the caller, and the host answers the route under the scoped
//! administration login.

use std::sync::Arc;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{HostRouteSet, SERVING_MANIFEST_FORMAT_VERSION};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_engine::flow_http_routing::{AuthenticatedCaller, FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{
    DeliveryError, DeliveryOutcome, DeliveryRequest, RouteDelivery, Source,
};
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_runtime::plugins::route_authentication::{
    PlatformRouteAuthenticator, SessionRouteAuthentication,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

#[path = "../../../platform/runtime/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also provides verifier-only fetch barriers."
)]
mod session_fixture;
use session_fixture::{AUDIENCE, ORG, Server, claims, header, signed};

const PROJECT: &str = "project";
const TENANT: &str = "tenant-a";
const READ: &str = "session-test:purchase/read@1.0.0";
const WRITE: &str = "session-test:purchase/write@1.0.0";
const MEMBER: &str = "34f2085c-19d8-474f-b87a-2a29a5357b9b";
const PASSWORD: &str = "host-route-test-only";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("fixture database connection") });
    Ok(client)
}

fn login(admin_url: &str, role: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(admin_url)?;
    url.set_username(role)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    Ok(url.into())
}

fn generation(family: WorkloadRoleFamily, database: &str) -> anyhow::Result<String> {
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
async fn install(admin: &Client, database: &str) -> anyhow::Result<(String, String)> {
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
        .batch_execute(include_str!("../../../../deploy/sql/app-schema.sql"))
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

fn credentials(admitter: &str, administration: &str) -> anyhow::Result<Arc<WamnPostgres>> {
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
fn load_release() -> anyhow::Result<Arc<LoadedRelease>> {
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

fn permission_mine() -> String {
    route_of("wamn-control:permission/mine")
}

/// The attachment id of the application route with the stable reference.
fn route_of(reference: &str) -> String {
    HostRouteSet::Application
        .attachments()
        .find(|(_, attachment)| attachment.reference == reference)
        .map(|(id, _)| id.to_owned())
        .expect("the application set serves the route")
}

async fn deliver(
    delivery: &HostRouteDelivery,
    attachment: &str,
    caller: Option<AuthenticatedCaller>,
) -> Result<Value, DeliveryError> {
    match outcome(delivery, attachment, caller, json!({})).await? {
        DeliveryOutcome::Respond(body) => {
            let [item]: [Value; 1] = serde_json::from_str(&body).expect("one outcome");
            Ok(item["value"].clone())
        }
        other => panic!("a host route responds: {other:?}"),
    }
}

/// Deliver one request item, as a generated client sends it: a read carries
/// its input, and a write carries a request id and its input under `value`.
async fn outcome(
    delivery: &HostRouteDelivery,
    attachment: &str,
    caller: Option<AuthenticatedCaller>,
    input: Value,
) -> Result<DeliveryOutcome, DeliveryError> {
    let read = HostRouteSet::Application
        .attachment(attachment)
        .or_else(|| HostRouteSet::Control.attachment(attachment))
        .expect("a host route")
        .route
        .type_
        .is_read();
    let item = if read {
        input
    } else {
        json!({"request_id": "host-route-test", "value": input})
    };
    delivery
        .deliver(
            DeliveryRequest {
                source: Source::Attachment(attachment.to_owned()),
                delivery_id: "host-route-test".to_owned(),
                payload: json!([item]).to_string(),
                caller: None,
                trace: None,
                parent_causation: None,
                if_none_match: None,
            },
            caller,
        )
        .await
        .outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_mine_answers_the_held_grants_of_the_session_caller() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("host_route")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let (admitter, administration) = install(&admin, &database).await?;

    // The administration login writes the authorization relations and
    // appends their history entries, and reads nothing on the run plane.
    let scoped = connect(&login(admin_url, &administration)?).await?;
    let identity = scoped
        .query_one(
            "SELECT rolsuper OR rolbypassrls, \
                    has_schema_privilege(current_user, 'wamn_run', 'USAGE') \
             FROM pg_roles WHERE rolname = current_user",
            &[],
        )
        .await?;
    assert!(!identity.get::<_, bool>(0) && !identity.get::<_, bool>(1));
    scoped
        .batch_execute(&format!(
            "BEGIN; \
             SELECT set_config('app.user_id', '{MEMBER}', true), \
                    set_config('app.operation', 'admin:host-route-fixture', true); \
             INSERT INTO app_system.roles (tenant_id, name) VALUES ('{TENANT}', 'written'); \
             DELETE FROM app_system.roles WHERE tenant_id = '{TENANT}' AND name = 'written'; \
             COMMIT;"
        ))
        .await?;
    let entries: i64 = admin
        .query_one(
            "SELECT count(*) FROM app_system.roles_history WHERE (before ->> 'name') = 'written' \
                OR (after ->> 'name') = 'written'",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(
        entries, 2,
        "each administration write appends its history entry"
    );
    drop(scoped);

    let mut server = Server::start().await;
    let (verifier, _token_clock, _key_clock) = server.verifier();
    session_fixture::install_authority(
        &admin,
        PROJECT,
        "dev",
        AUDIENCE,
        &[claims()["sub"].as_str().unwrap(), MEMBER],
    )
    .await?;
    admin
        .batch_execute(&sql::grant_identity_reader_surface_sql())
        .await?;
    admin
        .batch_execute(
            "CREATE ROLE host_route_identity_reader LOGIN; \
             GRANT wamn_identity_reader TO host_route_identity_reader;",
        )
        .await?;
    let identity_reader = connect(admin_url).await?;
    identity_reader
        .batch_execute("SET ROLE host_route_identity_reader")
        .await?;
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
                    Arc::new(identity_reader),
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
            identity: None,
        },
        None,
    );
    let route = permission_mine();
    let bearer = |body: &Value| format!("Bearer {}", signed(&header(), body));

    // An admin holds every operation the release serves, without version.
    let mut first = claims();
    first["roles"] = json!(["admin"]);
    let caller = routing
        .authenticate_authorization_for_test(&route, Some(&bearer(&first)))
        .await
        .expect("an admin session is admitted")
        .expect("a host-owned caller");
    assert_eq!(
        deliver(&delivery, &route, Some(caller))
            .await
            .expect("the host answers"),
        json!({"admin": true, "permissions": [
            "session-test:purchase/read",
            "session-test:purchase/write",
            "wamn-control:permission/grant",
            "wamn-control:permission/list",
            "wamn-control:permission/mine",
            "wamn-control:permission/revoke",
            "wamn-control:role/create",
            "wamn-control:role/delete",
            "wamn-control:role/list",
            "wamn-control:user-role/grant",
            "wamn-control:user-role/revoke",
            "wamn-control:user/list",
        ]})
    );

    // A member holds the stored permissions of its roles.
    let mut member = claims();
    member["sub"] = json!(MEMBER);
    member["authority"] = json!({"login": MEMBER});
    member["roles"] = json!(["purchase-reader"]);
    let caller = routing
        .authenticate_authorization_for_test(&route, Some(&bearer(&member)))
        .await
        .expect("a member session is admitted")
        .expect("a host-owned caller");
    assert_eq!(
        deliver(&delivery, &route, Some(caller))
            .await
            .expect("the host answers"),
        json!({"admin": false, "permissions": ["session-test:purchase/read"]})
    );

    // A read route by cookie needs the CSRF claim and no header.
    let csrf = "fixture-csrf-token";
    let mut carried = member.clone();
    carried["csrf"] = json!(hex::encode(Sha256::digest(csrf.as_bytes())));
    let cookie = format!("__Host-wamn-session={}", signed(&header(), &carried));
    assert!(
        routing
            .authenticate_headers_for_test(&route, &[("cookie", &cookie)])
            .await
            .expect("a cookie read without the header is admitted")
            .is_some()
    );
    let bare = format!("__Host-wamn-session={}", signed(&header(), &member));
    assert_eq!(
        routing
            .authenticate_headers_for_test(&route, &[("cookie", &bare)])
            .await
            .expect_err("a cookie without the CSRF claim is refused")
            .0,
        401
    );

    // No caller, no answer, and a PAT is unavailable without PAT authentication.
    assert!(matches!(
        deliver(&delivery, &route, None).await,
        Err(DeliveryError::InvalidRequest)
    ));
    assert_eq!(
        routing
            .authenticate_authorization_for_test(&route, Some("Bearer wamn_pat_unknown"))
            .await
            .expect_err("no PAT authentication is configured")
            .0,
        503
    );
    server.stop().await;
    Ok(())
}

/// The admin routes of §4.6 under the administration login: each one
/// refuses a caller without `admin`, a stored row naming a host route
/// grants nothing, and every write is stamped with the caller and the
/// route's sealed operation id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_routes_write_the_rows_of_section_4_6() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("application_route")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let (admitter, administration) = install(&admin, &database).await?;
    let first = claims()["sub"].as_str().unwrap().to_owned();
    let mut server = Server::start().await;
    let (verifier, _token_clock, _key_clock) = server.verifier();
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
    let bearer = |body: &Value| format!("Bearer {}", signed(&header(), body));
    let mut admin_claims = claims();
    admin_claims["roles"] = json!(["admin"]);
    let mut member_claims = claims();
    member_claims["sub"] = json!(MEMBER);
    member_claims["authority"] = json!({"login": MEMBER});
    member_claims["roles"] = json!(["purchase-reader"]);
    let call = async |claims: &Value, reference: &str, payload: Value| {
        let route = route_of(reference);
        let caller = routing
            .authenticate_authorization_for_test(&route, Some(&bearer(claims)))
            .await
            .expect("the session is admitted")
            .expect("a host-owned caller");
        outcome(&delivery, &route, Some(caller), payload).await
    };
    // The one outcome of the one item: its value, or its declared error.
    let item = async |reference: &str, payload: Value| -> Value {
        match call(&admin_claims, reference, payload).await {
            Ok(DeliveryOutcome::Respond(body)) => {
                let [item]: [Value; 1] = serde_json::from_str(&body).unwrap();
                item
            }
            other => panic!("{reference} answers: {other:?}"),
        }
    };
    let answer = async |reference: &str, payload: Value| -> Value {
        let item = item(reference, payload).await;
        assert!(item.get("error").is_none(), "{reference} answers: {item}");
        item["value"].clone()
    };
    let refusal = async |reference: &str, payload: Value| -> Value {
        let item = item(reference, payload).await;
        assert!(item.get("value").is_none(), "{reference} refuses: {item}");
        item["error"].clone()
    };

    // A caller without admin is refused, even with a stored row that names
    // the route.
    admin
        .execute(
            "INSERT INTO app_system.permissions (tenant_id, role_name, permission, required_by) \
             VALUES ($1, 'purchase-reader', 'wamn-control:role/list', 'wamn-control:role/list')",
            &[&TENANT],
        )
        .await?;
    for reference in [
        "wamn-control:user/list",
        "wamn-control:role/list",
        "wamn-control:role/create",
        "wamn-control:role/delete",
        "wamn-control:permission/list",
        "wamn-control:permission/grant",
        "wamn-control:permission/revoke",
        "wamn-control:user-role/grant",
        "wamn-control:user-role/revoke",
    ] {
        assert!(
            matches!(
                call(&member_claims, reference, json!({})).await,
                Err(DeliveryError::PermissionDenied(_))
            ),
            "{reference} refuses a member"
        );
    }

    assert_eq!(
        answer("wamn-control:user/list", json!({})).await,
        json!({"users": [
            {"id": first, "email": "first@example.test", "display_name": null, "roles": ["admin"]},
            {"id": MEMBER, "email": "member@example.test", "display_name": null,
             "roles": ["purchase-reader"]},
        ]})
    );
    assert_eq!(
        answer("wamn-control:role/list", json!({})).await,
        json!({"roles": ["admin", "purchase-reader"]})
    );

    // Roles: admin is fixed.
    assert_eq!(
        answer("wamn-control:role/create", json!({"role": "clerk"})).await,
        json!({"created": true})
    );
    assert_eq!(
        answer("wamn-control:role/create", json!({"role": "clerk"})).await,
        json!({"created": false})
    );
    assert_eq!(
        refusal("wamn-control:role/create", json!({"role": "admin"})).await,
        json!({"code": "admin_fixed", "detail": {"field": "role"}})
    );
    assert_eq!(
        refusal("wamn-control:role/delete", json!({"role": "admin"})).await,
        json!({"code": "admin_fixed", "detail": {"field": "role"}})
    );
    assert_eq!(
        refusal("wamn-control:role/create", json!({"role": "Clerk"})).await,
        json!({"code": "invalid_input", "detail": {"field": "role"}})
    );

    // Permissions: a grant writes the closure of the loaded release.
    let read = "session-test:purchase/read";
    let write = "session-test:purchase/write";
    assert_eq!(
        answer(
            "wamn-control:permission/grant",
            json!({"role": "clerk", "operation": write})
        )
        .await,
        json!({"rows_added": 2, "closure": [read, write]})
    );
    assert_eq!(
        answer(
            "wamn-control:permission/grant",
            json!({"role": "clerk", "operation": read})
        )
        .await,
        json!({"rows_added": 1, "closure": [read]})
    );
    assert_eq!(
        refusal(
            "wamn-control:permission/grant",
            json!({"role": "clerk", "operation": "session-test:purchase/archive"})
        )
        .await,
        json!({"code": "operation_not_served", "detail": {"field": "operation"}})
    );
    assert_eq!(
        refusal(
            "wamn-control:permission/grant",
            json!({"role": "clerk", "operation": "wamn-control:role/list"})
        )
        .await,
        json!({"code": "operation_not_grantable", "detail": {"field": "operation"}})
    );
    let listed = answer("wamn-control:permission/list", json!({"role": "clerk"})).await;
    let entry = |operation: &str| {
        listed["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["operation"] == operation)
            .cloned()
            .unwrap()
    };
    assert_eq!(
        entry(read),
        json!({"operation": read, "served": true, "grantable": true, "admin_only": false,
               "selected": true, "required_by": [write]})
    );
    assert_eq!(entry(write)["required_by"], json!([]));
    assert_eq!(
        entry("wamn-control:role/list"),
        json!({"operation": "wamn-control:role/list", "served": true, "grantable": false,
               "admin_only": true, "selected": false, "required_by": []})
    );
    assert_eq!(entry("wamn-control:permission/mine")["admin_only"], false);

    // A revoke keeps a row another root requires, and refuses a row that is
    // not a root.
    assert_eq!(
        answer(
            "wamn-control:permission/revoke",
            json!({"role": "clerk", "operation": read})
        )
        .await,
        json!({"still_required_by": [write]})
    );
    assert_eq!(
        refusal(
            "wamn-control:permission/revoke",
            json!({"role": "clerk", "operation": read})
        )
        .await,
        json!({"code": "permission_not_selected",
               "detail": {"field": "operation", "required_by": [write]}})
    );

    // User roles: a known user only, and admin stays under project-admin.
    assert_eq!(
        answer(
            "wamn-control:user-role/grant",
            json!({"user_id": MEMBER, "role": "clerk"})
        )
        .await,
        json!({"granted": true})
    );
    let stamp = admin
        .query_one(
            "SELECT operation, changed_by::text FROM app_system.user_roles_history \
             WHERE (after ->> 'role_name') = 'clerk'",
            &[],
        )
        .await?;
    assert_eq!(
        (stamp.get::<_, String>(0), stamp.get::<_, String>(1)),
        (
            "wamn-control:user-role/grant@0.1.0".to_owned(),
            first.clone()
        )
    );
    let stranger = "9b0c3a5e-6f0e-4c1e-8f33-1d5b2b7a6c11";
    assert_eq!(
        refusal(
            "wamn-control:user-role/grant",
            json!({"user_id": stranger, "role": "clerk"})
        )
        .await,
        json!({"code": "user_not_found", "detail": {"field": "user_id"}})
    );
    admin
        .execute(
            "INSERT INTO identity.project_roles VALUES ($1::text::uuid, $2, $3, 'project-admin')",
            &[&first, &ORG, &PROJECT],
        )
        .await?;
    assert_eq!(
        refusal(
            "wamn-control:user-role/revoke",
            json!({"user_id": first, "role": "admin"})
        )
        .await,
        json!({"code": "admin_covered", "detail": {"field": "user_id"}})
    );
    admin
        .execute("DELETE FROM identity.project_roles", &[])
        .await?;
    assert_eq!(
        answer(
            "wamn-control:user-role/revoke",
            json!({"user_id": MEMBER, "role": "clerk"})
        )
        .await,
        json!({"revoked": true})
    );
    assert_eq!(
        answer("wamn-control:role/delete", json!({"role": "clerk"})).await,
        json!({"deleted": true})
    );
    server.stop().await;
    Ok(())
}

/// `wamn-control:control/mine@0.1.0` on a control host: the control
/// serving root, the control route authenticator and the org's real
/// `control` login. Only a browser session of a current `project-admin` or
/// `org-admin` is admitted, and a revoked role refuses the next request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_mine_admits_only_a_current_project_admin_or_org_admin_session()
-> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_route")?;
    let admin_url = test_database.url();
    let admin = connect(admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let audience = wamn_platform_identity::control::control_audience(ORG)?;
    let principal = claims()["sub"]
        .as_str()
        .expect("fixture principal")
        .to_owned();
    session_fixture::install_authority(&admin, PROJECT, "dev", &audience, &[&principal]).await?;
    admin
        .execute(
            "INSERT INTO identity.project_roles VALUES ($1::text::uuid, $2, $3, 'project-admin')",
            &[&principal, &ORG, &PROJECT],
        )
        .await?;
    let role = workload_generation_role(
        WorkloadRoleFamily::Control,
        WorkloadRoleScope::Org {
            org: ORG,
            database: &database,
        },
        CredentialGeneration::A,
    )?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::Control,
            &database,
            &role,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    let control = Arc::new(connect(&login(admin_url, &role)?).await?);
    let writer = Arc::new(tokio::sync::Mutex::new(
        connect(&login(admin_url, &role)?).await?,
    ));

    let mut server = Server::start().await;
    let (keys, _key_clock) = server.cache();
    let (verifier, _token_clock) =
        wamn_session::verifier::SessionVerifier::with_test_clock(keys, ORG, &audience, 1000)?;
    let release = Arc::new(LoadedRelease::control_root());
    let routing = FlowHttpRouting::new(Some(Arc::clone(&release)), RouteInFlightLimit::default())
        .with_authenticator(Arc::new(
            wamn_runtime::plugins::route_authentication::ControlRouteAuthenticator::new(
                verifier,
                Arc::clone(&control),
            ),
        ));
    let delivery = HostRouteDelivery::new(
        release,
        HostRouteHandlers::Control {
            administration: None,
            control,
            writer,
            identity: None,
            org: ORG.to_owned(),
        },
        None,
    );
    let route = HostRouteSet::Control
        .attachments()
        .find(|(_, attachment)| attachment.reference == "wamn-control:control/mine")
        .map(|(id, _)| id.to_owned())
        .expect("the control set serves control.mine");
    let mut session = claims();
    session["aud"] = json!(audience);
    session["roles"] = json!([]);
    let bearer = format!("Bearer {}", signed(&header(), &session));

    let caller = routing
        .authenticate_authorization_for_test(&route, Some(&bearer))
        .await
        .expect("a project-admin session is admitted")
        .expect("a host-owned caller");
    assert_eq!(
        deliver(&delivery, &route, Some(caller))
            .await
            .expect("the host answers"),
        json!({"org_admin": false, "projects": [{"project": PROJECT, "project_admin": true}]})
    );

    // A cookie read needs the CSRF claim. A PAT is never a control credential.
    let mut carried = session.clone();
    carried["csrf"] = json!(hex::encode(Sha256::digest(b"control-csrf")));
    let cookie = format!("__Host-wamn-session={}", signed(&header(), &carried));
    assert!(
        routing
            .authenticate_headers_for_test(&route, &[("cookie", &cookie)])
            .await
            .expect("a cookie read with the claim is admitted")
            .is_some()
    );
    let bare = format!("__Host-wamn-session={}", signed(&header(), &session));
    for headers in [
        vec![("cookie", bare.as_str())],
        vec![("authorization", "Bearer wamn_pat_unknown")],
    ] {
        assert_eq!(
            routing
                .authenticate_headers_for_test(&route, &headers)
                .await
                .expect_err("refused")
                .0,
            401,
            "{headers:?}"
        );
    }

    // An application session is not a control session.
    let mut application = session.clone();
    application["aud"] = json!(AUDIENCE);
    assert_eq!(
        routing
            .authenticate_authorization_for_test(
                &route,
                Some(&format!("Bearer {}", signed(&header(), &application)))
            )
            .await
            .expect_err("another audience is refused")
            .0,
        401
    );

    admin
        .execute(
            "DELETE FROM identity.project_roles WHERE principal_id = $1::text::uuid",
            &[&principal],
        )
        .await?;
    assert_eq!(
        routing
            .authenticate_authorization_for_test(&route, Some(&bearer))
            .await
            .expect_err("a revoked role refuses the next request")
            .0,
        401
    );

    // An org-admin is admitted and holds every project of the org.
    admin
        .execute(
            "INSERT INTO identity.org_roles VALUES ($1::text::uuid, $2, 'org-admin')",
            &[&principal, &ORG],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO registry.projects VALUES ($1, 'shop'), ($1, $2), ('otherorg', 'mail')",
            &[&ORG, &PROJECT],
        )
        .await?;
    let caller = routing
        .authenticate_authorization_for_test(&route, Some(&bearer))
        .await
        .expect("an org-admin session is admitted")
        .expect("a host-owned caller");
    let projects = [PROJECT, "shop"];
    assert_eq!(
        deliver(&delivery, &route, Some(caller))
            .await
            .expect("the host answers"),
        json!({"org_admin": true, "projects": projects
            .iter()
            .map(|project| json!({"project": project, "project_admin": true}))
            .collect::<Vec<_>>()})
    );
    admin
        .execute(
            "DELETE FROM identity.org_roles WHERE principal_id = $1::text::uuid",
            &[&principal],
        )
        .await?;
    assert_eq!(
        routing
            .authenticate_authorization_for_test(&route, Some(&bearer))
            .await
            .expect_err("a revoked org-admin refuses the next request")
            .0,
        401
    );
    server.stop().await;
    Ok(())
}
