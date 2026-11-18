//! Build and publish one application's source before the shared deployment path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wamn_build::{BuildRequest, build_package};

use anyhow::{Context as _, ensure};
use wamn_catalog::manifest::PackageManifest;
use wamn_codegen::{
    build::{default_output_root, package_output},
    build_record,
};
use wamn_environment::environment::document::EnvironmentDocument;
use wamn_publish::package_artifact::{PackageRegistry, PushPackageRequest, push_package};

fn read_package_manifest(root: &Path) -> anyhow::Result<PackageManifest> {
    let bytes = if wamn_codegen::is_authored(root) {
        wamn_codegen::compile_manifest(root)?
    } else {
        std::fs::read(wamn_codegen::package_manifest_path(root))?
    };
    Ok(PackageManifest::from_slice(&bytes)?)
}

/// Build and publish source without deployment or target-database credentials.
pub async fn publish_from_env(
    document: &EnvironmentDocument,
    root: &Path,
    database_url: &str,
) -> anyhow::Result<(wamn_catalog::ApplicationName, wamn_digest::Digest)> {
    publish_source_from_env(build(document, root, database_url, false).await?).await
}

/// Build and publish immutable development coordinates derived from source content.
pub async fn publish_development_from_env(
    document: &EnvironmentDocument,
    root: &Path,
    database_url: &str,
) -> anyhow::Result<(wamn_catalog::ApplicationName, wamn_digest::Digest)> {
    publish_source_from_env(build(document, root, database_url, true).await?).await
}

/// Publish the selected source closure's existing build output without rebuilding.
/// Package publication verifies the build records before it pushes artifacts.
pub async fn publish_built_from_env(
    document: &EnvironmentDocument,
    root: &Path,
) -> anyhow::Result<(wamn_catalog::ApplicationName, wamn_digest::Digest)> {
    let root = root
        .canonicalize()
        .context("resolve selected package root")?;
    let output = wamn_codegen::output_root(&root);
    let output_parent = output.parent().context("build directory has no parent")?;
    let roots = ordered_sources(
        root,
        |root| {
            let path = wamn_codegen::package_manifest_path(root);
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            Ok(PackageManifest::from_slice(&bytes)?)
        },
        |id| Ok(package_output(output_parent, id)),
    )?;
    publish_source_from_env(SourceBuild {
        application: selected_application(document)?.clone(),
        roots,
    })
    .await
}

async fn publish_source_from_env(
    source: SourceBuild,
) -> anyhow::Result<(wamn_catalog::ApplicationName, wamn_digest::Digest)> {
    let variable = |name: &str| {
        std::env::var(name).with_context(|| format!("set {name} for source publication"))
    };
    let artifact_base = variable("WAMN_PACKAGE_ARTIFACT_BASE")?;
    let insecure_registry = url::Url::parse(&format!("http://{artifact_base}"))?
        .host_str()
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]"));
    let registry = PackageRegistry {
        artifact_base,
        credentials: None,
        registry_auth_file: variable("WAMN_REGISTRY_AUTH_FILE")?.into(),
        insecure_registry,
        oci_ca_paths: std::env::var("WAMN_OCI_CA_PATHS")
            .unwrap_or_default()
            .split(':')
            .filter(|path| !path.is_empty())
            .map(Into::into)
            .collect(),
    };
    publish_built(source, registry, &variable("WAMN_COMPONENT_ARTIFACT_BASE")?).await
}

struct SourceBuild {
    application: wamn_catalog::ApplicationName,
    roots: Vec<PathBuf>,
}

async fn build(
    document: &EnvironmentDocument,
    root: &Path,
    database_url: &str,
    development: bool,
) -> anyhow::Result<SourceBuild> {
    let (application, roots) = sources(document, root)?;
    for root in &roots {
        let mut request = BuildRequest::new(root, &default_output_root(root), database_url);
        let digest = development.then(|| content_digest(root)).transpose()?;
        if let Some(digest) = &digest {
            request.development_version = Some(wamn_publish::development::version(
                &read_package_manifest(root)?.package.version,
                digest,
            )?);
        }
        build_package(&request).await?;
        if let Some(digest) = digest {
            ensure!(
                digest == content_digest(root)?,
                "source changed during development build; retry"
            );
        }
    }
    Ok(SourceBuild { application, roots })
}

/// Source directories and exact configuration inputs consumed by a package build.
/// The digest and development watcher use this same dependency closure.
pub fn build_inputs(root: &Path) -> anyhow::Result<BTreeSet<PathBuf>> {
    let workspace = wamn_codegen::package_workspace(root);
    let repository = workspace.parent().context("package workspace parent")?;
    let mut inputs = [
        root.to_owned(),
        repository.join("guests"),
        repository.join("wit"),
        repository.join("crates/authority/wit/deps"),
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
        repository.join("Cargo.toml"),
        repository.join("Cargo.lock"),
        repository.join("rust-toolchain.toml"),
        repository.join(".cargo"),
        workspace.join(".cargo"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect::<BTreeSet<_>>();
    if workspace.join("Cargo.toml").is_file() {
        let manifest = read_package_manifest(root)?;
        let packages = manifest
            .components
            .keys()
            .map(|name| name.as_str().replace('_', "-"))
            .collect::<Vec<_>>();
        inputs.extend(wamn_build::source_dependencies(
            &workspace.join("Cargo.toml"),
            &packages,
        )?);
    }
    if repository.join("Cargo.toml").is_file() {
        inputs.extend(wamn_build::source_dependencies(
            &repository.join("Cargo.toml"),
            &["wamn-build".into()],
        )?);
    }
    Ok(inputs)
}

fn content_digest(root: &Path) -> anyhow::Result<wamn_digest::Digest> {
    let workspace = wamn_codegen::package_workspace(root);
    let repository = workspace
        .parent()
        .context("package workspace has no repository parent")?;
    let mut files = BTreeMap::new();
    for input in build_inputs(root)? {
        hash_inputs(repository, &input, &mut files)?;
    }
    let manifest = read_package_manifest(root)?;
    for dependency in manifest.base_dependencies.values() {
        let built = build_record::read(&package_output(
            &default_output_root(root),
            &dependency.package,
        ))?;
        files.insert(
            format!("base/{}", dependency.package),
            wamn_digest::bytes_sha256(&built.to_bytes()),
        );
    }
    Ok(wamn_digest::bytes_sha256(
        &wamn_digest::canonical_json_bytes(&serde_json::to_value(files)?),
    ))
}

fn hash_inputs(
    repository: &Path,
    path: &Path,
    files: &mut BTreeMap<String, wamn_digest::Digest>,
) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() || metadata.is_file(),
        "development build input must be a regular file: {}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if !matches!(
                entry.file_name().to_str(),
                Some("target" | "generated" | "node_modules" | "dist" | ".git")
            ) {
                hash_inputs(repository, &entry.path(), files)?;
            }
        }
    } else if metadata.is_file() {
        let name = path
            .strip_prefix(repository)?
            .to_str()
            .context("build input path must be UTF-8")?
            .to_owned();
        files.insert(name, wamn_digest::bytes_sha256(&std::fs::read(path)?));
    }
    Ok(())
}

async fn publish_built(
    source: SourceBuild,
    registry: PackageRegistry,
    component_artifact_base: &str,
) -> anyhow::Result<(wamn_catalog::ApplicationName, wamn_digest::Digest)> {
    let SourceBuild {
        application,
        mut roots,
    } = source;
    // A dependency-first walk visits the selected source package last.
    let top = roots.pop().context("source package closure is empty")?;
    let mut publication = PushPackageRequest {
        package: PathBuf::new(),
        registry,
        component_artifact_base: component_artifact_base.to_owned(),
    };
    let mut bases = BTreeMap::new();
    for root in roots {
        publication.package = root;
        let published = push_package(&publication, &bases).await?;
        bases.insert(published.coordinate.clone(), published);
    }
    publication.package = top;
    let digest = push_package(&publication, &bases).await?.digest;
    Ok((application, digest))
}

/// Resolve only the source package's declared application and dependency order.
pub fn sources(
    document: &EnvironmentDocument,
    root: &Path,
) -> anyhow::Result<(wamn_catalog::ApplicationName, Vec<PathBuf>)> {
    let root = root
        .canonicalize()
        .context("resolve selected package root")?;
    let output = default_output_root(&root);
    let application = selected_application(document)?.clone();
    let roots = ordered_sources(root, read_package_manifest, |id| {
        build_record::source_root(&package_output(&output, id))
    })?;
    Ok((application, roots))
}

fn selected_application(
    document: &EnvironmentDocument,
) -> anyhow::Result<&wamn_catalog::ApplicationName> {
    ensure!(
        document.applications.len() == 1,
        "select one application before publishing source"
    );
    Ok(document
        .applications
        .keys()
        .next()
        .expect("one application"))
}

fn ordered_sources(
    root: PathBuf,
    read: impl Fn(&Path) -> anyhow::Result<PackageManifest>,
    base_root: impl Fn(&wamn_catalog::PackageName) -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Vec<PathBuf>> {
    let manifest = read(&root)?;
    let mut manifests = BTreeMap::new();
    manifests.insert(manifest.package.id.clone(), (root, manifest));
    loop {
        let missing = manifests
            .values()
            .flat_map(|(_, manifest)| manifest.base_dependencies.values())
            .find(|dependency| !manifests.contains_key(&dependency.package))
            .map(|dependency| dependency.package.clone());
        let Some(id) = missing else {
            break;
        };
        let root = base_root(&id)?;
        let manifest = read(&root)?;
        ensure!(
            manifest.package.id == id,
            "source directory does not declare package {id}"
        );
        manifests.insert(id, (root, manifest));
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut roots = Vec::with_capacity(manifests.len());
    for id in manifests.keys() {
        visit(id, &manifests, &mut visiting, &mut visited, &mut roots)?;
    }
    Ok(roots)
}

fn visit<'a>(
    id: &'a wamn_catalog::PackageName,
    manifests: &'a BTreeMap<wamn_catalog::PackageName, (PathBuf, PackageManifest)>,
    visiting: &mut BTreeSet<&'a wamn_catalog::PackageName>,
    visited: &mut BTreeSet<&'a wamn_catalog::PackageName>,
    roots: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    if visited.contains(id) {
        return Ok(());
    }
    ensure!(visiting.insert(id), "cyclic source dependency at {id}");
    let (root, manifest) = &manifests[id];
    for dependency in manifest.base_dependencies.values() {
        let (_, base) = manifests.get(&dependency.package).with_context(|| {
            format!(
                "application does not declare dependency {}",
                dependency.package
            )
        })?;
        ensure!(
            base.package.version == dependency.version,
            "source version differs from dependency {}@{}",
            dependency.package,
            dependency.version
        );
        visit(&dependency.package, manifests, visiting, visited, roots)?;
    }
    visiting.remove(id);
    visited.insert(id);
    roots.push(root.to_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selected_application_publishes_the_source_closure_without_document_membership()
    -> anyhow::Result<()> {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        std::fs::create_dir_all(scratch.path().join("generated"))?;
        let manifest = wamn_publish::test_support::source_manifest();
        std::fs::write(
            scratch.path().join("generated/wamn.json"),
            serde_json::to_vec(&manifest)?,
        )?;
        let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
            "org":"acme","project":"source","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,
            "applications":{"selected":{"hostname":"selected.example"}}
        }))?)?;
        let (application, roots) = sources(&document, scratch.path())?;
        assert!(document.applications.contains_key(&application));
        assert_eq!(roots, [scratch.path().canonicalize()?]);
        document.applications.insert(
            "other".parse().unwrap(),
            document.applications["selected"].clone(),
        );
        assert!(sources(&document, scratch.path()).is_err());
        Ok(())
    }
}
