//! The packaging screen restores offline rows, corrects refusals and retries lost replies.
//!
//! Uses the same local host and inputs as `local_feed`. Build `web/sync-bench`
//! before running these ignored tests. Each case uses fresh PostgreSQL databases.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context as _;
use wamn_catalog::ServingAttachment;
use wamn_integration_tests::local_application::{
    LocalApplication, LocalApplicationConfig, LocalPackage,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::business_fixture::{LOCATION_A_ID, PACKAGING_ID};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN, node"]
async fn offline_creates_survive_reload_and_follow_the_feed() -> anyhow::Result<()> {
    run("offline").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN, node"]
async fn a_refused_create_succeeds_after_correction() -> anyhow::Result<()> {
    run("refusal").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN, node"]
async fn a_lost_create_reply_replays_without_another_row() -> anyhow::Result<()> {
    run("lost-reply").await
}

async fn run(scenario: &str) -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
        "WAMN_NATIVE_C_NATS_BIN",
        "node",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("the WMS test crate has an application parent")?
        .to_owned();
    let attachments: BTreeMap<String, ServingAttachment> =
        wamn_schema_generator::route_schema::read_package_attachments(&app)?;
    let components = PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    std::fs::write(
        scratch.path().join("environment.k"),
        include_str!("../../../tests/integration/fixtures/local-environments/wms.k"),
    )?;
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system.url(),
        database_url: project.url(),
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        tenant: crate::environment::identity().tenant.as_str(),
        org: crate::environment::identity().org.as_str(),
        project: crate::environment::identity().project.as_str(),
        environment: crate::environment::identity().environment.as_str(),
        schema: crate::environment::SCHEMA,
        caller_role: "admin",
        route_host: "wms.local.test",
        packages: &[LocalPackage {
            root: &app,
            component: "wms",
            wirings: &[],
        }],
        attachments: &attachments,
    })
    .await?;
    let (admin, connection) = tokio_postgres::connect(project.url(), tokio_postgres::NoTls).await?;
    let connection = tokio::spawn(connection);
    let (identity, identity_connection) =
        tokio_postgres::connect(system.url(), tokio_postgres::NoTls).await?;
    let identity_connection = tokio::spawn(identity_connection);
    let result = async {
        let scope = crate::environment::identity();
        let subject = wamn_platform_identity::operator_subject(
            &scope.org,
            &scope.project,
            &scope.environment,
        )?;
        let user = wamn_platform_identity::resolve_subject(
            &identity,
            wamn_platform_identity::PrincipalType::Service,
            &subject,
        )
        .await?
        .context("the local application's operator principal")?;
        // This PAT fixture has no registered session instance or identity service.
        // The browser's mocked session uses this test-only instance suffix.
        let audience = wamn_control_provision::session_target::session_audience(
            &wamn_control_registry::Triple::new(
                scope.org.clone(),
                scope.project.clone(),
                scope.environment.as_str(),
            ),
            "localapp",
        )?;
        crate::business_fixture::seed_fixture(&admin, 1).await?;
        let bench = app.join("../../web/sync-bench").canonicalize()?;
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(180),
            tokio::process::Command::new("node")
                .arg("bench/outbox.mjs")
                .arg(scenario)
                .current_dir(&bench)
                .env("WAMN_ENDPOINT", &application.endpoint)
                .env("WAMN_ROUTE_HOST", &application.route_host)
                .env("WAMN_BEARER", &application.bearer)
                .env("WAMN_USER_ID", user.id().as_str())
                .env("WAMN_AUD", audience)
                .env("WAMN_LOCATION_ID", LOCATION_A_ID)
                .env("WAMN_FIRST_ID", PACKAGING_ID)
                .stdin(Stdio::null())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("the screen driver exceeded 180 seconds")??;
        let stdout = String::from_utf8(output.stdout)?;
        eprintln!("outbox.mjs: {stdout}");
        let last = stdout.lines().last().context("the driver's result")?;
        let outcome: serde_json::Value = serde_json::from_str(last)?;
        anyhow::ensure!(
            output.status.success() && outcome["ok"] == serde_json::json!(true),
            "the {scenario} screen case: {outcome}"
        );
        let ids: Vec<String> = serde_json::from_value(outcome["ids"].clone())?;
        let expected = if scenario == "offline" { 3 } else { 1 };
        anyhow::ensure!(ids.len() == expected, "created ids: {outcome}");
        let rows = admin
            .query(
                "SELECT id::text, packaging_code, row_version FROM wms.packaging WHERE id::text <> $1",
                &[&PACKAGING_ID],
            )
            .await?;
        anyhow::ensure!(rows.len() == expected, "exactly {expected} new database rows");
        for row in rows {
            let id: String = row.get(0);
            let code: String = row.get(1);
            let version: i32 = row.get(2);
            anyhow::ensure!(ids.contains(&id), "database row {id} came from this screen");
            anyhow::ensure!(version == 1, "row {id} was created once: row_version {version}");
            let valid_code = match scenario {
                "offline" => ["OFFLINE-1", "OFFLINE-2", "OFFLINE-3"].contains(&code.as_str()),
                "refusal" => code == "CORRECTED",
                "lost-reply" => code == "LOST-REPLY",
                _ => unreachable!("the test names one of three cases"),
            };
            anyhow::ensure!(valid_code, "the stored packaging code is {code}");
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    drop(admin);
    connection.abort();
    drop(identity);
    identity_connection.abort();
    application.shutdown().await?;
    result
}
