//! Shell updates and saved requests across generated binding versions.
//!
//! Requires the local application's PostgreSQL, NATS and component inputs,
//! installed sync-bench dependencies, and Chrome. The driver builds its shells.

use std::path::Path;

use anyhow::Context as _;
use wamn_compiler::client_ir::ClientContractIr;
use wamn_compiler::client_ts::emit_ts_client;

/// Known flaky under load: wfe gap wamn-c361p. The waiting worker can stay
/// installed after skip-waiting, so the reload does not happen within 60 s.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn a_waiting_shell_reloads_after_pending_and_sending_entries_finish() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome",
    );
    super::local_outbox::run("release").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome"]
async fn an_old_binding_entry_gets_an_ordinary_refusal() -> anyhow::Result<()> {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_APPLICATION_COMPONENTS, env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_NATIVE_C_NATS_BIN, env:WAMN_SYNC_BENCH_DIST, bin:node, bin:google-chrome",
    );
    super::local_outbox::run("old-entry").await
}

/// R1 accepts a numeric packaging code. The installed R2 requires text.
pub(super) fn bindings(app: &Path, directory: &Path) -> anyhow::Result<()> {
    let manifest = wamn_compiler::manifest::PackageManifest::from_slice(&std::fs::read(
        wamn_compiler::package_manifest_path(app),
    )?)?;
    let routes = wamn_compiler::client_ir::published_routes(
        wamn_compiler::publication::read_package_attachments(app)?,
        &wamn_compiler::publication::read(app, wamn_compiler::publication::COMPONENTS)?,
    )?;
    let r2 = ClientContractIr::from_operations(
        &manifest.package.id,
        &manifest.compiled_operations,
        &routes,
    )?;
    let mut r1 = r2.clone();
    let operation = r1
        .models
        .iter_mut()
        .find(|model| model.name == "packaging")
        .context("WMS packaging model")?
        .operations
        .iter_mut()
        .find(|operation| operation.name == "create")
        .context("packaging create")?;
    let field = operation
        .input_fields
        .iter_mut()
        .find(|field| field.path == "packaging_code")
        .context("packaging code input")?;
    field.type_name = "int32".into();
    for (version, ir) in [("r1", r1), ("r2", r2)] {
        let files = emit_ts_client(&ir)?;
        let binding = files
            .iter()
            .find(|file| file.path() == "generated/client-ts/packaging.ts")
            .context("generated packaging binding")?;
        std::fs::write(directory.join(format!("{version}.ts")), binding.bytes())?;
    }
    Ok(())
}
