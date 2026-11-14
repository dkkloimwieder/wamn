//! Package and guest build orchestration shared by xtask and its command clients.
mod components;
pub mod eject;
pub mod kcl;
pub mod palette;
use anyhow::{Context as _, Result};
pub use components::{build_guest, guest_rustflags};
use std::{fs, path::Path};
pub use wamn_compiler::build::{BuildRequest, default_output_root, package_output};
use wamn_compiler::build_record::{BUILD_RECORD_FILE, BuildRecord};

/// Build a standalone component crate through the shared guest compiler.
pub async fn build_component(repository: &Path, manifest: &Path) -> Result<std::path::PathBuf> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest)
        .no_deps()
        .exec()?;
    let package = metadata
        .root_package()
        .context("component manifest has no root package")?;
    build_guest(repository, manifest, package.name.as_str(), None).await
}

/// Build into the package workspace output and write one completed record.
pub async fn build_package(request: &BuildRequest) -> Result<BuildRecord> {
    if std::env::var_os(wamn_compiler::kcl::KCL_ENV).is_none() {
        kcl::install(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .as_path(),
        )
        .await?;
    }
    let mut record = wamn_compiler::build::generate_package(request).await?;
    let output = package_output(&request.output_root, &record.package.id);
    wamn_compiler::materialize_client_consumers(
        &request.package_root,
        &record.package.id,
        &output.join("client-ts"),
    )?;
    record.outputs.components = components::compile_components(request, &record.package.id).await?;
    fs::write(output.join(BUILD_RECORD_FILE), record.to_bytes())
        .context("write completed build record")?;
    Ok(record)
}

/// Build the package's web client with Vite into `dist`.
///
/// # Errors
///
/// Refuses a missing web source, a failed process, or an unreadable output path.
pub async fn build_web_client(package_root: &Path, dist: &Path) -> Result<()> {
    let source = package_root.join("web");
    anyhow::ensure!(
        source.join("package.json").is_file(),
        "{} has no web/package.json",
        package_root.display()
    );
    let web = std::path::absolute(dist).with_context(|| format!("resolve {}", dist.display()))?;
    let output = tokio::process::Command::new("pnpm")
        .current_dir(&source)
        .args(["run", "build", "--outDir"])
        .arg(&web)
        .arg("--emptyOutDir")
        .output()
        .await
        .with_context(|| format!("run pnpm build in {}", source.display()))?;
    anyhow::ensure!(
        output.status.success(),
        "pnpm build in {} failed:\n{}{}",
        source.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Local source directories in the selected Cargo packages' dependency closure.
/// Cargo supplies package and feature resolution; registry sources stay pinned by its lock file.
pub fn source_dependencies(
    manifest: &Path,
    packages: &[String],
) -> Result<std::collections::BTreeSet<std::path::PathBuf>> {
    use std::collections::{BTreeMap, BTreeSet};
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest)
        .other_options(vec!["--offline".into()])
        .exec()?;
    let graph = metadata
        .resolve
        .context("Cargo metadata has no dependency graph")?;
    let nodes = graph
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect::<BTreeMap<_, _>>();
    let mut pending = Vec::new();
    for name in packages {
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name.as_str() == name)
            .with_context(|| format!("Cargo package {name} is absent"))?;
        pending.push(&package.id);
    }
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if visited.insert(id) {
            pending.extend(
                nodes
                    .get(id)
                    .context("Cargo package has no dependency node")?
                    .dependencies
                    .iter(),
            );
        }
    }
    metadata
        .packages
        .iter()
        .filter(|package| package.source.is_none() && visited.contains(&package.id))
        .map(|package| {
            package
                .manifest_path
                .parent()
                .context("Cargo manifest parent")
                .map(|path| path.as_std_path().to_owned())
        })
        .collect()
}

#[cfg(test)]
mod source_dependency_tests {
    use super::source_dependencies;

    #[test]
    fn cargo_closure_includes_transitive_path_dependencies() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver='2'\nmembers=['application','shared','scalar']\n",
        )?;
        for (name, dependency) in [
            ("application", Some("shared")),
            ("shared", Some("scalar")),
            ("scalar", None),
        ] {
            let package = root.join(name);
            std::fs::create_dir_all(package.join("src"))?;
            let mut manifest =
                format!("[package]\nname='{name}'\nversion='0.1.0'\nedition='2024'\n");
            if let Some(dependency) = dependency {
                manifest.push_str(&format!(
                    "[dependencies]\n{dependency}={{path='../{dependency}'}}\n"
                ));
            }
            std::fs::write(package.join("Cargo.toml"), manifest)?;
            std::fs::write(package.join("src/lib.rs"), "")?;
        }
        let roots = source_dependencies(&root.join("Cargo.toml"), &["application".to_owned()])?;
        assert_eq!(
            roots,
            ["application", "shared", "scalar"]
                .map(|name| root.join(name))
                .into()
        );
        Ok(())
    }
}
