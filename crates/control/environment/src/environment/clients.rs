//! Resolve the environment's web client before writing its release chart.

pub(super) mod plan;
pub(super) use plan::resolve;
use plan::verify;

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};

use super::Platform;
use super::analyse::ReleaseFacts;
use super::document::EnvironmentDocument;
use wamn_package_control::package_artifact::PackageRegistry;
use wamn_package_control::web_artifact::{WebManifest, pull_web_artifact};

pub(super) async fn analyse(
    platform: &Platform,
    document: &EnvironmentDocument,
    release: Option<&ReleaseFacts>,
) -> anyhow::Result<()> {
    let Some(client) = document.legacy_client() else {
        return Ok(());
    };
    let registry = PackageRegistry {
        artifact_base: platform
            .package_artifact_base
            .clone()
            .context("set WAMN_PACKAGE_ARTIFACT_BASE for the pinned web artifact")?,
        registry_auth_file: platform.registry_auth_file.clone(),
        insecure_registry: false,
        oci_ca_paths: platform.oci_ca_paths.clone(),
    };
    let web = pull_web_artifact(&registry, &client.web).await?;
    verify(
        document,
        &web,
        release.into_iter().flat_map(|release| {
            release
                .manifest
                .release
                .packages
                .iter()
                .map(|package| (package.package_id(), package.package_version()))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> EnvironmentDocument {
        serde_json::from_value(serde_json::json!({
            "org": "test", "project": "wms", "env": "dev",
            "target": "cloud", "policy": "dev",
            "applications": {"fixture": {"current": "none", "hostname": "wms.example.com", "packages": ["fixture"]}}}))
        .unwrap()
    }

    #[test]
    fn no_pin_serves_no_client() {
        for packages in [vec![], vec!["wms"], vec!["wms", "receiving"]] {
            assert!(
                resolve(&document(), packages.into_iter())
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn a_named_client_selects_its_package() {
        let mut document = document();
        document.applications.get_mut("fixture").unwrap().client =
            Some(super::super::document::Client {
                package: "receiving".into(),
                web: wamn_execution_contract::bytes_sha256(b"web artifact"),
            });
        assert_eq!(
            resolve(&document, ["wms", "receiving"].into_iter()).unwrap()["wms.example.com"],
            "receiving"
        );
        assert!(resolve(&document, ["wms"].into_iter()).is_err());
    }

    #[test]
    fn a_web_pin_requires_its_exact_package_version_in_the_release() {
        let web: WebManifest =
            serde_json::from_str(include_str!("../../tests/support/web-artifact/web.json"))
                .unwrap();
        let mut document = document();
        // DEP2-03 replaces the shared candidate with application publication.
        for application in document.applications.values_mut() {
            application.current = super::super::document::DeclaredRelease::Digest(
                wamn_execution_contract::bytes_sha256(b"release"),
            );
        }
        document.applications.get_mut("fixture").unwrap().client =
            Some(super::super::document::Client {
                package: "web_fixture".into(),
                web: wamn_execution_contract::bytes_sha256(b"web artifact"),
            });
        assert!(verify(&document, &web, [("web_fixture", "2.0.0")].into_iter()).is_err());
        verify(&document, &web, [("web_fixture", "1.0.0")].into_iter()).unwrap();
        document
            .applications
            .get_mut("fixture")
            .unwrap()
            .client
            .as_mut()
            .unwrap()
            .package = "receiving".into();
        assert!(verify(&document, &web, [("web_fixture", "1.0.0")].into_iter()).is_err());
        document
            .applications
            .get_mut("fixture")
            .unwrap()
            .client
            .as_mut()
            .unwrap()
            .package = "web_fixture".into();
        // DEP2-03 replaces the shared candidate with application publication.
        for application in document.applications.values_mut() {
            application.current = super::super::document::DeclaredRelease::None;
        }
        assert!(verify(&document, &web, [].into_iter()).is_err());
    }
}
