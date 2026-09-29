//! WMS business operations over real local components and PostgreSQL.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use wamn_catalog::ServingAttachment;
use wamn_integration_tests::local_application::{
    LocalApplication, LocalApplicationConfig, LocalPackage,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
async fn operations_and_replay() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("the WMS test crate has an application parent")?
        .to_owned();
    let mut attachments: BTreeMap<String, ServingAttachment> =
        wamn_schema_generator::route_schema::read_package_attachments(&app)?;
    // Business assertions use the authored direct move. Deployed tests retain
    // the composed label/blob boundary and its partial-completion assertions.
    attachments
        .get_mut("inventory-move-http")
        .context("WMS declares the move route")?
        .target = wamn_catalog::AttachmentTarget::Route {
        component: "wms".into(),
        operation: wamn_test_infrastructure::operations::sealed("wamn-wms:inventory/move"),
    };
    let components = PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let application = LocalApplication::start(LocalApplicationConfig {
        system_database_url: system.url(),
        database_url: project.url(),
        scratch: scratch.path(),
        component_directory: &components,
        flow_http_wasm: &flow_http,
        tenant: crate::environment::TENANT,
        org: crate::environment::ORG,
        project: crate::environment::PROJECT,
        environment: crate::environment::ENVIRONMENT,
        schema: crate::environment::SCHEMA,
        caller_role: "operator",
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
    // A revision near the top of int4, with room for the moves that follow.
    let initial_revision = 2_147_483_547_i64;
    crate::business_fixture::seed_fixture(&admin, initial_revision).await?;
    let runtime = crate::business_fixture::runtime_phase(application.endpoint.clone());
    let route = crate::wms_runtime_live::Route::local(
        application.endpoint.clone(),
        application.route_host.clone(),
        application.bearer.clone(),
    );
    crate::wms_runtime_live::assert_contention_and_replay(&route, &runtime, initial_revision)
        .await?;
    crate::wms_runtime_live::assert_remaining_operations(&route, &runtime).await?;
    crate::wms_runtime_live::assert_blank_codes_refuse(&route).await?;
    // Every balance equals the sum of its transactions, after the seed and
    // every command above.
    let disagreements = admin
        .query(include_str!("inventory_balance.sql"), &[])
        .await
        .context("run the inventory balance query")?;
    let disagreements = disagreements
        .iter()
        .map(|row| {
            (0..5)
                .map(|column| row.get::<_, Option<String>>(column).unwrap_or_default())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        disagreements.is_empty(),
        "every balance equals the sum of its transactions: {disagreements:?}"
    );
    drop(admin);
    connection.abort();
    application.shutdown().await?;
    Ok(())
}
