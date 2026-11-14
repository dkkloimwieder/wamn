//! Resolve an application's web client before writing its release chart.

pub(super) mod plan;
use plan::verify;
pub(super) use plan::verify_client_package;

use anyhow::{Context as _, ensure};

use super::Platform;
use super::analyse::ReleaseFacts;
use super::document::ApplicationSelection;
use wamn_package_control::web_artifact::{WebManifest, pull_web_artifact};

pub(super) async fn analyse(
    platform: &Platform,
    selection: &ApplicationSelection,
    release: Option<&ReleaseFacts>,
) -> anyhow::Result<()> {
    let Some(client) = selection.client.as_ref() else {
        return Ok(());
    };
    let registry = platform
        .registry(
            platform
                .package_artifact_base
                .as_deref()
                .context("set WAMN_PACKAGE_ARTIFACT_BASE for the pinned web artifact")?,
        )
        .await?;
    let web = pull_web_artifact(&registry, &client.web).await?;
    verify(
        selection,
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
    use crate::environment::document::EnvironmentDocument;

    fn document() -> EnvironmentDocument {
        serde_json::from_value(serde_json::json!({
            "org": "test", "project": "wms", "env": "dev",
            "target": "cloud", "policy": "dev","revocation_seconds":60,
            "applications": {"fixture": {"revocation_seconds":5,"hostname": "wms.example.com"}}}))
        .unwrap()
    }

    #[test]
    fn a_named_client_selects_its_package() {
        let mut document = document();
        document.applications.get_mut("fixture").unwrap().client =
            Some(super::super::document::Client {
                package: "receiving".into(),
                web: wamn_digest::bytes_sha256(b"web artifact"),
            });
        verify_client_package(
            &document.applications["fixture"],
            ["wms", "receiving"].into_iter(),
        )
        .unwrap();
        assert!(
            verify_client_package(&document.applications["fixture"], ["wms"].into_iter()).is_err()
        );
    }

    #[test]
    fn a_web_pin_requires_its_package_but_allows_a_different_code_version() {
        let web: WebManifest =
            serde_json::from_str(include_str!("../../tests/support/web-artifact/web.json"))
                .unwrap();
        let mut document = document();
        // DEP2-03 replaces the shared candidate with application publication.
        for application in document.applications.values_mut() {
            application.current = super::super::document::DeclaredRelease::Digest(
                wamn_digest::bytes_sha256(b"release"),
            );
        }
        document.applications.get_mut("fixture").unwrap().client =
            Some(super::super::document::Client {
                package: "web_fixture".into(),
                web: wamn_digest::bytes_sha256(b"web artifact"),
            });
        verify(
            &document.applications["fixture"],
            &web,
            [("web_fixture", "2.0.0")].into_iter(),
        )
        .unwrap();
        verify(
            &document.applications["fixture"],
            &web,
            [("web_fixture", "1.0.0")].into_iter(),
        )
        .unwrap();
        document
            .applications
            .get_mut("fixture")
            .unwrap()
            .client
            .as_mut()
            .unwrap()
            .package = "receiving".into();
        assert!(
            verify(
                &document.applications["fixture"],
                &web,
                [("web_fixture", "1.0.0")].into_iter()
            )
            .is_err()
        );
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
        assert!(verify(&document.applications["fixture"], &web, [].into_iter()).is_err());
    }
}
