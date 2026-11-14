//! Dependency declarations inherit identities from their owning workspace.

use super::Problems;
use anyhow::{Context as _, ensure};
use std::path::Path;

pub(super) fn report_problems(root: &Path, problems: &mut Problems) {
    for manifest in ["Cargo.toml", "apps/Cargo.toml"] {
        if let Err(error) = workspace(&root.join(manifest), problems) {
            problems.push(format!("{manifest}: {error:#}"));
        }
    }
    if let Err(error) = runtime(root) {
        problems.push(format!("runtime dependencies: {error:#}"));
    }
}

fn workspace(manifest: &Path, problems: &mut Problems) -> anyhow::Result<()> {
    let source: toml::Value = toml::from_str(&std::fs::read_to_string(manifest)?)?;
    let governed = source["workspace"]["dependencies"]
        .as_table()
        .context("workspace dependencies")?;
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest)
        .no_deps()
        .other_options(vec!["--locked".into(), "--offline".into()])
        .exec()?;
    for package in metadata.workspace_packages() {
        let source: toml::Value =
            toml::from_str(&std::fs::read_to_string(&package.manifest_path)?)?;
        declarations(
            &source,
            package.name.as_str(),
            governed,
            package.manifest_path.as_std_path(),
            problems,
        );
    }
    // The serving runtime requires the persistent compilation cache.
    if let Some(wasmtime) = governed.get("wasmtime") {
        ensure!(
            wasmtime["features"]
                .as_array()
                .is_some_and(|features| features
                    .iter()
                    .any(|feature| feature.as_str() == Some("cache"))),
            "Wasmtime requires the cache feature"
        );
    }
    Ok(())
}

fn declarations(
    value: &toml::Value,
    package: &str,
    governed: &toml::Table,
    manifest: &Path,
    problems: &mut Problems,
) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, value) in table {
        if matches!(
            key.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) {
            for (name, declaration) in value.as_table().into_iter().flatten() {
                let renamed = declaration
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(name);
                if !(renamed.starts_with("wamn-") || governed.contains_key(name)) {
                    continue;
                }
                let own_test = key == "dev-dependencies"
                    && name == package
                    && declaration.get("path").and_then(toml::Value::as_str) == Some(".");
                problems.require(
                    own_test
                        || declaration.get("workspace").and_then(toml::Value::as_bool)
                            == Some(true),
                    || {
                        format!(
                            "{}: {name} must inherit its workspace identity",
                            manifest.display()
                        )
                    },
                );
            }
        } else {
            declarations(value, package, governed, manifest, problems);
        }
    }
}

// Cross-package version agreement and source identity are not cargo-deny bans.
fn runtime(root: &Path) -> anyhow::Result<()> {
    let source: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
    let dependencies = &source["workspace"]["dependencies"];
    let version = dependencies["wasmtime"]["version"]
        .as_str()
        .context("Wasmtime version")?
        .trim_start_matches('=');
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .other_options(vec!["--locked".into(), "--offline".into()])
        .exec()?;
    let runtime = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == "wash-runtime")
        .context("wash-runtime dependency")?;
    let upstream = &dependencies["wash-runtime"];
    let upstream_source = format!(
        "git+{}?tag={}#",
        upstream["git"].as_str().context("runtime repository")?,
        upstream["tag"].as_str().context("runtime tag")?
    );
    ensure!(
        runtime
            .source
            .as_ref()
            .is_some_and(|source| source.repr.starts_with(&upstream_source)),
        "wash-runtime must resolve directly to its workspace release tag"
    );
    for package in metadata.packages.iter().filter(|package| {
        package.name.as_str() == "async-nats"
            || package.name.as_str() == "wasmtime"
            || package.name.as_str().starts_with("wasmtime-")
    }) {
        let version = if package.name.as_str() == "async-nats" {
            dependencies["async-nats"]["version"]
                .as_str()
                .context("async-nats version")?
                .trim_start_matches('=')
        } else {
            version
        };
        ensure!(
            package.version.to_string() == version
                && package
                    .source
                    .as_ref()
                    .is_some_and(cargo_metadata::Source::is_crates_io),
            "{} must use its workspace version from crates.io",
            package.name
        );
    }
    Ok(())
}
