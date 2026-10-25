//! The packaging screen follows the change feed of a local WMS host
//! (Epic 2b, `docs/plan/wamn-sync.md`).
//!
//! The test serves WMS over real components and PostgreSQL, and runs
//! `web/sync-bench/bench/follow.mjs`, which opens the packaging screen in
//! headless Chrome. The screen lists once. A create through the host's route
//! then reaches it through the feed with no second list read, and a row that
//! the test deletes in the database leaves it. Build the screen first with
//! `pnpm --dir web/sync-bench run build`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use wamn_catalog::ServingAttachment;
use wamn_integration_tests::local_application::{
    LocalApplication, LocalApplicationConfig, LocalPackage,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

use crate::business_fixture::{LOCATION_A_ID, PACKAGING_ID};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN, node"]
async fn the_packaging_screen_follows_a_write_and_a_delete() -> anyhow::Result<()> {
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
        let mut driver = tokio::process::Command::new("node")
            .arg("bench/follow.mjs")
            .current_dir(&bench)
            .env("WAMN_ENDPOINT", &application.endpoint)
            .env("WAMN_ROUTE_HOST", &application.route_host)
            .env("WAMN_BEARER", &application.bearer)
            .env("WAMN_USER_ID", user.id().as_str())
            .env("WAMN_AUD", audience)
            .env("WAMN_LOCATION_ID", LOCATION_A_ID)
            .env("WAMN_FIRST_ID", PACKAGING_ID)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("start the screen driver")?;
        let mut stdin = driver.stdin.take().context("the driver's stdin")?;
        let mut lines = BufReader::new(driver.stdout.take().context("the driver's stdout")?).lines();
        let mut last = String::new();
        while let Some(line) = lines.next_line().await? {
            eprintln!("follow.mjs: {line}");
            if let Some(id) = line.strip_prefix("DELETE ") {
                // A delete with no operation route: the test is the writer,
                // and the history entry names it.
                admin
                    .batch_execute(&format!(
                        "BEGIN;\n\
                         SELECT set_config('app.user_id', '00000000-0000-4000-8000-0000000000f1', true), \
                                set_config('app.operation', 'admin:follow-delete', true);\n\
                         DELETE FROM wms.packaging WHERE id = '{id}';\n\
                         COMMIT;"
                    ))
                    .await
                    .context("delete the created row")?;
                stdin.write_all(b"deleted\n").await?;
                stdin.flush().await?;
            }
            last = line;
        }
        let status = driver.wait().await?;
        let outcome: serde_json::Value =
            serde_json::from_str(&last).with_context(|| format!("the driver's result: {last}"))?;
        anyhow::ensure!(
            status.success() && outcome["ok"] == serde_json::json!(true),
            "the screen follows the write and the delete: {outcome}"
        );
        anyhow::ensure!(
            outcome["reads"]["list"] == serde_json::json!(1),
            "the screen lists once: {outcome}"
        );
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
