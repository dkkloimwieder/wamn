//! Build application fixtures and the pinned ingress guest.

use anyhow::{Context as _, Result, ensure};
use cargo_metadata::semver::Version;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use wamn_build::{
    BuildMode, BuildRequest, DescribeMode, GuestTarget, default_output_root, package_output,
};

const ROLLBACK_FIXTURE: &str = "platform_fixture";

/// The application roots: each directory under `apps/` with a `wamn.k`.
pub(super) fn applications(root: &Path) -> Result<Vec<PathBuf>> {
    let mut applications = std::fs::read_dir(root.join("apps"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    applications.retain(|path| path.join("wamn.k").is_file());
    applications.sort_unstable();
    Ok(applications)
}

pub(super) async fn run(root: &Path) -> Result<()> {
    let mut pending = VecDeque::new();
    for path in applications(root)? {
        let source = wamn_build::prepare_source(&path)
            .await
            .with_context(|| format!("read application {}", path.display()))?;
        pending.push_back(source);
    }
    let fixture_version = pending
        .iter()
        .find(|source| source.package().id.as_str() == ROLLBACK_FIXTURE)
        .context("missing platform fixture source")?
        .package()
        .version
        .as_str()
        .parse::<Version>()
        .context("parse the platform fixture version")?;
    pending
        .make_contiguous()
        .sort_by_key(|source| !source.bases().is_empty());

    let mut built = BTreeMap::new();
    while !pending.is_empty() {
        let ready = pending.iter().position(|source| {
            source
                .bases()
                .values()
                .all(|base| built.contains_key(&base.package))
        });
        let Some(source) = ready.and_then(|index| pending.remove(index)) else {
            let waiting = pending
                .iter()
                .map(|source| source.package().id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("missing or cyclic application base dependencies for {waiting}");
        };
        let output_root = default_output_root(source.root());
        let request = BuildRequest::new(source.root(), &output_root, DescribeMode::Cached);
        let result = wamn_build::build_prepared(&request, &source, BuildMode::Published)
            .await
            .with_context(|| format!("build application {}", source.package().id))?;
        let id = result.package.id;
        let output = package_output(&output_root, &id);
        eprintln!("Prepared {} at {}", id, output.display());
        built.insert(id, output);
    }

    let fixture = root.join("apps").join(ROLLBACK_FIXTURE);
    let rollback_variant = rollback_variant(root, &fixture, fixture_version)
        .await
        .context("prepare the second rollback fixture build")?;
    let ingress = wamn_build::build_component(root, &root.join("apps/guests/ingress/Cargo.toml"))
        .await
        .context("build pinned ingress component")?;
    let workflow = wamn_build::build_guest(
        root,
        &root.join("apps/Cargo.toml"),
        "workflow-service",
        GuestTarget::WasiP3,
    )
    .await
    .context("build workflow service component")?;
    // The component consumers name these variables and components.
    let mut exports = Vec::new();
    for (name, id, component) in [
        (
            "WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM",
            ROLLBACK_FIXTURE,
            "fixture.wasm",
        ),
        (
            "WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM",
            "platform_fixture_overlay",
            "fixture_overlay.wasm",
        ),
        ("WAMN_EDGE_DEVICE_COMPONENT", "edge_device", "device.wasm"),
    ] {
        let output = built
            .get(id)
            .with_context(|| format!("missing application {id}"))?;
        exports.push((name, output.join("components").join(component)));
    }
    exports.push(("WAMN_INGRESS_COMPONENT", ingress));
    exports.push(("WAMN_WORKFLOW_SERVICE_COMPONENT", workflow));
    for (name, path) in &exports {
        ensure!(path.is_file(), "missing {name} at {}", path.display());
    }
    exports.push(("WAMN_ROLLBACK_FIXTURE_V1_SOURCE", fixture));
    exports.push(("WAMN_ROLLBACK_FIXTURE_V2_SOURCE", rollback_variant));
    let mut lines = Vec::new();
    for (name, path) in exports {
        let value = path.to_str().context("prepared path is not valid UTF-8")?;
        lines.push(format!("export {name}='{}'", value.replace('\'', "'\\''")));
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

async fn rollback_variant(root: &Path, fixture: &Path, mut version: Version) -> Result<PathBuf> {
    // A second workspace gives the same package ID its own build directory.
    let workspace = root.join("target");
    fs::create_dir_all(&workspace).with_context(|| format!("create {}", workspace.display()))?;
    let lock_root = workspace.clone();
    let _lock =
        tokio::task::spawn_blocking(move || wamn_codegen::fingerprint::lock(&lock_root)).await??;
    let staging = tempfile::tempdir_in(&workspace)?;
    let staged = staging.path().join(ROLLBACK_FIXTURE);
    let status = Command::new("cp")
        .arg("-R")
        .arg(fixture)
        .arg(&staged)
        .status()
        .await
        .context("copy the platform fixture")?;
    ensure!(
        status.success(),
        "copy {} failed: {status}",
        fixture.display()
    );

    let authored = staged.join("wamn.k");
    let source = fs::read_to_string(&authored)?;
    let original = format!("version = \"{version}\"");
    ensure!(
        source.matches(&original).count() == 1,
        "expected one declaration of {original} in {}",
        authored.display()
    );
    version.patch = version
        .patch
        .checked_add(1)
        .context("platform fixture patch version overflow")?;
    let version = version.to_string();
    fs::write(
        &authored,
        source.replace(&original, &format!("version = \"{version}\"")),
    )?;

    let handler = staged.join("component/src/widget.rs");
    let source = fs::read_to_string(&handler)?;
    let row = "|row| operation::row!(row, contract::ListRow)";
    ensure!(
        source.matches(row).count() == 1,
        "expected one widget list row conversion in {}",
        handler.display()
    );
    fs::write(
        &handler,
        source.replace(row, "include!(\"rollback_list_row.rs\")"),
    )?;
    fs::write(
        handler.with_file_name("rollback_list_row.rs"),
        include_str!("../../tests/journeys/fixtures/rollback/list_row.rs"),
    )?;

    let apps = root.join("apps");
    let mut manifest: toml::Table = fs::read_to_string(apps.join("Cargo.toml"))?.parse()?;
    let component: toml::Table =
        fs::read_to_string(staged.join("component/Cargo.toml"))?.parse()?;
    let settings = manifest
        .get_mut("workspace")
        .and_then(toml::Value::as_table_mut)
        .context("read guest workspace settings")?;
    settings.insert(
        "members".to_owned(),
        toml::Value::Array(vec![toml::Value::String(format!(
            "{ROLLBACK_FIXTURE}/component"
        ))]),
    );
    for section in ["dependencies", "package"] {
        settings
            .get_mut(section)
            .and_then(toml::Value::as_table_mut)
            .with_context(|| format!("read guest workspace {section}"))?
            .retain(|name, _| {
                component
                    .get(section)
                    .and_then(|table| table.get(name))
                    .and_then(|value| value.get("workspace"))
                    .and_then(toml::Value::as_bool)
                    == Some(true)
            });
    }
    settings
        .get_mut("dependencies")
        .and_then(|dependencies| dependencies.get_mut("wamn-guest"))
        .context("read rollback fixture guest dependency")?["path"] =
        toml::Value::String("../apps/crates/guest".to_owned());
    fs::write(workspace.join("Cargo.toml"), toml::to_string(&manifest)?)?;
    fs::copy(apps.join("Cargo.lock"), workspace.join("Cargo.lock"))?;

    let variant = workspace.join(ROLLBACK_FIXTURE);
    if variant.is_dir() {
        fs::remove_dir_all(&variant)?;
    }
    fs::rename(&staged, &variant)?;
    let output = default_output_root(&variant);
    let request = BuildRequest::new(&variant, &output, DescribeMode::Cached);
    let built = wamn_build::build_package(&request).await?;
    ensure!(
        built.package.version.as_str() == version,
        "expected rollback fixture version {version}, found {}",
        built.package.version
    );
    let output = package_output(&output, &built.package.id);
    eprintln!("Prepared rollback variant at {}", output.display());
    Ok(variant)
}
