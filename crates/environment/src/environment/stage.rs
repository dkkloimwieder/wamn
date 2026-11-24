//! Application baseline initialization and immutable release caching.

pub(super) mod plan;
pub use plan::{apply_order, sequence_defaults};

use std::collections::BTreeMap;

use super::database::DatabaseSession;
use anyhow::{Context as _, bail};

use super::Platform;
use super::analyse::{self, Installed};
use super::document::EnvironmentDocument;
use wamn_release::package_source::OpenPackage;

/// The installed packages of the project database, read now.
pub(super) async fn installed(
    platform: &Platform,
    system: &dyn DatabaseSession,
    document: &EnvironmentDocument,
) -> anyhow::Result<BTreeMap<wamn_package::PackageName, Installed>> {
    let row = system
        .environment_row(&document.triple())
        .await?
        .context("the environment row is missing after step 4")?;
    Ok(platform
        .database
        .read_project(&analyse::project_url(document, &row)?)
        .await?
        .context("the project database is missing after step 4")?
        .installed)
}

/// The package artifact `<id>-<version>`, pulled from the registry.
pub(super) async fn open(
    platform: &Platform,
    id: &wamn_package::PackageName,
    version: &wamn_package::package_version::PackageVersion,
) -> anyhow::Result<Box<dyn OpenPackage>> {
    let Some(base) = &platform.package_artifact_base else {
        bail!(
            "release names package {id}@{version}; set WAMN_PACKAGE_ARTIFACT_BASE to read \
             its artifact"
        );
    };
    platform
        .packages
        .repository(base)
        .await?
        .open(id, version)
        .await
        .with_context(|| format!("open package artifact {id}@{version}"))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::apply_order;

    #[test]
    fn sequence_defaults_use_map_names_and_refuse_conflicting_declarations() {
        let mut base = wamn_test_infrastructure::publish::source_manifest();
        base.package.id = "base".parse().expect("valid name");
        base.sequences = serde_json::from_value(serde_json::json!({
            "order": {"pattern": "PO-{seq:po}"}
        }))
        .unwrap();
        let mut overlay = base.clone();
        overlay.package.id = "overlay".parse().expect("valid name");
        let applications = serde_json::from_value(serde_json::json!({
            "receiving": {"packages": ["base", "overlay"]}
        }))
        .unwrap();
        let defaults = super::sequence_defaults(&applications, [&base, &overlay]).unwrap();
        assert_eq!(
            defaults,
            [crate::configuration::SequenceDefault {
                application: "receiving".parse().unwrap(),
                name: "order".into(),
                pattern: "PO-{seq:po}".into(),
            }]
        );
        overlay.sequences.get_mut("order").unwrap().pattern = "ACME-{seq:po}".into();
        assert!(super::sequence_defaults(&applications, [&base, &overlay]).is_err());
        assert!(super::sequence_defaults(&BTreeMap::new(), [&base]).is_err());
        let separate = serde_json::from_value(serde_json::json!({
            "receiving": {"packages": ["base"]}, "orders": {"packages": ["overlay"]}
        }))
        .unwrap();
        assert_eq!(
            super::sequence_defaults(&separate, [&base, &overlay])
                .unwrap()
                .len(),
            separate.len()
        );
    }

    /// An overlay applies after the base it extends, whatever the manifest
    /// order (wamn-k8127.5).
    #[test]
    fn an_overlay_applies_after_its_base() {
        let [overlay, base, other, outside] = [
            "client_acme_receiving",
            "wamn_receiving",
            "other",
            "outside_the_release",
        ]
        .map(|name| name.parse().unwrap());
        let packages = [
            (&overlay, vec![&base]),
            (&base, Vec::new()),
            (&other, vec![&outside]),
        ];
        let order = apply_order(&packages).unwrap();
        assert_eq!(
            order.iter().copied().collect::<BTreeSet<_>>(),
            (0..packages.len()).collect()
        );
        for (index, (_, dependencies)) in packages.iter().enumerate() {
            for dependency in dependencies {
                if let Some(base) = packages.iter().position(|(id, _)| id == dependency) {
                    assert!(
                        order.iter().position(|item| *item == base)
                            < order.iter().position(|item| *item == index)
                    );
                }
            }
        }
        assert!(
            apply_order(&[
                (&"a".parse().unwrap(), vec![&"b".parse().unwrap()]),
                (&"b".parse().unwrap(), vec![&"a".parse().unwrap()])
            ])
            .is_err()
        );
    }
}
