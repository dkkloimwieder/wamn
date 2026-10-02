//! The host routes through their generated TypeScript clients
//! (docs/plan/platform-ui.md §4.4 to §4.6, wamn-a40n.7 and wamn-a40n.8).
//!
//! Each test serves one set through the shipped flow-http component, as a
//! host does, and runs its test file in `web/components` against it with
//! session tokens. The application set answers an `admin` and a member, and
//! the control set answers an `org-admin` and a `project-admin`.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_control_provision::PlatformComponent;
use wamn_engine::engine::build_engine;
use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::RouteDelivery;
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_platform_identity::org::{MemberGrants, invite_member};
use wamn_platform_identity::{PrincipalId, create_or_reuse_user};
use wamn_runtime::plugins::route_authentication::ControlRouteAuthenticator;
use wash_runtime::wasmtime::component::Component;

use crate::local_application::serve;

#[path = "../../../crates/platform/runtime/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also provides verifier-only fetch barriers."
)]
mod session_fixture;
use session_fixture::{ISSUER, ORG, Server, claims, header, signed};

#[path = "../../../crates/execution/host/tests/support/application_fixture.rs"]
mod application_fixture;
use application_fixture::{ApplicationHost, MEMBER, PROJECT, application_host};

#[path = "../../../crates/execution/host/tests/support/control_fixture.rs"]
mod control_fixture;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: WAMN_FLOW_HTTP_COMPONENT"]
async fn application_routes_answer_through_the_generated_client() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["WAMN_FLOW_HTTP_COMPONENT"]);
    let flow_http_wasm = std::env::var("WAMN_FLOW_HTTP_COMPONENT")?;
    let mut host = application_host("control_client").await?;
    let ApplicationHost {
        admin,
        routing,
        delivery,
        first,
        ..
    } = &host;
    // The admin also holds project-admin, so its own admin row stays.
    admin
        .execute(
            "INSERT INTO identity.project_roles VALUES ($1::text::uuid, $2, $3, 'project-admin')",
            &[&first.as_str(), &ORG, &PROJECT],
        )
        .await?;

    let engine = Arc::new(build_engine(&[])?);
    let flow_http = Component::new(engine.inner(), std::fs::read(&flow_http_wasm)?)?;
    let (endpoint, task) = serve(
        engine,
        flow_http,
        Arc::clone(routing),
        Arc::clone(delivery) as Arc<dyn RouteDelivery>,
    )
    .await?;

    let token = |claims: &Value| signed(&header(), claims);
    let mut admin_claims = claims();
    admin_claims["roles"] = json!(["admin"]);
    let mut member_claims = claims();
    member_claims["sub"] = json!(MEMBER);
    member_claims["authority"] = json!({"login": MEMBER});
    member_claims["roles"] = json!(["purchase-reader"]);
    vitest(
        "test/control-client.live.test.ts",
        &[
            ("WAMN_CONTROL_CLIENT_BASE_URL", endpoint),
            ("WAMN_CONTROL_CLIENT_ADMIN_TOKEN", token(&admin_claims)),
            ("WAMN_CONTROL_CLIENT_ADMIN_ID", first.clone()),
            ("WAMN_CONTROL_CLIENT_MEMBER_TOKEN", token(&member_claims)),
            ("WAMN_CONTROL_CLIENT_MEMBER_ID", MEMBER.to_owned()),
        ],
    )
    .await?;
    task.abort();
    host.server.stop().await;
    Ok(())
}

/// Run one test file of `web/components` with `pnpm`, and require that both
/// of its cases pass.
async fn vitest(file: &str, env: &[(&str, String)]) -> anyhow::Result<()> {
    let components = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/components");
    let output = tokio::process::Command::new("pnpm")
        .current_dir(&components)
        .args(["exec", "vitest", "run", file])
        .envs(env.iter().map(|(name, value)| (name, value)))
        .output()
        .await
        .context("run the generated client test")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("2 passed"),
        "the generated client test passes and runs both cases:\n{stdout}\n{stderr}"
    );
    Ok(())
}

/// The org and project routes of the control serving root, on the org's
/// real `control` login, through `@wamn/control-org-client`. Boss holds
/// `org-admin`, Cat holds `project-admin` in `billing`, Ann is an active
/// member, and Dan is a user outside the org. The routes that write
/// application rows need the administration logins, so this test calls the
/// writes that refuse before them; `control_route_live` covers the rest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires: WAMN_FLOW_HTTP_COMPONENT"]
async fn control_routes_answer_through_the_generated_client() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["WAMN_FLOW_HTTP_COMPONENT"]);
    let flow_http_wasm = std::env::var("WAMN_FLOW_HTTP_COMPONENT")?;
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_client")?;
    let admin_url = test_database.url();
    let admin = control_fixture::connect(admin_url).await?;
    let control_url = control_fixture::install(&admin, admin_url).await?;
    let audience = wamn_platform_identity::control::control_audience(ORG)?;

    // The users and their grants, written as provisioning, and one password
    // login of each admin for the control audience.
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await?;
    let mut ids = Vec::new();
    for (email, name) in [
        ("boss@example.test", "Boss"),
        ("cat@example.test", "Cat"),
        ("ann@example.test", "Ann"),
        ("dan@example.test", "Dan"),
    ] {
        ids.push(
            create_or_reuse_user(&admin, email, name)
                .await?
                .principal_id,
        );
    }
    let [boss, cat, ann, dan]: [PrincipalId; 4] = ids.try_into().expect("four users");
    let grants = [
        (
            &boss,
            MemberGrants {
                org_admin: true,
                ..MemberGrants::default()
            },
        ),
        (
            &cat,
            MemberGrants {
                project_admins: vec!["billing".to_owned()],
                ..MemberGrants::default()
            },
        ),
        (&ann, MemberGrants::default()),
    ];
    for (principal, grants) in &grants {
        invite_member(&admin, principal, ORG, grants).await?;
    }
    for principal in [&boss, &cat] {
        admin
            .execute(
                "INSERT INTO identity.password_logins \
                   (id, principal_id, issuer, audience, authenticated_at, expires_at, renewal_expires_at) \
                 VALUES ($1::text::uuid, $1::text::uuid, $2, $3, now(), now() + interval '8 hours', \
                   now() + interval '30 minutes')",
                &[&principal.as_str(), &ISSUER, &audience],
            )
            .await?;
    }

    let control = Arc::new(control_fixture::connect(&control_url).await?);
    let writer = Arc::new(tokio::sync::Mutex::new(
        control_fixture::connect(&control_url).await?,
    ));
    let mut server = Server::start().await;
    let (keys, _key_clock) = server.cache();
    let (verifier, _token_clock) =
        wamn_session::verifier::SessionVerifier::with_test_clock(keys, ORG, &audience, 1000)?;
    let release = Arc::new(LoadedRelease::control_root());
    let routing = Arc::new(
        FlowHttpRouting::new(Some(Arc::clone(&release)), RouteInFlightLimit::default())
            .with_authenticator(Arc::new(ControlRouteAuthenticator::new(
                verifier,
                Arc::clone(&control),
            ))),
    );
    let delivery = Arc::new(HostRouteDelivery::new(
        release,
        HostRouteHandlers::Control {
            administration: None,
            control,
            writer,
            identity: None,
            org: ORG.to_owned(),
        },
        None,
    ));

    let engine = Arc::new(build_engine(&[])?);
    let flow_http = Component::new(engine.inner(), std::fs::read(&flow_http_wasm)?)?;
    let (endpoint, task) = serve(
        engine,
        flow_http,
        routing,
        delivery as Arc<dyn RouteDelivery>,
    )
    .await?;

    let token = |principal: &PrincipalId| {
        let mut session = claims();
        session["sub"] = json!(principal.as_str());
        session["aud"] = json!(audience);
        session["roles"] = json!([]);
        session["authority"] = json!({"login": principal.as_str()});
        signed(&header(), &session)
    };
    vitest(
        "test/control-org-client.live.test.ts",
        &[
            ("WAMN_CONTROL_ORG_CLIENT_BASE_URL", endpoint),
            ("WAMN_CONTROL_ORG_CLIENT_BOSS_TOKEN", token(&boss)),
            ("WAMN_CONTROL_ORG_CLIENT_CAT_TOKEN", token(&cat)),
            ("WAMN_CONTROL_ORG_CLIENT_BOSS_ID", boss.as_str().to_owned()),
            ("WAMN_CONTROL_ORG_CLIENT_CAT_ID", cat.as_str().to_owned()),
            ("WAMN_CONTROL_ORG_CLIENT_ANN_ID", ann.as_str().to_owned()),
            ("WAMN_CONTROL_ORG_CLIENT_DAN_ID", dan.as_str().to_owned()),
        ],
    )
    .await?;
    task.abort();
    server.stop().await;
    Ok(())
}
