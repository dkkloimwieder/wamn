//! Build and publish one application's source before the shared deployment path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wamn_codegen::fingerprint;
use wamn_component_policy::PackageManifest;
use wamn_env::env::document::EnvDocument;
use wamn_package::PackageName;
use wamn_package::manifest::BaseDependencyRequirement;
use wamn_package::manifest::PackageIdentity;
use wamn_package::manifest::source_name::SourceName;
use wamn_release::release_package::RegistryPackagePublisher;

use anyhow::{Context as _, ensure};

use crate::source_build::SourceBuilder;
use crate::{BuildMode, PreparedSource, default_output_root, package_output, prepare_source};

/// Build and publish source using its committed database evidence.
pub async fn publish_from_env(
    build: &SourceBuilder<'_>,
    publisher: &RegistryPackagePublisher<'_>,
    document: &EnvDocument,
    root: &Path,
) -> anyhow::Result<(wamn_registry::AppName, wamn_digest::Digest)> {
    let (app, sources) = sources(document, root).await?;
    build_and_publish(build, publisher, app, sources, BuildMode::Published).await
}

/// Build and publish the source documents already compiled for this dev pass.
#[cfg(target_os = "linux")]
pub(crate) async fn publish_development(
    build: &SourceBuilder<'_>,
    publisher: &RegistryPackagePublisher<'_>,
    app: wamn_registry::AppName,
    sources: Vec<PreparedSource>,
) -> anyhow::Result<(wamn_registry::AppName, wamn_digest::Digest)> {
    build_and_publish(build, publisher, app, sources, BuildMode::Development).await
}

async fn build_and_publish(
    build: &SourceBuilder<'_>,
    publisher: &RegistryPackagePublisher<'_>,
    app: wamn_registry::AppName,
    sources: Vec<PreparedSource>,
    mode: BuildMode,
) -> anyhow::Result<(wamn_registry::AppName, wamn_digest::Digest)> {
    let mut roots = Vec::with_capacity(sources.len());
    for source in sources {
        roots.push(build.build(&source, mode).await?);
    }
    let digest = publisher.publish(roots).await?;
    Ok((app, digest))
}

/// Publish the selected package's existing build output without rebuilding.
pub async fn publish_built_from_env(
    publisher: &RegistryPackagePublisher<'_>,
    root: &Path,
) -> anyhow::Result<wamn_digest::Digest> {
    let root = wamn_codegen::output_root(&root.canonicalize()?);
    let parent = root
        .parent()
        .context("build directory has no parent")?
        .to_owned();
    let read = |root: PathBuf| async move {
        let marker = fingerprint::read(&root)?;
        let (bytes, _) = fingerprint::read_artifact(&root, &marker, Path::new("wamn.json"))?;
        let manifest = PackageManifest::from_slice(&bytes)?;
        Ok(BuiltSource { root, manifest })
    };
    let roots = ordered_sources(read(root).await?, read, |id| {
        Ok(package_output(&parent, id))
    })
    .await?
    .into_iter()
    .map(|source| source.root)
    .collect();
    publisher.publish(roots).await
}

/// Resolve and compile the source closure in dependency order.
pub(crate) async fn sources(
    document: &EnvDocument,
    root: &Path,
) -> anyhow::Result<(wamn_registry::AppName, Vec<PreparedSource>)> {
    let root = root
        .canonicalize()
        .context("resolve selected package root")?;
    let app = selected_app(document)?.clone();
    let sources = ordered_sources(
        prepare_source(&root).await?,
        |root| async move { prepare_source(&root).await },
        |id| wamn_codegen::source_root(&package_output(&default_output_root(&root), id)),
    )
    .await?;
    Ok((app, sources))
}

fn selected_app(document: &EnvDocument) -> anyhow::Result<&wamn_registry::AppName> {
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

/// The coordinate and authored base dependencies used to order a source closure.
pub trait PackageSource {
    fn package(&self) -> &PackageIdentity;
    fn bases(&self) -> &BTreeMap<SourceName, BaseDependencyRequirement>;
}

impl PackageSource for PreparedSource {
    fn package(&self) -> &PackageIdentity {
        self.package()
    }

    fn bases(&self) -> &BTreeMap<SourceName, BaseDependencyRequirement> {
        self.bases()
    }
}

struct BuiltSource {
    root: PathBuf,
    manifest: PackageManifest,
}

impl PackageSource for BuiltSource {
    fn package(&self) -> &PackageIdentity {
        &self.manifest.package
    }

    fn bases(&self) -> &BTreeMap<SourceName, BaseDependencyRequirement> {
        &self.manifest.base_dependencies
    }
}

async fn ordered_sources<T: PackageSource, F: Future<Output = anyhow::Result<T>>>(
    source: T,
    read: impl Fn(PathBuf) -> F,
    base_root: impl Fn(&PackageName) -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Vec<T>> {
    let mut sources = BTreeMap::from([(source.package().id.clone(), source)]);
    loop {
        let missing = sources
            .values()
            .flat_map(|source| source.bases().values())
            .find(|dependency| !sources.contains_key(&dependency.package))
            .map(|dependency| dependency.package.clone());
        let Some(id) = missing else { break };
        let source = read(base_root(&id)?).await?;
        ensure!(
            source.package().id == id,
            "source directory does not declare package {id}"
        );
        sources.insert(id, source);
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut order = Vec::with_capacity(sources.len());
    for id in sources.keys() {
        visit(id, &sources, &mut visiting, &mut visited, &mut order)?;
    }
    let order = order.into_iter().cloned().collect::<Vec<_>>();
    order
        .into_iter()
        .map(|id| sources.remove(&id).context("package dependency order"))
        .collect()
}

fn visit<'a, T: PackageSource>(
    id: &'a PackageName,
    sources: &'a BTreeMap<PackageName, T>,
    visiting: &mut BTreeSet<&'a PackageName>,
    visited: &mut BTreeSet<&'a PackageName>,
    order: &mut Vec<&'a PackageName>,
) -> anyhow::Result<()> {
    if visited.contains(id) {
        return Ok(());
    }
    ensure!(visiting.insert(id), "cyclic source dependency at {id}");
    for dependency in sources[id].bases().values() {
        let base = sources
            .get(&dependency.package)
            .context("missing base source")?;
        ensure!(
            base.package().version == dependency.version,
            "source version differs from dependency {}@{}",
            dependency.package,
            dependency.version
        );
        visit(&dependency.package, sources, visiting, visited, order)?;
    }
    visiting.remove(id);
    visited.insert(id);
    order.push(id);
    Ok(())
}
