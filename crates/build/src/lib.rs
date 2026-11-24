//! Package and guest build orchestration shared by xtask and its command clients.
mod components;
pub mod composer;
pub mod dev;
pub mod eject;
#[cfg(target_os = "linux")]
mod git_source;
mod guest_workspace;
mod inputs;
pub mod kcl;
pub mod palette;
pub mod source_build;
pub mod source_deploy;
#[cfg(target_os = "linux")]
pub mod watch;
use anyhow::{Context as _, Result};
pub use components::{GuestTarget, build_guest, guest_rustflags};
pub use inputs::paths as build_inputs;
use std::path::Path;
pub use wamn_codegen::build::{
    BuildRequest, DescribeMode, PreparedSource, default_output_root, package_output,
};
pub use wamn_codegen::database::BuildDatabase;
use wamn_codegen::fingerprint::{self, Fingerprint};
use wamn_component_policy::PackageManifest;
use wamn_digest::Digest;

/// Build a standalone component crate through the shared guest compiler.
pub async fn build_component(repository: &Path, manifest: &Path) -> Result<std::path::PathBuf> {
    let manifest = manifest.canonicalize()?;
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(&manifest)
        .no_deps()
        .exec()?;
    let package = metadata
        .packages
        .iter()
        .find(|package| package.manifest_path.as_std_path() == manifest)
        .context("component manifest has no package")?;
    build_guest(
        repository,
        &manifest,
        package.name.as_str(),
        GuestTarget::WasiP2,
    )
    .await
}

/// The version policy used to build an authored package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum BuildMode {
    Published,
    Development,
}

/// Package identity and exact compiled manifest digest.
#[derive(Debug)]
pub struct BuiltPackage {
    pub package: wamn_package::manifest::PackageIdentity,
    pub manifest_digest: Digest,
}

/// Install pinned KCL if needed and compile one frozen authored document.
pub async fn prepare_source(root: &Path) -> Result<PreparedSource> {
    if std::env::var_os(wamn_codegen::kcl::KCL_ENV).is_none() {
        kcl::install(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .as_path(),
        )
        .await?;
    }
    wamn_codegen::build::prepare_source(&root.canonicalize()?)
}

/// Build into the package workspace output.
pub async fn build_package(
    database: &dyn BuildDatabase,
    request: &BuildRequest<'_>,
) -> Result<BuiltPackage> {
    let source = prepare_source(&request.package_root).await?;
    build_prepared(database, request, &source, BuildMode::Published).await
}

/// Reuse the authored package already compiled during discovery.
pub async fn build_prepared(
    database: &dyn BuildDatabase,
    request: &BuildRequest<'_>,
    source: &PreparedSource,
    mode: BuildMode,
) -> Result<BuiltPackage> {
    let lock_root = request.output_root.clone();
    let _lock = tokio::task::spawn_blocking(move || fingerprint::lock(&lock_root)).await??;
    let output_root = request.output_root.canonicalize()?;
    let request = BuildRequest::new(source.root(), &output_root, request.describe);
    let output = package_output(&request.output_root, &source.package().id);
    let bases = components::base_components(&request.output_root, source, mode)?;
    let paths = build_inputs(source)?;
    let workspace = wamn_codegen::package_workspace(source.root());
    let (rustflags, toolchain) = if source.components().next().is_some() {
        components::guest_compiler(&workspace.join("..")).await?
    } else {
        (String::new(), String::new())
    };
    let key = || inputs::digest(source, &paths, &bases.bytes, mode, &toolchain);
    source.validate_snapshot()?;
    let before = key()?;
    if matches!(request.describe, DescribeMode::Cached)
        && let Ok(marker) = fingerprint::read(&output)
        && marker.input == before
    {
        let (bytes, manifest_digest) =
            fingerprint::read_artifact(&output, &marker, Path::new("wamn.json"))?;
        let manifest = PackageManifest::from_slice(&bytes)?;
        return Ok(BuiltPackage {
            package: manifest.package,
            manifest_digest,
        });
    }

    let version = if mode == BuildMode::Development {
        Some(development_version(&source.package().version, &before)?)
    } else {
        None
    };
    let development = version.as_ref().map(|version| (version, &bases.pins));
    let (mut manifest, mut outputs) = wamn_codegen::build::generate_prepared(
        database,
        &request,
        source,
        &bases.manifests,
        development,
    )
    .await?;
    if matches!(request.describe, DescribeMode::Check(_)) {
        let manifest_digest = wamn_digest::bytes_sha256(&wamn_digest::canonical_json_bytes(
            &serde_json::to_value(&manifest)?,
        ));
        return Ok(BuiltPackage {
            package: manifest.package,
            manifest_digest,
        });
    }
    // Refresh intentionally changes the committed evidence and can append history.
    let expected = if matches!(request.describe, DescribeMode::Refresh(_)) {
        key()?
    } else {
        before
    };
    if mode == BuildMode::Development && expected != before {
        // Refresh can append history and replace evidence. Publish the version
        // of those final inputs, using the same KCL document and updated snapshot.
        let version = development_version(&source.package().version, &expected)?;
        let cached = BuildRequest::new(source.root(), &request.output_root, DescribeMode::Cached);
        (manifest, outputs) = wamn_codegen::build::generate_prepared(
            database,
            &cached,
            source,
            &bases.manifests,
            Some((&version, &bases.pins)),
        )
        .await?;
    }
    outputs.extend(
        components::compile_components(source.root(), &output, &manifest, &bases.wasm, &rustflags)
            .await?,
    );
    anyhow::ensure!(
        key()? == expected,
        "package inputs changed during build; retry"
    );
    let manifest_digest = outputs[Path::new("wamn.json")];
    fingerprint::write(
        &output,
        &Fingerprint {
            input: expected,
            outputs,
        },
    )?;
    Ok(BuiltPackage {
        package: manifest.package,
        manifest_digest,
    })
}

/// Derive the development version from the complete build fingerprint.
pub fn development_version(
    authored: &wamn_package::package_version::PackageVersion,
    digest: &Digest,
) -> Result<wamn_package::package_version::PackageVersion> {
    let base = authored
        .as_str()
        .split(['-', '+'])
        .next()
        .context("authored package version")?;
    Ok(format!("{base}-dev.h{}", &digest.hex()[..12]).parse()?)
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
    use std::fmt::Write as _;

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
                write!(
                    manifest,
                    "[dependencies]\n{dependency}={{path='../{dependency}'}}\n"
                )?;
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
