//! Emit client bindings from existing contracts without a database.
//!
//! With no arguments, emit the fixture used by the TypeScript runtime test.
//! Otherwise, pass application directories whose contracts already exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use wamn_schema_generator::client_ir::{ClientContractIr, published_routes};
use wamn_schema_generator::client_ts::{emit_ts_client, emit_ts_package_json};

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
        wamn_schema_generator::receipt::check_current(&package)?;
        let output = wamn_schema_generator::output_root(&package);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("wamn.json"))?)?;
        let mut contracts = BTreeMap::new();
        for model in std::fs::read_dir(output.join("contracts"))? {
            let model = model?.path();
            if !model.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(model)? {
                let path = entry?.path();
                let name = path
                    .strip_prefix(output.join("contracts"))?
                    .to_str()
                    .context("contract path")?
                    .to_owned();
                contracts.insert(name, std::fs::read(path)?);
            }
        }
        let read = |name: &str| {
            let path = output.join(name.strip_prefix("generated/").unwrap_or(name));
            let bytes = std::fs::read(&path).map_err(|_| {
                wamn_schema_generator::route_schema::RouteSchemaError::unknown(name)
            })?;
            wamn_schema_generator::route_schema::parse(name, &bytes)
        };
        let routes = published_routes(
            &package.join("publication/attachments.json"),
            &mut |name| {
                if output
                    .join(name.strip_prefix("generated/").unwrap_or(name))
                    .exists()
                {
                    read(name).map(Some)
                } else {
                    Ok(None)
                }
            },
            &mut |name| read(name),
        )?;
        let id = manifest["package"]["id"].as_str().context("package id")?;
        let ir = ClientContractIr::from_release_contracts(id, &contracts, &routes)?;
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
        wamn_schema_generator::materialize_client_consumers(
            &package,
            id,
            &output.join("client-ts"),
        )?;
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
