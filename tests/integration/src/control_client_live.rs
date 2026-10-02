//! The application administration routes through their generated
//! TypeScript client (docs/plan/platform-ui.md §4.6, wamn-a40n.7).
//!
//! The test serves the application set of a fixture release through the
//! shipped flow-http component, as a host does, and runs
//! `web/components/test/control-client.live.test.ts` against it with two
//! session tokens: one of an `admin` and one of a member.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_engine::engine::build_engine;
use wamn_engine::router_delivery::RouteDelivery;
use wash_runtime::wasmtime::component::Component;

use crate::local_application::serve;

#[path = "../../../crates/platform/runtime/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared fixture also provides verifier-only fetch barriers."
)]
mod session_fixture;
use session_fixture::{ORG, claims, header, signed};

#[path = "../../../crates/execution/host/tests/support/application_fixture.rs"]
mod application_fixture;
use application_fixture::{ApplicationHost, MEMBER, PROJECT, application_host};

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
    let components = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/components");
    let output = tokio::process::Command::new("pnpm")
        .current_dir(&components)
        .args(["exec", "vitest", "run", "test/control-client.live.test.ts"])
        .env("WAMN_CONTROL_CLIENT_BASE_URL", &endpoint)
        .env("WAMN_CONTROL_CLIENT_ADMIN_TOKEN", token(&admin_claims))
        .env("WAMN_CONTROL_CLIENT_ADMIN_ID", first.as_str())
        .env("WAMN_CONTROL_CLIENT_MEMBER_TOKEN", token(&member_claims))
        .env("WAMN_CONTROL_CLIENT_MEMBER_ID", MEMBER)
        .output()
        .await
        .context("run the generated client test")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("2 passed"),
        "the generated client test passes and runs both cases:\n{stdout}\n{stderr}"
    );
    task.abort();
    host.server.stop().await;
    Ok(())
}
