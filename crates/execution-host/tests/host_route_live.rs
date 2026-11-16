//! The application host routes through a fixture serving release
//! (docs/architecture/execution.md#host-run-routes, wamn-a40n.2, wamn-a40n.7).
//!
//! The test starts its own PostgreSQL 18 server. A real session token is
//! verified against local HTTPS keys, the targeted session permission reader
//! admits the caller, and the host answers the route under the targeted
//! administration login.

use sqlx::Row as _;
use std::collections::BTreeSet;
use std::sync::Arc;
use wamn_broker::Publisher;

use serde_json::{Value, json};
use wamn_authentication::route_authentication::PlatformRouteAuthenticator;
use wamn_catalog::HostRouteSet;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, sql, workload_login};
use wamn_engine::flow_http_routing::{AuthenticatedCaller, FlowHttpRouting};
use wamn_engine::native_routes::{DeliveryError, DeliveryOutcome, DeliveryRequest, Source};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_package_artifact::http_client::HttpClient;

#[path = "../../authentication/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also provides verifier-only fetch barriers."
)]
mod session_fixture;
use session_fixture::{AUDIENCE, ORG, Server, claims, header, signed};

#[path = "support/application_fixture.rs"]
mod application_fixture;
#[path = "support/session_authority.rs"]
mod session_authority;
use application_fixture::{
    ApplicationHost, MEMBER, PASSWORD, PROJECT, application_host, connect, credentials, install,
    load_release, login,
};

fn permission_mine() -> wamn_catalog::canonical_name::CanonicalName {
    route_of("wamn-control:permission/mine")
}

/// The attachment id of the application route with the stable reference.
fn route_of(reference: &str) -> wamn_catalog::canonical_name::CanonicalName {
    HostRouteSet::Application
        .attachments()
        .find(|(_, attachment)| attachment.reference == reference)
        .map(|(id, _)| id.to_owned())
        .expect("the application set serves the route")
}

async fn deliver(
    delivery: &HostRouteDelivery,
    attachment: &wamn_catalog::canonical_name::CanonicalName,
    caller: Option<AuthenticatedCaller>,
) -> Result<Value, DeliveryError> {
    let body = outcome(delivery, attachment, caller, json!({})).await?;
    let [request_element]: [Value; 1] = serde_json::from_str(&body).expect("one outcome");
    Ok(request_element["value"].clone())
}

/// Deliver one request element, as a generated client sends it: a read carries
/// its input, and a write carries a request id and its input under `value`.
async fn outcome(
    delivery: &HostRouteDelivery,
    attachment: &wamn_catalog::canonical_name::CanonicalName,
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
    let request_element = if read {
        input
    } else {
        json!({"request_id": "host-route-test", "value": input})
    };
    delivery
        .deliver(
            DeliveryRequest {
                source: Source::Attachment(attachment.clone()),
                delivery_id: "host-route-test".to_owned(),
                payload: json!([request_element]).to_string(),
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
#[ignore = "requires: postgres"]
async fn permission_mine_answers_the_held_grants_of_the_session_caller() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("host_route").await?;
    let admin_url = test_database.url();
    let mut admin = connect(admin_url).await?;
    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    let (admitter, administration) = install(&mut admin, &database, admin_url).await?;

    // The administration login writes the authorization relations and
    // appends their history entries, and reads nothing on the run plane.
    let mut targeted = connect(&login(admin_url, &administration)?).await?;
    let identity = sqlx::query(
        "SELECT rolsuper OR rolbypassrls, \
                    has_schema_privilege(current_user, 'wamn_run', 'USAGE') \
             FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&mut targeted)
    .await?;
    assert!(!identity.get::<bool, _>(0) && !identity.get::<bool, _>(1));
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "BEGIN; \
             INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('{MEMBER}'::text::uuid,'','fixture','admin:host-route-fixture'); \
             INSERT INTO platform.role (name) VALUES ('written'); \
             DELETE FROM platform.role WHERE name = 'written'; \
             COMMIT;"
        ))
            .to_string(),
    ))
    .execute(&mut targeted)
    .await
    .map(drop)?;
    let entries: i64 = sqlx::query(
        "SELECT count(*) FROM platform.role_history WHERE (before ->> 'name') = 'written' \
                OR (after ->> 'name') = 'written'",
    )
    .fetch_one(&mut admin)
    .await?
    .get(0);
    assert_eq!(
        entries, 2,
        "each administration write appends its history entry"
    );
    drop(targeted);

    let mut server = Server::start().await;
    let (verifier, _token_clock, _key_clock) = server.verifier();
    session_authority::install_authority(
        &mut admin,
        &PROJECT.parse().unwrap(),
        &"dev".parse().unwrap(),
        AUDIENCE,
        &[claims()["sub"].as_str().unwrap(), MEMBER],
    )
    .await?;
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
    let identity_reader = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::raw_sql("SET ROLE host_route_identity_reader")
                    .execute(connection)
                    .await
                    .map(drop)
            })
        })
        .connect_lazy_with(wamn_postgres::connect_options(admin_url).await?);
    let release = load_release()?;
    let postgres_credentials = credentials(
        &login(admin_url, &admitter)?,
        &login(admin_url, &administration)?,
        None,
    )?;
    let routing = FlowHttpRouting::for_test(Some(Arc::clone(&release))).with_authenticator(
        Arc::new(PlatformRouteAuthenticator::for_test(
            Arc::clone(&postgres_credentials),
            &application_fixture::PROJECT_ENV_KEY.parse().unwrap(),
            Some(identity_reader),
            Some(verifier),
        )),
    );
    let delivery = HostRouteDelivery::new(
        Arc::clone(&release),
        HostRouteHandlers::Application {
            postgres: postgres_credentials,
            project: PROJECT.parse().unwrap(),
            identity: None,
            changes: Publisher::default(),
        },
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
    let response = deliver(&delivery, &route, Some(caller))
        .await
        .expect("the host answers");
    assert_eq!(response["admin"].as_bool(), Some(true));
    let manifest = release.manifest();
    let expected: BTreeSet<String> = manifest
        .components
        .iter()
        .flat_map(|component| component.descriptor.operations.values())
        .filter_map(|operation| operation.registered_operation.as_ref())
        .map(|operation| wamn_catalog::sealed_operation_reference(operation.as_str()).to_owned())
        .chain(
            manifest
                .host_routes
                .iter()
                .flat_map(|set| set.attachments())
                .map(|(_, attachment)| attachment.reference.clone()),
        )
        .collect();
    let permissions = response["permissions"].as_array().expect("permissions");
    assert_eq!(permissions.len(), expected.len());
    assert_eq!(
        permissions
            .iter()
            .map(|permission| permission.as_str().expect("permission").to_owned())
            .collect::<BTreeSet<_>>(),
        expected
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
    let response = deliver(&delivery, &route, Some(caller))
        .await
        .expect("the host answers");
    assert_eq!(response["admin"].as_bool(), Some(false));
    let expected: BTreeSet<String> = sqlx::query_scalar(
        "SELECT DISTINCT p.permission FROM platform.permission p
         JOIN platform.user_role r ON r.role_name=p.role_name WHERE r.user_id=$1::text::uuid",
    )
    .bind(MEMBER)
    .fetch_all(&mut admin)
    .await?
    .into_iter()
    .collect();
    let permissions = response["permissions"].as_array().expect("permissions");
    assert_eq!(permissions.len(), expected.len());
    assert_eq!(
        permissions
            .iter()
            .map(|permission| permission.as_str().expect("permission").to_owned())
            .collect::<BTreeSet<_>>(),
        expected
    );

    // A read route by cookie needs the CSRF JWT claim and no header.
    let csrf = "fixture-csrf-token";
    let mut carried = member.clone();
    carried["csrf"] = json!(wamn_digest::bytes_sha256(csrf.as_bytes()));
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
            .expect_err("a cookie without the CSRF JWT claim is refused")
            .0,
        401
    );

    // No caller receives no answer.
    assert!(matches!(
        deliver(&delivery, &route, None).await,
        Err(DeliveryError::InvalidRequest)
    ));
    server.stop().await;
    Ok(())
}

/// The admin routes of §4.6 under the administration login: each one
/// refuses a caller without `admin`, a stored row naming a host route
/// grants nothing, and every write is stamped with the caller and the
/// route's sealed operation id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres"]
async fn application_routes_write_the_rows_of_section_4_6() -> anyhow::Result<()> {
    let mut host = application_host("application_route").await?;
    let ApplicationHost {
        admin,
        routing,
        delivery,
        first,
        ..
    } = &mut host;
    let first = first.clone();
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
        outcome(delivery, &route, Some(caller), payload).await
    };
    // The one outcome of the one request element: its value, or its declared error.
    let request_element = async |reference: &str, payload: Value| -> Value {
        match call(&admin_claims, reference, payload).await {
            Ok(body) => {
                let [request_element]: [Value; 1] = serde_json::from_str(&body).unwrap();
                request_element
            }
            other => panic!("{reference} answers: {other:?}"),
        }
    };
    let answer = async |reference: &str, payload: Value| -> Value {
        let request_element = request_element(reference, payload).await;
        assert!(
            request_element.get("error").is_none(),
            "{reference} answers: {request_element}"
        );
        request_element["value"].clone()
    };
    let refusal = async |reference: &str, payload: Value| -> Value {
        let request_element = request_element(reference, payload).await;
        assert!(
            request_element.get("value").is_none(),
            "{reference} refuses: {request_element}"
        );
        request_element["error"].clone()
    };

    // A caller without admin is refused, even with a stored row that names
    // the route.
    sqlx::query(
        "INSERT INTO platform.permission (role_name, permission) \
             VALUES ('purchase-reader', 'wamn-control:role/list')",
    )
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())?;
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
        "wamn-control:workflow/start",
    ] {
        assert!(
            matches!(
                call(&member_claims, reference, json!({})).await,
                Err(DeliveryError::PermissionDenied(_))
            ),
            "{reference} refuses a member"
        );
    }

    {
        let observed = &(answer("wamn-control:user/list", json!({})).await);
        assert_eq!(observed["users"].as_array().unwrap().len(), 2);
        assert_eq!(observed["users"][0]["id"], serde_json::json!(first));
        assert_eq!(
            observed["users"][0]["email"],
            serde_json::json!("first@example.test")
        );
        assert_eq!(
            observed["users"][0]["display_name"],
            serde_json::json!(null)
        );
        assert_eq!(observed["users"][0]["roles"].as_array().unwrap().len(), 1);
        assert_eq!(observed["users"][0]["roles"][0], serde_json::json!("admin"));
        assert_eq!(
            observed["users"][0]["admin_covered"],
            serde_json::json!(false)
        );
        assert_eq!(observed["users"][1]["id"], serde_json::json!(MEMBER));
        assert_eq!(
            observed["users"][1]["email"],
            serde_json::json!("member@example.test")
        );
        assert_eq!(
            observed["users"][1]["display_name"],
            serde_json::json!(null)
        );
        assert_eq!(observed["users"][1]["roles"].as_array().unwrap().len(), 1);
        assert_eq!(
            observed["users"][1]["roles"][0],
            serde_json::json!("purchase-reader")
        );
        assert_eq!(
            observed["users"][1]["admin_covered"],
            serde_json::json!(false)
        );
    };
    {
        let observed = &(answer("wamn-control:role/list", json!({})).await);
        assert_eq!(observed["roles"].as_array().unwrap().len(), 2);
        assert_eq!(observed["roles"][0], serde_json::json!("admin"));
        assert_eq!(observed["roles"][1], serde_json::json!("purchase-reader"));
    };

    // Roles: admin is fixed.
    {
        let observed = &(answer("wamn-control:role/create", json!({"role": "clerk"})).await);
        assert_eq!(observed["created"], serde_json::json!(true));
    };
    {
        let observed = &(answer("wamn-control:role/create", json!({"role": "clerk"})).await);
        assert_eq!(observed["created"], serde_json::json!(false));
    };
    {
        let observed = &(refusal("wamn-control:role/create", json!({"role": "admin"})).await);
        assert_eq!(observed["code"], serde_json::json!("admin_fixed"));
        assert_eq!(observed["detail"]["field"], serde_json::json!("role"));
    };
    {
        let observed = &(refusal("wamn-control:role/delete", json!({"role": "admin"})).await);
        assert_eq!(observed["code"], serde_json::json!("admin_fixed"));
        assert_eq!(observed["detail"]["field"], serde_json::json!("role"));
    };
    {
        let observed = &(refusal("wamn-control:role/create", json!({"role": "Clerk"})).await);
        assert_eq!(observed["code"], serde_json::json!("invalid_input"));
        assert_eq!(observed["detail"]["field"], serde_json::json!("role"));
    };

    // Permissions: a grant stores one row.
    let read = "session-test:purchase/read";
    let write = "session-test:purchase/write";
    {
        let observed = &(answer(
            "wamn-control:permission/grant",
            json!({"role": "clerk", "operation": write}),
        )
        .await);
        assert_eq!(observed["rows_added"], serde_json::json!(1));
    };
    {
        let observed = &(answer(
            "wamn-control:permission/grant",
            json!({"role": "clerk", "operation": read}),
        )
        .await);
        assert_eq!(observed["rows_added"], serde_json::json!(1));
    };
    {
        let observed = &(answer(
            "wamn-control:permission/grant",
            json!({"role": "clerk", "operation": "session-test:purchase/archive"}),
        )
        .await);
        assert_eq!(observed["rows_added"], serde_json::json!(1));
    };
    {
        let observed = &(refusal(
            "wamn-control:permission/grant",
            json!({"role": "clerk", "operation": "wamn-control:role/list"}),
        )
        .await);
        assert_eq!(
            observed["code"],
            serde_json::json!("operation_not_grantable")
        );
        assert_eq!(observed["detail"]["field"], serde_json::json!("operation"));
    };
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
    {
        let observed = &(entry(read));
        assert_eq!(observed["operation"], serde_json::json!(read));
        assert_eq!(observed["served"], serde_json::json!(true));
        assert_eq!(observed["grantable"], serde_json::json!(true));
        assert_eq!(observed["admin_only"], serde_json::json!(false));
        assert_eq!(observed["selected"], serde_json::json!(true));
    };
    {
        let observed = &(entry("wamn-control:role/list"));
        assert_eq!(
            observed["operation"],
            serde_json::json!("wamn-control:role/list")
        );
        assert_eq!(observed["served"], serde_json::json!(true));
        assert_eq!(observed["grantable"], serde_json::json!(false));
        assert_eq!(observed["admin_only"], serde_json::json!(true));
        assert_eq!(observed["selected"], serde_json::json!(false));
    };
    assert_eq!(entry("wamn-control:permission/mine")["admin_only"], false);

    // A revoke removes the row, and refuses a row the role does not hold.
    {
        let observed = &(answer(
            "wamn-control:permission/revoke",
            json!({"role": "clerk", "operation": read}),
        )
        .await);
        assert!(observed.as_object().unwrap().is_empty());
    };
    {
        let observed = &(refusal(
            "wamn-control:permission/revoke",
            json!({"role": "clerk", "operation": read}),
        )
        .await);
        assert_eq!(observed["code"], serde_json::json!("permission_not_held"));
        assert_eq!(observed["detail"]["field"], serde_json::json!("operation"));
    };

    // User roles: a known user only, and admin stays under project-admin.
    for reference in [
        "wamn-control:user-role/grant",
        "wamn-control:user-role/revoke",
    ] {
        {
            let observed = &(refusal(reference, json!({"user_id": MEMBER, "role": "admin"})).await);
            assert_eq!(observed["code"], serde_json::json!("admin_covered"));
            assert_eq!(observed["detail"]["field"], serde_json::json!("user_id"));
        };
    }
    {
        let observed = &(answer(
            "wamn-control:user-role/grant",
            json!({"user_id": MEMBER, "role": "clerk"}),
        )
        .await);
        assert_eq!(observed["granted"], serde_json::json!(true));
    };
    let stamp = sqlx::query(
        "SELECT operation, changed_by FROM platform.user_role_history \
             WHERE (after ->> 'role_name') = 'clerk'",
    )
    .fetch_one(&mut *admin)
    .await?;
    assert_eq!(
        (
            stamp.get::<String, _>(0),
            stamp.get::<wamn_identity_protocol::PrincipalId, _>(1)
        ),
        ("wamn-control:user-role/grant@0.9.0".to_owned(), first)
    );
    let stranger = "9b0c3a5e-6f0e-4c1e-8f33-1d5b2b7a6c11";
    {
        let observed = &(refusal(
            "wamn-control:user-role/grant",
            json!({"user_id": stranger, "role": "clerk"}),
        )
        .await);
        assert_eq!(observed["code"], serde_json::json!("user_not_found"));
        assert_eq!(observed["detail"]["field"], serde_json::json!("user_id"));
    };
    sqlx::query(
        "INSERT INTO identity.project_roles (principal_id,org,project,role) VALUES ($1::text::uuid, $2, $3, 'project-admin')",
    )
    .bind(&first)
    .bind(&ORG)
    .bind(&PROJECT)
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())?;
    // user.list answers the covering grant before any revoke.
    assert_eq!(
        answer("wamn-control:user/list", json!({})).await["users"]
            .as_array()
            .expect("users")
            .iter()
            .map(|user| (user["id"].clone(), user["admin_covered"].clone()))
            .collect::<Vec<_>>(),
        [(json!(first), json!(true)), (json!(MEMBER), json!(false))]
    );
    {
        let observed = &(refusal(
            "wamn-control:user-role/revoke",
            json!({"user_id": first, "role": "admin"}),
        )
        .await);
        assert_eq!(observed["code"], serde_json::json!("admin_covered"));
        assert_eq!(observed["detail"]["field"], serde_json::json!("user_id"));
    };
    sqlx::query("DELETE FROM identity.project_roles")
        .execute(admin)
        .await
        .map(|result| result.rows_affected())?;
    {
        let observed = &(answer(
            "wamn-control:user-role/revoke",
            json!({"user_id": MEMBER, "role": "clerk"}),
        )
        .await);
        assert_eq!(observed["revoked"], serde_json::json!(true));
    };
    {
        let observed = &(answer("wamn-control:role/delete", json!({"role": "clerk"})).await);
        assert_eq!(observed["deleted"], serde_json::json!(true));
    };
    host.server.stop().await;
    Ok(())
}

/// `number_counter.set` is granted per role
/// (docs/architecture/data-access.md#setting-a-counter): an authored role
/// without the permission is refused, `permission.grant` gives it to the role,
/// and the set then runs in an operation transaction under the caller, logged
/// in the write log by its request id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres"]
async fn number_counter_set_is_granted_per_role() -> anyhow::Result<()> {
    let mut host = application_host("number_counter_set").await?;
    let ApplicationHost {
        admin,
        routing,
        delivery,
        ..
    } = &mut host;
    sqlx::query(
        "INSERT INTO platform.number_sequence (application, name, pattern) \
             VALUES ('receiving', 'purchase_order', 'P-{seq:po}')",
    )
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())?;
    let bearer = |body: &Value| format!("Bearer {}", signed(&header(), body));
    let mut admin_claims = claims();
    admin_claims["roles"] = json!(["admin"]);
    let mut member_claims = claims();
    member_claims["sub"] = json!(MEMBER);
    member_claims["authority"] = json!({"login": MEMBER});
    member_claims["roles"] = json!(["purchase-reader"]);
    let set = "wamn-control:number-counter/set";
    // One write request element with its own request id, answered as its one request
    // element.
    let call = async |claims: &Value, reference: &str, request_id: &str, value: Value| {
        let route = route_of(reference);
        let caller = routing
            .authenticate_authorization_for_test(&route, Some(&bearer(claims)))
            .await
            .expect("the session is admitted")
            .expect("a host-owned caller");
        let outcome = delivery
            .deliver(
                DeliveryRequest {
                    source: Source::Attachment(route.clone()),
                    delivery_id: request_id.to_owned(),
                    payload: json!([{"request_id": request_id, "value": value}]).to_string(),
                    caller: None,
                    trace: None,
                    parent_causation: None,
                    if_none_match: None,
                },
                Some(caller),
            )
            .await
            .outcome?;
        let body = outcome;
        let [request_element]: [Value; 1] = serde_json::from_str(&body).expect("one outcome");
        Ok::<_, DeliveryError>(request_element)
    };
    let request = |counter: &str, value: i64| {
        json!({"application": "receiving", "sequence": "purchase_order",
               "counter": counter, "key": "", "value": value})
    };

    assert!(
        matches!(
            call(&member_claims, set, "denied", request("po", 7)).await,
            Err(DeliveryError::PermissionDenied(_))
        ),
        "an authored role without the permission is refused"
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM platform.number_counter")
            .fetch_one(&mut *admin)
            .await?
            .get::<i64, _>(0),
        0
    );

    let granted = call(
        &admin_claims,
        "wamn-control:permission/grant",
        "grant",
        json!({"role": "purchase-reader", "operation": set}),
    )
    .await
    .expect("the grant answers");
    {
        let observed = &(granted["value"]);
        assert_eq!(observed["rows_added"], serde_json::json!(1));
    };

    for application in ["session_test", "unknown"] {
        let mut invalid = request("po", 7);
        invalid["application"] = json!(application);
        let refused = call(&member_claims, set, application, invalid)
            .await
            .expect("the refusal answers");
        {
            let observed = &(refused["error"]);
            assert_eq!(observed["code"], serde_json::json!("invalid_input"));
            assert_eq!(
                observed["detail"]["field"],
                serde_json::json!("application")
            );
        };
    }

    {
        let observed = &(call(&member_claims, set, "set-7", request("po", 7))
            .await
            .expect("the set answers"));
        assert_eq!(observed["request_id"], serde_json::json!("set-7"));
        assert_eq!(observed["value"]["value"], serde_json::json!(7));
    };
    let row = sqlx::query(
        "SELECT value, updated_by::text FROM platform.number_counter \
             WHERE counter = 'po' AND key_value = ''",
    )
    .fetch_one(&mut *admin)
    .await?;
    assert_eq!(
        (row.get::<i64, _>(0), row.get::<String, _>(1)),
        (7, MEMBER.to_owned())
    );
    let logged = sqlx::query(
        "SELECT result FROM platform.write_log \
             WHERE operation = $1 AND idempotency_key = 'set-7'",
    )
    .bind(&set)
    .fetch_one(admin)
    .await?;
    assert_eq!(
        logged.get::<Option<String>, _>(0).as_deref(),
        Some(r#"{"value":7}"#)
    );
    {
        let observed = &(call(&member_claims, set, "nope", request("nope", 7))
            .await
            .expect("the set answers"));
        assert_eq!(observed["request_id"], serde_json::json!("nope"));
        assert_eq!(
            observed["error"]["code"],
            serde_json::json!("counter_not_found")
        );
        assert_eq!(
            observed["error"]["detail"]["field"],
            serde_json::json!("counter")
        );
    };
    host.server.stop().await;
    Ok(())
}

/// `wamn-control:control/mine@0.9.0` on a control host: the control
/// serving root, the control route authenticator and the org's real
/// `control` login. Only a browser session of a current `project-admin` or
/// `org-admin` is admitted, and a revoked role refuses the next request.
#[sqlx::test(migrations = false)]
async fn control_mine_admits_only_a_current_project_admin_or_org_admin_session(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(test_database.as_str()).await;
    let admin_url = test_database.as_str();
    let mut admin = connect(admin_url).await?;
    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES(current_user,$1) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind(wamn_control_provision::PlatformComponent::Provisioning.principal_id())
        .execute(&mut admin).await.unwrap();
    let audience = wamn_platform_identity::control::control_audience(&ORG.parse().unwrap())?;
    let principal = claims()["sub"]
        .as_str()
        .expect("fixture principal")
        .to_owned();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::seed_org_project_env_sql(
            &ORG.parse().unwrap(),
            &PROJECT.parse().unwrap(),
            &"dev".parse().unwrap(),
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::query("INSERT INTO identity.principals(id,type,subject,email,display_name) \
             VALUES($1::text::uuid,'user','control-route@example.test','control-route@example.test','Control route fixture')").bind(&principal).execute(&mut admin).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO identity.org_memberships(principal_id,org,status) VALUES($1::text::uuid,$2,'active')").bind(&principal).bind(&ORG).execute(&mut admin).await.map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO identity.project_env_memberships(principal_id,org,project,env) \
             VALUES($1::text::uuid,$2,$3,'dev')",
    )
    .bind(&principal)
    .bind(&ORG)
    .bind(&PROJECT)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO identity.password_logins(id,principal_id,issuer,audience,authenticated_at,expires_at,renewal_expires_at) \
             VALUES($1::text::uuid,$1::text::uuid,$2,$3,now(),now()+interval '8 hours',now()+interval '30 minutes')").bind(&principal).bind(&session_fixture::ISSUER).bind(&audience).execute(&mut admin).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO identity.project_roles(principal_id,org,project,role) VALUES ($1::text::uuid, $2, $3, 'project-admin')").bind(&principal).bind(&ORG).bind(&PROJECT).execute(&mut admin).await.map(|result| result.rows_affected())?;
    let role = workload_login(
        WorkloadRoleFamily::Control,
        WorkloadRoleIdentity::Org {
            org: &ORG.parse().unwrap(),
            database: &database,
        },
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::Control,
            &database,
            &role,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    let control = wamn_postgres::pool(&login(admin_url, &role)?, 4).await?;
    let writer = Arc::new(tokio::sync::Mutex::new(
        connect(&login(admin_url, &role)?).await?,
    ));

    let mut server = Server::start().await;
    let (keys, _key_clock) = server.cache();
    let (verifier, _token_clock) =
        wamn_identity_protocol::verifier::SessionVerifier::with_test_clock(
            keys, ORG, &audience, 1000,
        )?;
    let release = Arc::new(LoadedRelease::control_root());
    let routing =
        FlowHttpRouting::for_test(Some(Arc::clone(&release))).with_authenticator(Arc::new(
            wamn_authentication::route_authentication::ControlRouteAuthenticator::new(
                verifier,
                control.clone(),
            ),
        ));
    let delivery = HostRouteDelivery::new(
        release,
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: None,
            control,
            writer,
            identity: None,
            org: ORG.parse().unwrap(),
            changes: Publisher::default(),
        },
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
    {
        let observed = &(deliver(&delivery, &route, Some(caller))
            .await
            .expect("the host answers"));
        assert_eq!(observed["next_cursor"], serde_json::json!(null));
        assert_eq!(observed["total"], serde_json::json!(1));
        assert_eq!(observed["org_admin"], serde_json::json!(false));
        assert_eq!(observed["projects"].as_array().expect("array").len(), 1);
        assert_eq!(
            observed["projects"][0]["project"],
            serde_json::json!(PROJECT)
        );
        assert_eq!(
            observed["projects"][0]["project_admin"],
            serde_json::json!(true)
        );
    };

    // A cookie read needs the CSRF JWT claim. A PAT is never a control credential.
    let mut carried = session.clone();
    carried["csrf"] = json!(wamn_digest::bytes_sha256(b"control-csrf"));
    let cookie = format!("__Host-wamn-session={}", signed(&header(), &carried));
    assert!(
        routing
            .authenticate_headers_for_test(&route, &[("cookie", &cookie)])
            .await
            .expect("a cookie read with the JWT claim is admitted")
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

    sqlx::query("DELETE FROM identity.project_roles WHERE principal_id = $1::text::uuid")
        .bind(&principal)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    assert_eq!(
        routing
            .authenticate_authorization_for_test(&route, Some(&bearer))
            .await
            .expect_err("a revoked role refuses the next request")
            .0,
        401
    );

    // An org-admin is admitted and holds every project of the org.
    sqlx::query("INSERT INTO identity.org_roles(principal_id,org,role) VALUES ($1::text::uuid, $2, 'org-admin')").bind(&principal).bind(&ORG).execute(&mut admin).await.map(|result| result.rows_affected())?;
    sqlx::raw_sql("INSERT INTO registry.orgs(id,placement_type) VALUES('otherorg','dedicated')")
        .execute(&mut admin)
        .await
        .map(drop)?;
    sqlx::query("INSERT INTO registry.projects(org,id) VALUES ($1, 'shop'), ('otherorg', 'mail')")
        .bind(&ORG)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    let caller = routing
        .authenticate_authorization_for_test(&route, Some(&bearer))
        .await
        .expect("an org-admin session is admitted")
        .expect("a host-owned caller");
    let projects = [PROJECT, "shop"];
    {
        let observed = &(deliver(&delivery, &route, Some(caller.clone()))
            .await
            .expect("the host answers"));
        assert_eq!(observed["next_cursor"], serde_json::json!(null));
        assert_eq!(observed["total"], serde_json::json!(2));
        assert_eq!(observed["org_admin"], serde_json::json!(true));
        let returned = observed["projects"].as_array().expect("projects");
        assert_eq!(returned.len(), projects.len());
        for (returned, expected) in returned.iter().zip(projects) {
            assert_eq!(returned["project"].as_str(), Some(expected));
            assert_eq!(returned["project_admin"].as_bool(), Some(true));
        }
    };
    // Every page, not only the first, carries org_admin.
    let mut input = json!({"limit": 1});
    for (index, project) in projects.iter().enumerate() {
        let body = outcome(&delivery, &route, Some(caller.clone()), input)
            .await
            .expect("the host answers");
        let [request_element]: [Value; 1] = serde_json::from_str(&body).expect("one outcome");
        let page = &request_element["value"];
        assert_eq!(page["org_admin"], true);
        {
            let observed = &(page["projects"]);
            assert_eq!(observed.as_array().expect("array").len(), 1);
            assert_eq!(observed[0]["project"], serde_json::json!(project));
            assert_eq!(observed[0]["project_admin"], serde_json::json!(true));
        };
        assert!(page["total"].is_null());
        assert_eq!(page["next_cursor"].is_null(), index + 1 == projects.len());
        input = json!({"limit": 1, "cursor": page["next_cursor"]});
    }
    sqlx::query("DELETE FROM identity.org_roles WHERE principal_id = $1::text::uuid")
        .bind(&principal)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: postgres, env:WAMN_SESSION_EXCHANGE_CTL_BIN"]
async fn cli_admin_revoke_reaches_the_same_http_refusal() -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut host = application_host("cli_admin_revoke").await?;
    sqlx::query("INSERT INTO identity.project_roles (principal_id,org,project,role) VALUES($1::text::uuid,$2,$3,'project-admin')")
        .bind(&host.first)
        .bind(&ORG)
        .bind(&PROJECT)
        .execute(&mut host.admin)
        .await
        .map(|result| result.rows_affected())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let host_url = format!("http://{}", listener.local_addr()?);
    let delivery = Arc::clone(&host.delivery);
    let routing = Arc::clone(&host.routing);
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await?;
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let header_end = loop {
                let count = stream.read(&mut buffer).await?;
                anyhow::ensure!(count != 0, "HTTP request ended before headers");
                request.extend_from_slice(&buffer[..count]);
                if let Some(offset) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                    break offset + 4;
                }
            };
            let headers = String::from_utf8(request[..header_end].to_vec())?;
            let mut target = headers.lines().next().unwrap().split_ascii_whitespace();
            assert_eq!(target.next(), Some("POST"));
            assert_eq!(target.next(), Some("/wamn_control/user_role/revoke"));
            let header = |name: &str| {
                headers.lines().find_map(|line| {
                    line.split_once(':')
                        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, value)| value.trim().to_owned())
                })
            };
            let length: usize = header("content-length").expect("body length").parse()?;
            while request.len() < header_end + length {
                let count = stream.read(&mut buffer).await?;
                anyhow::ensure!(count != 0, "HTTP request ended before its body");
                request.extend_from_slice(&buffer[..count]);
            }
            let route = route_of("wamn-control:user-role/revoke");
            let authorization = header("authorization").expect("session authorization");
            let caller = routing
                .authenticate_authorization_for_test(&route, Some(&authorization))
                .await
                .expect("real session admission");
            let report = delivery
                .deliver(
                    DeliveryRequest {
                        source: Source::Attachment(route),
                        delivery_id: "cli-parity".to_owned(),
                        payload: String::from_utf8(
                            request[header_end..header_end + length].to_vec(),
                        )?,
                        caller: None,
                        trace: None,
                        parent_causation: None,
                        if_none_match: None,
                    },
                    caller,
                )
                .await;
            let Ok(body) = report.outcome else {
                panic!("the route must answer");
            };
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
        }
        anyhow::Ok(())
    });
    let mut session = claims();
    session["roles"] = json!(["admin"]);
    let token = signed(&header(), &session);
    let direct: Value = HttpClient::builder()
        .build()
        .expect("HTTP client")
        .post(format!("{host_url}/wamn_control/user_role/revoke"))
        .bearer_auth(&token)
        .json(
            &json!([{"request_id":"direct-parity","value":{"user_id":host.first,"role":"admin"}}]),
        )
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(direct[0]["error"]["code"], "admin_covered");
    let output = tokio::process::Command::new(
        std::env::var_os("WAMN_SESSION_EXCHANGE_CTL_BIN").expect("built CLI"),
    )
    .args([
        "revoke-role",
        "--host-url",
        &host_url,
        "--token",
        &token,
        "--user-id",
        &host.first.to_string(),
        "--role",
        "admin",
    ])
    .output()
    .await?;
    assert!(!output.status.success());
    server.await??;
    let still_admin: bool = sqlx::query("SELECT EXISTS (SELECT 1 FROM platform.user_role WHERE user_id=$1::text::uuid AND role_name='admin')").bind(&host.first).fetch_one(&mut host.admin).await?.get(0);
    assert!(still_admin);
    host.server.stop().await;
    Ok(())
}
