//! Build application fixtures and the pinned ingress guest.

use anyhow::{Context as _, Result, ensure};
use cargo_metadata::semver::{Prerelease, Version};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use wamn_build::{
    BuildMode, BuildRequest, DescribeMode, GuestTarget, default_output_root, package_output,
};

const ROLLBACK_FIXTURE: &str = "platform_fixture";

/// The application roots: each directory under `apps/` with a `wamn.k`.
pub(super) fn apps(root: &Path) -> Result<Vec<PathBuf>> {
    let mut apps = std::fs::read_dir(root.join("apps"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    apps.retain(|path| path.join("wamn.k").is_file());
    apps.sort_unstable();
    Ok(apps)
}

pub(super) async fn run(root: &Path) -> Result<()> {
    let database_url = std::env::var("DATABASE_URL")
        .context("set DATABASE_URL to build the rollback fixtures with PostgreSQL")?;
    let describe = DescribeMode::from_options(Some(&database_url), true, false)?;
    let mut pending = VecDeque::new();
    for path in apps(root)? {
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
    let healthy_rollout = rollback_variant(
        &root.join("target"),
        root,
        &fixture,
        &fixture_version,
        Prerelease::EMPTY,
        describe,
    )
    .await
    .context("prepare the second rollback fixture build")?;
    let failed_rollout = rollback_variant(
        &root.join("evidence"),
        root,
        &fixture,
        &fixture_version,
        Prerelease::new("failed-rollout")?,
        describe,
    )
    .await
    .context("prepare the failed rollout fixture build")?;
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
    exports.push(("WAMN_ROLLBACK_FIXTURE_V2_SOURCE", healthy_rollout));
    exports.push(("WAMN_ROLLBACK_FIXTURE_FAILED_SOURCE", failed_rollout));
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

async fn rollback_variant(
    workspace: &Path,
    root: &Path,
    fixture: &Path,
    base: &Version,
    prerelease: Prerelease,
    describe: DescribeMode<'_>,
) -> Result<PathBuf> {
    // Each workspace gives the same package ID its own build directory.
    fs::create_dir_all(workspace).with_context(|| format!("create {}", workspace.display()))?;
    let lock_root = workspace.to_owned();
    let _lock =
        tokio::task::spawn_blocking(move || wamn_codegen::fingerprint::lock(&lock_root)).await??;
    let staging = tempfile::tempdir_in(workspace)?;
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
    let original = format!("version = \"{base}\"");
    ensure!(
        source.matches(&original).count() == 1,
        "expected one declaration of {original} in {}",
        authored.display()
    );
    let patch = base
        .patch
        .checked_add(1)
        .context("platform fixture patch version overflow")?;
    let failed = !prerelease.is_empty();
    let version = Version {
        patch,
        pre: prerelease,
        ..base.clone()
    }
    .to_string();
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
    let source = source.replace(row, "include!(\"rollback_list_row.rs\")");
    let source = if failed {
        // Publication checks the platform import identity. The serving host links
        // the type, which deliberately requires a function it does not supply.
        let wit = staged.join("component/wit");
        fs::write(
            wit.join("rollout-failure.wit"),
            "package wasi:random@0.2.12;\ninterface rollout-fixture { probe: async func(); }\n",
        )?;
        let world = wit.join("authored.wit");
        let wit = fs::read_to_string(&world)?;
        fs::write(
            world,
            wit.replace(
                "world fixture {",
                "world fixture {\n  import wasi:random/rollout-fixture@0.2.12;",
            ),
        )?;
        let lib = staged.join("component/src/lib.rs");
        let lib_source = fs::read_to_string(&lib)?;
        fs::write(
            &lib,
            lib_source.replace(
                "\"wit/authored.wit\",",
                "\"wit/rollout-failure.wit\",\n        \"wit/authored.wit\",",
            ),
        )?;
        let list = "        widget::list(connection)\n";
        ensure!(
            source.matches(list).count() == 1,
            "expected one widget list call in {}",
            handler.display()
        );
        source.replace(
            list,
            "        crate::wasi::random::rollout_fixture::probe().await;\n        widget::list(connection)\n",
        )
    } else {
        source
    };
    fs::write(&handler, source)?;
    fs::write(
        handler.with_file_name("rollback_list_row.rs"),
        include_bytes!("../../tests/journeys/fixtures/rollback/list_row.rs"),
    )?;
    fs::write(
        staged.join("migrations/0100_rollout_fixture.sql"),
        include_bytes!("../../tests/journeys/fixtures/rollback/0100_rollout_fixture.sql"),
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
    let request = BuildRequest::new(&variant, &output, describe);
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
