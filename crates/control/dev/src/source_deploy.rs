//! Build and publish one application's source before the shared deployment path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wamn_build::{BuildRequest, build_package};

use anyhow::{Context as _, ensure};
use wamn_catalog::{ManifestDigest, PackageCoordinate};
use wamn_compiler::manifest::PackageManifest;
use wamn_compiler::{
    build::{default_output_root, package_output},
    build_record,
};
use wamn_environment::environment::{Platform, document::EnvironmentDocument};
use wamn_package_control::package_artifact::{PackageRegistry, PushPackageRequest, push_package};
use wamn_release_control::publish_release::{
    EntryPublicationInputs, PublishReleaseRequest, publish_release,
};

use crate::dev::config::read_package_manifest;

/// Build the selected closure and publish through the application release owner.
/// The build uses private databases. This operation never writes the environment file.
pub async fn publish(
    platform: &Platform,
    document: &EnvironmentDocument,
    root: &Path,
    database_url: &str,
) -> anyhow::Result<(String, ManifestDigest)> {
    let source = build(document, root, database_url).await?;
    let mut platform = platform.clone();
    wamn_environment::environment::runtime::resolve_platform(&mut platform, &document.org).await?;
    let registry = PackageRegistry {
        artifact_base: platform
            .package_artifact_base
            .as_ref()
            .context("set WAMN_PACKAGE_ARTIFACT_BASE for source deployment")?
            .to_owned(),
        credentials: None,
        registry_auth_file: platform.registry_auth_file.clone(),
        insecure_registry: platform.insecure_registry,
        oci_ca_paths: platform.oci_ca_paths.clone(),
    };
    let component_artifact_base = platform
        .component_artifact_base
        .as_deref()
        .context("set WAMN_COMPONENT_ARTIFACT_BASE for source deployment")?;
    publish_built(
        document,
        source,
        registry,
        component_artifact_base,
        &platform.release_artifact_base,
    )
    .await
}

/// Build and publish source without deployment or target-database credentials.
pub async fn publish_from_env(
    document: &EnvironmentDocument,
    root: &Path,
    database_url: &str,
) -> anyhow::Result<(String, ManifestDigest)> {
    let source = build(document, root, database_url).await?;
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
    publish_built(
        document,
        source,
        registry,
        &variable("WAMN_COMPONENT_ARTIFACT_BASE")?,
        &variable("WAMN_RELEASE_ARTIFACT_BASE")?,
    )
    .await
}

struct SourceBuild {
    application: String,
    roots: Vec<PathBuf>,
    packages: Vec<PackageCoordinate>,
}

async fn build(
    document: &EnvironmentDocument,
    root: &Path,
    database_url: &str,
) -> anyhow::Result<SourceBuild> {
    let (application, roots) = sources(document, root)?;
    let mut packages = Vec::with_capacity(roots.len());
    for root in &roots {
        let record = build_package(&BuildRequest::new(
            root,
            &default_output_root(root),
            database_url,
        ))
        .await?;
        packages.push(PackageCoordinate::new(
            record.package.id,
            record.package.version,
        )?);
    }
    Ok(SourceBuild {
        application,
        roots,
        packages,
    })
}

async fn publish_built(
    document: &EnvironmentDocument,
    source: SourceBuild,
    registry: PackageRegistry,
    component_artifact_base: &str,
    release_artifact_base: &str,
) -> anyhow::Result<(String, ManifestDigest)> {
    let SourceBuild {
        application,
        roots,
        packages,
    } = source;
    let mut publication = PushPackageRequest {
        package: PathBuf::new(),
        registry,
        component_artifact_base: component_artifact_base.to_owned(),
    };
    for root in roots {
        publication.package = root;
        push_package(&publication).await?;
    }
    let route_guest = std::env::var_os("WAMN_FLOW_HTTP_COMPONENT").map_or_else(
        || PathBuf::from("/usr/local/share/wamn/http_route.wasm"),
        PathBuf::from,
    );
    let selection = document
        .applications
        .get(&application)
        .expect("source resolution selected an existing application");
    let digest = publish_release(
        PublishReleaseRequest {
            applications: [(
                application.clone(),
                wamn_catalog::ApplicationDefinition {
                    packages: packages
                        .iter()
                        .map(|package| package.package_id().to_owned())
                        .collect(),
                    connections: selection.connections.clone(),
                },
            )]
            .into(),
            application: None,
            org: document.org.clone(),
            project: document.project.clone(),
            environment: document.env.clone(),
            packages,
        },
        &publication.registry,
        release_artifact_base,
        Some(&EntryPublicationInputs {
            component_artifact_base,
            route_guest: &route_guest,
        }),
    )
    .await?;
    Ok((application, digest))
}

/// Resolve only the source package's declared application and dependency order.
pub fn sources(
    document: &EnvironmentDocument,
    root: &Path,
) -> anyhow::Result<(String, Vec<PathBuf>)> {
    let root = root
        .canonicalize()
        .context("resolve selected package root")?;
    let manifest = read_package_manifest(&root)?;
    ensure!(
        document.applications.len() == 1,
        "select one application before publishing source"
    );
    let application = document
        .applications
        .keys()
        .next()
        .expect("one application");
    let output = default_output_root(&root);
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
        let root = build_record::source_root(&package_output(&output, &id))?;
        let manifest = read_package_manifest(&root)?;
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
    Ok((application.to_owned(), roots))
}

fn visit<'a>(
    id: &'a str,
    manifests: &'a BTreeMap<String, (PathBuf, PackageManifest)>,
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
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
        let manifest = wamn_package_control::test_support::source_manifest();
        std::fs::write(
            scratch.path().join("generated/wamn.json"),
            serde_json::to_vec(&manifest)?,
        )?;
        let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
            "org":"acme","project":"source","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,
            "applications":{"selected":{"revocation_seconds":5,"hostname":"selected.example"}}
        }))?)?;
        let (application, roots) = sources(&document, scratch.path())?;
        assert!(document.applications.contains_key(&application));
        assert_eq!(roots, [scratch.path().canonicalize()?]);
        document.applications.insert(
            "other".to_owned(),
            document.applications["selected"].clone(),
        );
        assert!(sources(&document, scratch.path()).is_err());
        Ok(())
    }
}
