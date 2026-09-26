//! Bounded command histories compared with the independent executable model.

mod adapter;
mod cases;

#[expect(
    unexpected_cfgs,
    reason = "the imported oracle also compiles directly with Kani"
)]
#[expect(
    unused_attributes,
    reason = "the oracle declares its standalone crate type"
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the unchanged finite oracle uses booleans for two-value identities"
)]
#[path = "../formal/model.rs"]
mod oracle;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use proptest::test_runner::{
    Config, FileFailurePersistence, RngSeed, TestCaseError, TestError, TestRunner,
};
use wamn_catalog::ServingAttachment;
use wamn_integration_tests::local_application::{
    LocalApplication, LocalApplicationConfig, LocalPackage,
};
use wamn_test_infrastructure::scratch::ScratchRoot;

type FailureIdentity = (&'static str, &'static str, &'static str, &'static str);

fn failure_identity(error: &anyhow::Error) -> Option<FailureIdentity> {
    error.downcast_ref::<adapter::Mismatch>().map(|failure| {
        (
            failure.property,
            failure.operation,
            failure.expected,
            failure.observed,
        )
    })
}

#[test]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT"]
fn command_histories() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_APPLICATION_COMPONENTS",
        "WAMN_FLOW_HTTP_COMPONENT",
    ]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_test_postgres::database();
    let project = wamn_test_postgres::database();
    let scratch = ScratchRoot::create()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("the WMS test crate has an application parent")?
        .to_owned();
    let mut attachments: BTreeMap<String, ServingAttachment> =
        wamn_schema_generator::route_schema::read_package_attachments(&root)?;
    attachments
        .get_mut("inventory-move-http")
        .context("the direct move attachment exists")?
        .target = wamn_catalog::AttachmentTarget::Route {
        component: "wms".into(),
        operation: "wamn-wms:inventory/move@1.0.0".into(),
    };
    let components = PathBuf::from(std::env::var("WAMN_APPLICATION_COMPONENTS")?);
    let flow_http = PathBuf::from(std::env::var("WAMN_FLOW_HTTP_COMPONENT")?);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let application = runtime.block_on(LocalApplication::start(LocalApplicationConfig {
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
        caller_role: "route-caller",
        route_host: "wms.local.test",
        packages: &[LocalPackage {
            root: &root,
            component: "wms",
            wirings: &[],
        }],
        attachments: &attachments,
    }))?;
    let result = (|| {
        let (db, connection) = runtime.block_on(tokio_postgres::connect(
            project.url(),
            tokio_postgres::NoTls,
        ))?;
        let connection = runtime.spawn(connection);
        let result = run(&runtime, &db, &application);
        drop(db);
        connection.abort();
        result
    })();
    let shutdown = runtime.block_on(application.shutdown());
    result?;
    shutdown
}

fn run(
    runtime: &tokio::runtime::Runtime,
    db: &tokio_postgres::Client,
    application: &LocalApplication,
) -> anyhow::Result<()> {
    let run_case = |history: &cases::History| {
        runtime.block_on(adapter::run_history(
            db,
            &application.endpoint,
            &application.route_host,
            &application.bearer,
            history,
        ))
    };
    let examples = cases::examples();
    let mut example_failures = Vec::new();
    for (index, history) in examples.iter().enumerate() {
        match run_case(history) {
            Ok(()) => println!("WMS_CONFORMANCE example={index} passed"),
            Err(error) if failure_identity(&error).is_some() => {
                let detail = format!("deterministic history {index}: {error:#}; {history:#?}");
                eprintln!("WMS_CONFORMANCE {detail}");
                example_failures.push(detail);
            }
            Err(error) => return Err(error.context(format!("deterministic history {index}"))),
        }
    }
    let count =
        std::env::var("WAMN_WMS_HISTORY_CASES").map_or(Ok(16), |value| value.parse::<u32>())?;
    anyhow::ensure!(count > 0, "WAMN_WMS_HISTORY_CASES must be positive");
    let seed =
        std::env::var("WAMN_WMS_HISTORY_SEED").map_or(Ok(43_017), |value| value.parse::<u64>())?;
    let mut runner = TestRunner::new(Config {
        cases: count,
        rng_seed: RngSeed::Fixed(seed),
        max_shrink_iters: 128,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/differential/regressions.txt"
        )))),
        ..Config::default()
    });
    let target = RefCell::new(None);
    let infrastructure = RefCell::new(None);
    let generated = runner.run(&cases::strategy(), |history| {
        if infrastructure.borrow().is_some() {
            return Ok(());
        }
        match run_case(&history) {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Some(identity) = failure_identity(&error) {
                    let mut target = target.borrow_mut();
                    if target.is_some_and(|original| original != identity) {
                        return Err(TestCaseError::reject(
                            "different conformance property or outcome",
                        ));
                    }
                    *target = Some(identity);
                    Err(TestCaseError::fail(format!("{error:#}")))
                } else {
                    *infrastructure.borrow_mut() = Some(format!("{error:#}"));
                    Err(TestCaseError::reject(
                        "application test infrastructure failed",
                    ))
                }
            }
        }
    });
    if let Some(error) = infrastructure.into_inner() {
        anyhow::bail!("conformance infrastructure failed, not a business counterexample: {error}");
    }
    match generated {
        Ok(()) => {}
        Err(TestError::Fail(reason, minimized)) => {
            let reproduced = run_case(&minimized);
            let repeated = reproduced.as_ref().err().and_then(failure_identity);
            anyhow::ensure!(
                repeated == target.into_inner(),
                "minimized mismatch did not reproduce: {reproduced:?}; history={minimized:#?}"
            );
            anyhow::bail!(
                "unclassified conformance mismatch; seed={seed}; reason={reason}; minimized history={minimized:#?}"
            );
        }
        Err(error) => anyhow::bail!("history generation aborted: {error}"),
    }
    anyhow::ensure!(
        example_failures.is_empty(),
        "unclassified deterministic discrepancies: {}",
        example_failures.join("\n")
    );
    println!(
        "WMS_CONFORMANCE passed generated={count} examples={} seed={seed}",
        examples.len()
    );
    Ok(())
}
