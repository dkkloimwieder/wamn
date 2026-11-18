//! Build and publish one application's source before the shared deployment path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wamn_build::{BuildMode, BuildRequest, DescribeMode, PreparedSource, build_prepared};
use wamn_catalog::PackageName;
use wamn_catalog::manifest::{
    BaseDependencyRequirement, PackageIdentity, PackageManifest, source_name::SourceName,
};
use wamn_codegen::{
    build::{default_output_root, package_output},
    fingerprint,
};
use wamn_environment::environment::document::EnvironmentDocument;
use wamn_publish::package_artifact::{PackageRegistry, PushPackageRequest, push_package};

use anyhow::{Context as _, ensure};

/// Build and publish source using its committed database evidence.
pub async fn publish_from_env(
    document: &EnvironmentDocument,
    root: &Path,
    describe: DescribeMode<'_>,
) -> anyhow::Result<(wamn_catalog::ApplicationName, wamn_digest::Digest)> {
    let (application, sources) = sources(document, root).await?;
    build_and_publish(application, sources, BuildMode::Published, describe).await
}

/// Build and publish the source documents already compiled for this dev pass.
pub async fn publish_development(
    application: wamn_catalog::ApplicationName,
    sources: Vec<PreparedSource>,
    describe: DescribeMode<'_>,
) -> anyhow::Result<(wamn_catalog::ApplicationName, wamn_digest::Digest)> {
    build_and_publish(application, sources, BuildMode::Development, describe).await
}

async fn build_and_publish(
    application: wamn_catalog::ApplicationName,
    sources: Vec<PreparedSource>,
    mode: BuildMode,
    describe: DescribeMode<'_>,
) -> anyhow::Result<(wamn_catalog::ApplicationName, wamn_digest::Digest)> {
    let mut roots = Vec::with_capacity(sources.len());
    for source in sources {
        let root = default_output_root(source.root());
        let request = BuildRequest::new(source.root(), &root, describe);
        let built = build_prepared(&request, &source, mode).await?;
        roots.push(package_output(&root, &built.package.id));
    }
    let digest = publish_source_from_env(roots).await?;
    Ok((application, digest))
}

/// Publish the selected package's existing build output without rebuilding.
pub async fn publish_built_from_env(root: &Path) -> anyhow::Result<wamn_digest::Digest> {
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
    publish_source_from_env(roots).await
}

async fn publish_source_from_env(roots: Vec<PathBuf>) -> anyhow::Result<wamn_digest::Digest> {
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
    publish_built(roots, registry, &variable("WAMN_COMPONENT_ARTIFACT_BASE")?).await
}

async fn publish_built(
    mut roots: Vec<PathBuf>,
    registry: PackageRegistry,
    component_artifact_base: &str,
) -> anyhow::Result<wamn_digest::Digest> {
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
    Ok(push_package(&publication, &bases).await?.digest)
}

/// Resolve and compile the source closure in dependency order.
pub async fn sources(
    document: &EnvironmentDocument,
    root: &Path,
) -> anyhow::Result<(wamn_catalog::ApplicationName, Vec<PreparedSource>)> {
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
