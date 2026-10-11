//! The recorded qualification of a release (owner rulings of 2026-10-02 on
//! `wamn-zua8.3`, [release qualification](../../../../../docs/plan/release-qualification.md)).
//!
//! A qualification proves bytes, not names. Its key is the package set, the
//! exact `(package_id, version, component_digest)` triples of the release, and
//! the platform image set the qualification deployed through the release
//! chart: the `@sha256` digests of the candidate's host image and of each of
//! its role components (R12, R15), with the registry names ignored. The gates
//! and identity digests the qualification ran with are recorded beside them and
//! are not part of the key. `env apply` reads it by the package set and every
//! digest of the release chart's image set.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde::Serialize;
use wamn_catalog::ServingManifest;

use crate::release_chart::ImageSet;

/// One `(package_id, version, component_digest)` triple of a release.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PackageTriple {
    pub package_id: String,
    pub version: String,
    pub component_digest: String,
}

/// The sorted `(package_id, version, component_digest)` triples of a release.
pub fn package_set(manifest: &ServingManifest) -> anyhow::Result<Vec<PackageTriple>> {
    let versions: BTreeMap<&str, &str> = manifest
        .release
        .packages
        .iter()
        .map(|package| (package.package_id(), package.package_version()))
        .collect();
    let mut triples = manifest
        .components
        .iter()
        .map(|component| {
            let version = versions
                .get(component.package_id.as_str())
                .with_context(|| {
                    format!(
                        "component {} names package {}, which the release does not hold",
                        component.descriptor.component, component.package_id
                    )
                })?;
            Ok(PackageTriple {
                package_id: component.package_id.clone(),
                version: (*version).to_owned(),
                component_digest: component.descriptor.component_digest.as_str().to_owned(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    triples.sort();
    triples.dedup();
    Ok(triples)
}

/// The `sha256:<hex>` of each image of the platform image set, by its key in
/// `catalog.qualifications.image_digests`: the host and every role component.
pub fn image_set_digests(image_set: &ImageSet) -> anyhow::Result<BTreeMap<&'static str, String>> {
    Ok(BTreeMap::from([
        ("host", image_digest(&image_set.host)?),
        ("http", image_digest(&image_set.http)?),
        ("materializer", image_digest(&image_set.materializer)?),
    ]))
}

/// The `sha256:<hex>` of an image reference. A reference without an
/// `@sha256` digest is refused, so the lookup is by bytes.
pub fn image_digest(reference: &str) -> anyhow::Result<String> {
    let (repository, hex) = reference
        .rsplit_once("@sha256:")
        .with_context(|| format!("image {reference:?} has no @sha256 digest; a tag is refused"))?;
    ensure!(
        !repository.is_empty()
            && hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "image {reference:?} has an invalid @sha256 digest"
    );
    Ok(format!("sha256:{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_without_a_digest_is_refused() {
        let digest = "a".repeat(64);
        assert_eq!(
            image_digest(&format!("registry.example/wamn/host@sha256:{digest}")).unwrap(),
            format!("sha256:{digest}")
        );
        for refused in [
            "registry.example/wamn/host:1.0",
            "registry.example/wamn/host@sha256:ABC",
            "@sha256:aaaa",
        ] {
            assert!(image_digest(refused).is_err(), "accepted {refused}");
        }
    }
}
