//! Build and publish one application's source before the shared deployment path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wamn_build::{
    BuildDatabase, BuildMode, BuildRequest, DescribeMode, PreparedSource, build_prepared,
};
use wamn_codegen::{
    build::{default_output_root, package_output},
    fingerprint,
};
use wamn_component_policy::PackageManifest;
use wamn_environment::environment::document::EnvironmentDocument;
use wamn_package::PackageName;
use wamn_package::manifest::BaseDependencyRequirement;
use wamn_package::manifest::PackageIdentity;
use wamn_package::manifest::source_name::SourceName;
use wamn_release::package_artifact::PackagePublisher;

use anyhow::{Context as _, ensure};

/// Build and publish source using its committed database evidence.
pub async fn publish_from_env(
    database: &dyn BuildDatabase,
    publisher: &dyn PackagePublisher,
    document: &EnvironmentDocument,
    root: &Path,
    describe: DescribeMode<'_>,
) -> anyhow::Result<(wamn_registry::ApplicationName, wamn_digest::Digest)> {
    let (application, sources) = sources(document, root).await?;
    build_and_publish(
        database,
        publisher,
        application,
        sources,
        BuildMode::Published,
        describe,
    )
    .await
}

/// Build and publish the source documents already compiled for this dev pass.
pub async fn publish_development(
    database: &dyn BuildDatabase,
    publisher: &dyn PackagePublisher,
    application: wamn_registry::ApplicationName,
    sources: Vec<PreparedSource>,
    describe: DescribeMode<'_>,
) -> anyhow::Result<(wamn_registry::ApplicationName, wamn_digest::Digest)> {
    build_and_publish(
        database,
        publisher,
        application,
        sources,
        BuildMode::Development,
        describe,
    )
    .await
}

async fn build_and_publish(
    database: &dyn BuildDatabase,
    publisher: &dyn PackagePublisher,
    application: wamn_registry::ApplicationName,
    sources: Vec<PreparedSource>,
    mode: BuildMode,
    describe: DescribeMode<'_>,
) -> anyhow::Result<(wamn_registry::ApplicationName, wamn_digest::Digest)> {
    let mut roots = Vec::with_capacity(sources.len());
    for source in sources {
        let root = default_output_root(source.root());
        let request = BuildRequest::new(source.root(), &root, describe);
        let built = build_prepared(database, &request, &source, mode).await?;
        roots.push(package_output(&root, &built.package.id));
    }
    let digest = publisher.publish(roots).await?;
    Ok((application, digest))
}

/// Publish the selected package's existing build output without rebuilding.
pub async fn publish_built_from_env(
    publisher: &dyn PackagePublisher,
    root: &Path,
) -> anyhow::Result<wamn_digest::Digest> {
    let root = wamn_codegen::output_root(&root.canonicalize()?);
    let parent = root
        .parent()
        .context("build directory has no parent")?
        .to_owned();
    let roots = ordered_sources(
        root,
        async |root| {
            let marker = fingerprint::read(&root)?;
            let (bytes, _) = fingerprint::read_artifact(&root, &marker, Path::new("wamn.json"))?;
            Ok(BuiltSource {
                root,
                manifest: PackageManifest::from_slice(&bytes)?,
            })
        },
        |id| Ok(package_output(&parent, id)),
    )
    .await?
    .into_iter()
    .map(|source| source.root)
    .collect();
    publisher.publish(roots).await
}

/// Resolve and compile the source closure in dependency order.
pub async fn sources(
    document: &EnvironmentDocument,
    root: &Path,
) -> anyhow::Result<(wamn_registry::ApplicationName, Vec<PreparedSource>)> {
    let root = root
        .canonicalize()
        .context("resolve selected package root")?;
    let output = default_output_root(&root);
    let application = selected_application(document)?.clone();
    let sources = ordered_sources(
        root,
        async |root| wamn_build::prepare_source(&root).await,
        |id| wamn_codegen::source_root(&package_output(&output, id)),
    )
    .await?;
    Ok((application, sources))
}

fn selected_application(
    document: &EnvironmentDocument,
) -> anyhow::Result<&wamn_registry::ApplicationName> {
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

trait PackageSource {
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

async fn ordered_sources<T: PackageSource>(
    root: PathBuf,
    read: impl AsyncFn(PathBuf) -> anyhow::Result<T>,
    base_root: impl Fn(&PackageName) -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Vec<T>> {
    let source = read(root).await?;
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
