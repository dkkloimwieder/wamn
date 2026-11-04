//! Resolve the environment's web client before writing its release chart.

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
    let Some(client) = &document.client else {
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

fn verify<'a>(
    document: &EnvironmentDocument,
    web: &WebManifest,
    packages: impl Iterator<Item = (&'a str, &'a str)>,
) -> anyhow::Result<()> {
    let client = document.client.as_ref().expect("a pinned client");
    let (package, version) = web
        .package
        .split_once('@')
        .context("web.json package must be <id>@<version>")?;
    ensure!(
        package == client.package,
        "web artifact package {package}@{version} differs from client.package {} for release {}",
        client.package,
        document.release
    );
    ensure!(
        packages
            .into_iter()
            .any(|coordinate| coordinate == (package, version)),
        "web artifact package {package}@{version} is not in release {}",
        document.release
    );
    Ok(())
}

pub(super) fn resolve<'a>(
    document: &EnvironmentDocument,
    packages: impl Iterator<Item = &'a str>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let selected = match &document.client {
        Some(client) => {
            let name = client.package.as_str();
            ensure!(
                packages.into_iter().any(|package| package == name),
                "host {} names client package {name}, which the release does not provide",
                document.route_host
            );
            Some(name)
        }
        None => None,
    };
    Ok(selected
        .map(|name| (document.route_host.clone(), name.to_owned()))
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> EnvironmentDocument {
        serde_json::from_value(serde_json::json!({
            "org": "test", "project": "wms", "env": "dev", "release": "none",
            "target": "cloud", "route_host": "wms.example.com", "policy": "dev", "applications": {}
        }))
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
        document.client = Some(super::super::document::Client {
            package: "receiving".into(),
            web: wamn_execution_contract::bytes_sha256(b"web artifact"),
        });
        assert_eq!(
            resolve(&document, ["wms", "receiving"].into_iter()).unwrap()["wms.example.com"],
            "receiving"
        );
        assert!(
            resolve(&document, ["wms"].into_iter())
                .unwrap_err()
                .to_string()
                .contains("receiving")
        );
    }

    #[test]
    fn a_web_pin_requires_its_exact_package_version_in_the_release() {
        let web: WebManifest =
            serde_json::from_str(include_str!("../../tests/support/web-artifact/web.json"))
                .unwrap();
        let mut document = document();
        document.release = super::super::document::DeclaredRelease::Digest(
            wamn_execution_contract::bytes_sha256(b"release"),
        );
        document.client = Some(super::super::document::Client {
            package: "web_fixture".into(),
            web: wamn_execution_contract::bytes_sha256(b"web artifact"),
        });
        let error = verify(&document, &web, [("web_fixture", "2.0.0")].into_iter())
            .unwrap_err()
            .to_string();
        for detail in ["web_fixture", "1.0.0", &document.release.to_string()] {
            assert!(error.contains(detail), "{error}");
        }
        verify(&document, &web, [("web_fixture", "1.0.0")].into_iter()).unwrap();
        let mut second = document.clone();
        second.env = "qa".into();
        verify(&second, &web, [("web_fixture", "1.0.0")].into_iter()).unwrap();
        assert_eq!(document.client, second.client);
        document.client.as_mut().unwrap().package = "receiving".into();
        let error = verify(&document, &web, [("web_fixture", "1.0.0")].into_iter())
            .unwrap_err()
            .to_string();
        for detail in [
            "web_fixture@1.0.0",
            "receiving",
            &document.release.to_string(),
        ] {
            assert!(error.contains(detail), "{error}");
        }
        document.client.as_mut().unwrap().package = "web_fixture".into();
        document.release = super::super::document::DeclaredRelease::None;
        assert!(verify(&document, &web, [].into_iter()).is_err());
    }
}
