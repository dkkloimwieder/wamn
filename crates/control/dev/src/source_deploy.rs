//! Build and publish one application's source before the shared deployment path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use wamn_catalog::{ManifestDigest, PackageCoordinate, manifest::PackageManifest};
use wamn_environment_control::environment::{Platform, document::EnvironmentDocument};
use wamn_package_control::package_artifact::{PackageRegistry, PushPackageRequest, push_package};
use wamn_release_control::publish_release::{
    EntryPublicationInputs, PublishReleaseRequest, publish_release,
};
use wamn_schema_generator::{
    build::{BuildRequest, build_package, default_output_root, package_output},
    build_record,
};

use crate::dev::config::read_package_manifest;

/// Build the selected closure and publish through the application release owner.
/// The build uses private databases. This operation never writes the environment file.
pub async fn publish(
    platform: &Platform,
    mut document: EnvironmentDocument,
    root: &Path,
    principal: String,
    database_url: &str,
) -> anyhow::Result<ManifestDigest> {
    ensure!(!principal.is_empty(), "publisher principal is empty");
    let (application, roots) = sources(&document, root)?;
    let mut platform = platform.clone();
    wamn_environment_control::environment::runtime::resolve_platform(&mut platform, &document.org)
        .await?;
    let registry = PackageRegistry {
        artifact_base: platform
            .package_artifact_base
            .as_ref()
            .context("set WAMN_PACKAGE_ARTIFACT_BASE for source deployment")?
            .to_owned(),
        registry_auth_file: platform.registry_auth_file.clone(),
        insecure_registry: platform.insecure_registry,
        oci_ca_paths: platform.oci_ca_paths.clone(),
    };
    let component_artifact_base = platform
        .component_artifact_base
        .as_deref()
        .context("set WAMN_COMPONENT_ARTIFACT_BASE for source deployment")?;
    let mut packages = Vec::with_capacity(roots.len());
    for root in &roots {
        // build_package owns current_build/current_generation and builds only stale inputs.
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
        .remove(&application)
        .expect("source resolution selected an existing application");
    publish_release(
        PublishReleaseRequest {
            applications: [(application, selection.into())].into(),
            application: None,
            org: document.org,
            project: document.project,
            environment: document.env,
            verified_publisher_principal: principal,
            packages,
        },
        &publication.registry,
        &platform.release_artifact_base,
        Some(&EntryPublicationInputs {
            component_artifact_base,
            route_guest: &route_guest,
        }),
    )
    .await
}

fn sources(document: &EnvironmentDocument, root: &Path) -> anyhow::Result<(String, Vec<PathBuf>)> {
    let root = root
        .canonicalize()
        .context("resolve selected package root")?;
    let manifest = read_package_manifest(&root)?;
    let mut owners = document
        .applications
        .iter()
        .filter(|(_, selection)| selection.packages.contains(&manifest.package.id));
    let (application, selection) = owners
        .next()
        .context("source package has no declared application")?;
    ensure!(
        owners.next().is_none(),
        "source package belongs to multiple applications"
    );
    let output = default_output_root(&root);
    let mut manifests = BTreeMap::new();
    manifests.insert(manifest.package.id.clone(), (root, manifest));
    for id in &selection.packages {
        if !manifests.contains_key(id) {
            let root = build_record::source_root(&package_output(&output, id))?;
            let manifest = read_package_manifest(&root)?;
            ensure!(
                &manifest.package.id == id,
                "source directory does not declare package {id}"
            );
            manifests.insert(id.clone(), (root, manifest));
        }
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
    fn source_membership_orders_dependencies_without_reading_other_applications()
    -> anyhow::Result<()> {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        let mut expected = Vec::new();
        for (id, dependency) in [
            ("base", None),
            ("middle", Some("base")),
            ("overlay", Some("middle")),
        ] {
            let root = scratch.path().join(id);
            std::fs::create_dir_all(&root)?;
            let mut manifest = json!({
                "package": {"id": id, "version": "1.0.0"},
                "required_platform_policy_contract": {"id": "fixture", "state": "satisfied"},
                "models": {}, "components": {}, "connections": {}, "base_dependencies": {}
            });
            if let Some(dependency) = dependency {
                manifest["base_dependencies"][dependency] = json!({
                    "package": dependency, "version": "1.0.0",
                    "digest": format!("sha256:{}", "a".repeat(64)), "operations": []
                });
            }
            std::fs::write(root.join("wamn.json"), serde_json::to_vec(&manifest)?)?;
            expected.push(root);
        }
        let members = expected
            .iter()
            .map(|root| root.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>();
        let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
            "org": "acme", "project": "source", "env": "dev", "target": "cloud", "policy": "dev",
            "applications": {
                "selected": {"current": "none", "hostname": "selected.invalid", "packages": members},
                "unavailable": {"current": "none", "hostname": "unavailable.invalid", "packages": ["missing"]}
            }
        }))?)?;
        let (name, roots) = sources(&document, &scratch.path().join("overlay"))?;
        assert_eq!(name, "selected");
        assert_eq!(roots, expected);
        document
            .applications
            .get_mut("selected")
            .unwrap()
            .packages
            .remove("base");
        assert!(sources(&document, &scratch.path().join("overlay")).is_err());
        document
            .applications
            .get_mut("unavailable")
            .unwrap()
            .packages
            .insert("overlay".into());
        assert!(sources(&document, &scratch.path().join("overlay")).is_err());
        Ok(())
    }
}
