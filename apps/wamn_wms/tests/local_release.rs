//! Shell updates and saved requests across generated binding versions.
//!
//! Requires the local application's PostgreSQL, NATS and component inputs,
//! installed sync-bench dependencies, and Chrome. The driver builds its shells.

use std::path::Path;

use anyhow::Context as _;
use wamn_schema_generator::client_ir::ClientContractIr;
use wamn_schema_generator::client_ts::emit_ts_client;

/// Known flaky under load: wfe gap wamn-c361p. The waiting worker can stay
/// installed after skip-waiting, so the reload does not happen within 60 s.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN, node, Chrome"]
async fn a_waiting_shell_reloads_after_pending_and_sending_entries_finish() -> anyhow::Result<()> {
    super::local_outbox::run("release").await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_APPLICATION_COMPONENTS, WAMN_FLOW_HTTP_COMPONENT, WAMN_NATIVE_C_NATS_BIN, node, Chrome"]
async fn an_old_binding_entry_gets_an_ordinary_refusal() -> anyhow::Result<()> {
    super::local_outbox::run("old-entry").await
}

/// R1 accepts a numeric packaging code. The installed R2 requires text.
pub(super) fn bindings(app: &Path, directory: &Path) -> anyhow::Result<()> {
    let r2 = ClientContractIr::from_release(
        "wamn_wms",
        &wamn_schema_generator::output_root(app).join("contracts"),
        &app.join("publication/attachments.json"),
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
