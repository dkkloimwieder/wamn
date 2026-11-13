//! Emit client bindings from existing contracts without a database.
//!
//! With no arguments, emit the fixture used by the TypeScript runtime test.
//! Otherwise, pass application directories whose contracts already exist.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use wamn_compiler::client_ir::{ClientContractIr, published_routes};
use wamn_compiler::client_ts::{emit_ts_client, emit_ts_package_json};

#[path = "../tests/support/platform_fixture.rs"]
mod fixture;

fn main() -> Result<()> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap();
    let packages: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if packages.is_empty() {
        // The fixture reads the current `wamn build` output of
        // apps/platform_fixture, so this writes no package output.
        emit(
            &fixture::client_release(),
            "@wamn/sync-fixture-client",
            "0.0.0",
            &repository.join("web/sync-bench/src/generated/sync_fixture"),
        )?;
        return emit(
            &fixture::line_key_release(),
            "@wamn/sync-line-key-client",
            "0.0.0",
            &repository.join("web/sync-bench/src/generated/sync_line_key"),
        );
    }
    for package in packages {
        wamn_compiler::build_record::require_current(&package)?;
        let output = wamn_compiler::output_root(&package);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("wamn.json"))?)?;
        let descriptions = serde_json::from_value(manifest["compiled_operations"].clone())?;
        let attachments = wamn_compiler::publication::read_package_attachments(&package)?;
        let declarations =
            wamn_compiler::publication::read(&package, wamn_compiler::publication::COMPONENTS)?;
        let routes = published_routes(attachments, &declarations)?;
        let id = manifest["package"]["id"].as_str().context("package id")?;
        let ir = ClientContractIr::from_operations(id, &descriptions, &routes)?;
        emit(
            &ir,
            manifest["client_package"]["name"]
                .as_str()
                .context("client package")?,
            manifest["package"]["version"]
                .as_str()
                .context("package version")?,
            &output.join("client-ts"),
        )?;
        wamn_compiler::materialize_client_consumers(&package, id, &output.join("client-ts"))?;
    }
    Ok(())
}

fn emit(ir: &ClientContractIr, name: &str, version: &str, output: &Path) -> Result<()> {
    std::fs::create_dir_all(output)?;
    let mut files = emit_ts_client(ir)?;
    if output.file_name().is_some_and(|name| name == "client-ts") {
        files.push(emit_ts_package_json(name, version));
    }
    for file in files {
        let path = Path::new(file.path()).strip_prefix("generated/client-ts")?;
        std::fs::write(output.join(path), file.bytes())?;
    }
    println!("{}", output.display());
    Ok(())
}
