//! Resolve an application's web client before writing its release chart.

pub(super) mod plan;
use plan::verify;
pub(super) use plan::verify_client_package;

use anyhow::{Context as _, ensure};

use super::Platform;
use super::analyse::ReleaseFacts;
use super::document::ApplicationSelection;
use wamn_publish::web_artifact::{WebManifest, pull_web_artifact};

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

    #[test]
    fn a_web_pin_requires_its_package_but_allows_a_different_code_version() {
        let web: WebManifest =
            serde_json::from_str(include_str!("../../tests/support/web-artifact/web.json"))
                .unwrap();
        let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&serde_json::json!({
            "org": "test", "project": "wms", "env": "dev", "target": "cloud",
            "policy": "dev", "revocation_seconds": 60,
            "applications": {"fixture": {"revocation_seconds": 5, "hostname": "wms.example.com",
                "client": {"package": "web_fixture", "web": wamn_digest::bytes_sha256(b"web artifact")}}}
        })).unwrap()).unwrap();
        for (package, packages, member, allowed) in [
            ("web_fixture", vec![("web_fixture", "1.0.0")], true, true),
            (
                "web_fixture",
                vec![("receiving", "1.0.0"), ("web_fixture", "2.0.0")],
                true,
                true,
            ),
            ("web_fixture", vec![("receiving", "1.0.0")], false, false),
            ("receiving", vec![("receiving", "1.0.0")], true, false),
            ("web_fixture", vec![], false, false),
        ] {
            let selection = document.applications.get_mut("fixture").unwrap();
            selection.client.as_mut().unwrap().package = package.into();
            assert_eq!(
                verify_client_package(selection, packages.iter().map(|(id, _)| *id)).is_ok(),
                member
            );
            assert_eq!(
                verify(selection, &web, packages.into_iter()).is_ok(),
                allowed
            );
        }
    }
}
