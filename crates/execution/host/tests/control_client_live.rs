//! The host routes through their generated TypeScript clients
//! (docs/architecture/execution.md#host-run-routes, wamn-a40n.7, wamn-a40n.8).
//!
//! Each test serves one set through the native platform handler and runs
//! its test file in `apps/platform_ui` against it with
//! session tokens. The application set answers an `admin` and a member, and
//! the control set answers an `org-admin` and a `project-admin`.

use std::path::Path;
use std::sync::Arc;
use wamn_broker::Publisher;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_authentication::route_authentication::ControlRouteAuthenticator;
use wamn_control_provision::PlatformComponent;
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{HostRouteDelivery, HostRouteHandlers};
use wamn_identity_protocol::PrincipalId;
use wamn_platform_identity::create_or_reuse_user;
use wamn_platform_identity::org::{MemberGrants, invite_member};

use http_body_util::BodyExt as _;

#[path = "../../../platform/postgres-capability/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also serves verifier lifecycle tests."
)]
mod session_fixture;
use session_fixture::{ISSUER, ORG, Server, claims, header, signed};

#[path = "support/application_fixture.rs"]
mod application_fixture;
use application_fixture::{ApplicationHost, MEMBER, PROJECT, application_host};

#[path = "support/control_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also provides project databases for host route tests."
)]
mod control_fixture;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_routes_answer_through_the_generated_client() -> anyhow::Result<()> {
    let mut host = application_host("control_client").await?;
    let ApplicationHost {
        admin,
        routing,
        delivery,
        first,
        ..
    } = &mut host;
    // The admin also holds project-admin, so its own admin row stays.
    sqlx::query(
        "INSERT INTO identity.project_roles VALUES ($1::text::uuid, $2, $3, 'project-admin')",
    )
    .bind(&first.as_str())
    .bind(&ORG)
    .bind(&PROJECT)
    .execute(&mut *admin)
    .await
    .map(|result| result.rows_affected())?;

    let (endpoint, _task) = serve(Arc::clone(routing), Arc::clone(delivery)).await?;

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
    host.server.stop().await;
    Ok(())
}

/// Run one test file of `apps/platform_ui` with `pnpm`, and require that both
/// of its cases pass.
async fn vitest(file: &str, env: &[(&str, String)]) -> anyhow::Result<()> {
    let platform_ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/platform_ui");
    // `pnpm exec` runs no package script, so the clients are written here.
    wamn_compiler::materialize_host_route_client(&wamn_compiler::host_route_client_output_root(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    ))?;
    let output = tokio::process::Command::new("pnpm")
        .current_dir(&platform_ui)
        .args(["exec", "vitest", "run", file])
        .envs(env.iter().map(|(name, value)| (name, value)))
        .output()
        .await
        .context("run the generated client test")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
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
async fn control_routes_answer_through_the_generated_client() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let test_database = postgres.create_database("control_client").await?;
    let admin_url = test_database.url();
    let mut admin = control_fixture::connect(admin_url).await?;
    let control_url = control_fixture::install(&mut admin, admin_url).await?;
    let audience = wamn_platform_identity::control::control_audience(ORG)?;

    // The users and their grants, written as provisioning, and one password
    // login of each admin for the control audience.
    sqlx::query("SELECT set_config('app.user_id', $1, false)")
        .bind(&PlatformComponent::Provisioning.principal_id().to_string())
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    let mut ids = Vec::new();
    for (email, name) in [
        ("boss@example.test", "Boss"),
        ("cat@example.test", "Cat"),
        ("ann@example.test", "Ann"),
        ("dan@example.test", "Dan"),
    ] {
        ids.push(
            create_or_reuse_user(&mut admin, email, name)
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
        invite_member(&mut admin, principal, ORG, grants).await?;
    }
    for principal in [&boss, &cat] {
        sqlx::query("INSERT INTO identity.password_logins \
                   (id, principal_id, issuer, audience, authenticated_at, expires_at, renewal_expires_at) \
                 VALUES ($1::text::uuid, $1::text::uuid, $2, $3, now(), now() + interval '8 hours', \
                   now() + interval '30 minutes')").bind(&principal.to_string().as_str()).bind(&ISSUER).bind(&audience).execute(&mut admin).await.map(|result| result.rows_affected())?;
    }

    let control = wamn_postgres::pool(&control_url, 4).await?;
    let writer = Arc::new(tokio::sync::Mutex::new(
        control_fixture::connect(&control_url).await?,
    ));
    let mut server = Server::start().await;
    let (keys, _key_clock) = server.cache();
    let (verifier, _token_clock) =
        wamn_identity_protocol::verifier::SessionVerifier::with_test_clock(
            keys, ORG, &audience, 1000,
        )?;
    let release = Arc::new(LoadedRelease::control_root());
    let routing = Arc::new(
        FlowHttpRouting::for_test(Some(Arc::clone(&release))).with_authenticator(Arc::new(
            ControlRouteAuthenticator::new(verifier, control.clone()),
        )),
    );
    let delivery = Arc::new(HostRouteDelivery::new(
        release,
        HostRouteHandlers::Control {
            status: Arc::new(wamn_execution_host::environment_status::KubernetesSource),
            administration: None,
            control,
            writer,
            identity: None,
            org: ORG.to_owned(),
            changes: Publisher::default(),
        },
    ));

    let (endpoint, _task) = serve(routing, delivery).await?;

    let token = |principal: &PrincipalId| {
        let mut session = claims();
        session["sub"] = json!(principal.to_string().as_str());
        session["aud"] = json!(audience);
        session["roles"] = json!([]);
        session["authority"] = json!({"login": principal.to_string().as_str()});
        signed(&header(), &session)
    };
    vitest(
        "test/control-org-client.live.test.ts",
        &[
            ("WAMN_CONTROL_ORG_CLIENT_BASE_URL", endpoint),
            ("WAMN_CONTROL_ORG_CLIENT_BOSS_TOKEN", token(&boss)),
            ("WAMN_CONTROL_ORG_CLIENT_CAT_TOKEN", token(&cat)),
            (
                "WAMN_CONTROL_ORG_CLIENT_BOSS_ID",
                boss.to_string().as_str().to_owned(),
            ),
            (
                "WAMN_CONTROL_ORG_CLIENT_CAT_ID",
                cat.to_string().as_str().to_owned(),
            ),
            (
                "WAMN_CONTROL_ORG_CLIENT_ANN_ID",
                ann.to_string().as_str().to_owned(),
            ),
            (
                "WAMN_CONTROL_ORG_CLIENT_DAN_ID",
                dan.to_string().as_str().to_owned(),
            ),
        ],
    )
    .await?;
    server.stop().await;
    Ok(())
}

/// Provide a socket for the same native request handler the host registers.
async fn serve(
    routing: Arc<FlowHttpRouting>,
    delivery: Arc<HostRouteDelivery>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            let routing = Arc::clone(&routing);
            let delivery = Arc::clone(&delivery);
            connections.spawn(async move {
                let service = hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| {
                        let routing = Arc::clone(&routing);
                        let delivery = Arc::clone(&delivery);
                        async move {
                            let request = request.map(|body| {
                                body.map_err(wasmtime_wasi_http::Error::from).boxed_unsync()
                            });
                            Ok::<_, std::convert::Infallible>(
                                delivery.http(&routing, request).await,
                            )
                        }
                    },
                );
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            });
            while connections.try_join_next().is_some() {}
        }
    });
    Ok((format!("http://{address}"), task))
}
