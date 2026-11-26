//! Application baseline initialization and immutable release caching.

pub(super) mod plan;
pub use plan::{apply_order, sequence_defaults};

use std::collections::BTreeMap;

use crate::env_sql_effects::{self, PostgresSession};
use anyhow::{Context as _, ensure};

use super::Platform;
use super::analyse::{self, Installed};
use super::document::EnvDocument;
use wamn_component_policy::PackageManifest;
use wamn_digest::Digest;
use wamn_package::PackageName;
use wamn_release::release_package::{OpenedPackage, open_package_digest};

/// The installed packages of the project database, read now.
pub(super) async fn installed(
    system: &PostgresSession,
    document: &EnvDocument,
) -> anyhow::Result<BTreeMap<wamn_package::PackageName, Installed>> {
    let row = system
        .env_row(&document.triple())
        .await?
        .context("the environment row is missing after step 4")?;
    Ok(
        env_sql_effects::read_project(&analyse::project_url(document, &row)?)
            .await?
            .context("the project database is missing after step 4")?
            .installed,
    )
}

/// Open installed packages by their recorded artifact digests.
pub(super) async fn open(
    platform: &Platform,
    installed: impl IntoIterator<Item = (PackageName, Option<Digest>)>,
) -> anyhow::Result<Vec<(PackageName, OpenedPackage, PackageManifest)>> {
    let mut installed = installed.into_iter().peekable();
    if installed.peek().is_none() {
        return Ok(Vec::new());
    }
    let base = platform
        .package_artifact_base
        .as_deref()
        .context("package artifact base is absent")?;
    let registry = platform.packages.repository(base).await?;
    let mut packages = Vec::with_capacity(installed.size_hint().0);
    for (id, digest) in installed {
        let digest =
            digest.with_context(|| format!("installed package {id} requires a pinned artifact"))?;
        let (artifact, manifest) = open_package_digest(&registry, &digest)
            .await
            .with_context(|| format!("open installed package {id} at {digest}"))?;
        ensure!(
            manifest.package.id == id,
            "installed artifact {digest} is not {id}"
        );
        packages.push((id, artifact, manifest));
    }
    Ok(packages)
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
        let apps = serde_json::from_value(serde_json::json!({
            "receiving": {"packages": ["base", "overlay"]}
        }))
        .unwrap();
        let defaults = super::sequence_defaults(&apps, [&base, &overlay]).unwrap();
        assert_eq!(
            defaults,
            [crate::configuration::SequenceDefault {
                app: "receiving".parse().unwrap(),
                name: "order".into(),
                pattern: "PO-{seq:po}".into(),
            }]
        );
        overlay.sequences.get_mut("order").unwrap().pattern = "ACME-{seq:po}".into();
        assert!(super::sequence_defaults(&apps, [&base, &overlay]).is_err());
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
