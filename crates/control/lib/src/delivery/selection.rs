//! The recorded qualification of a release (owner rulings of 2026-10-02 on
//! `wamn-zua8.3`, [release qualification](../../../../../docs/plan/release-qualification.md)).
//!
//! A qualification proves bytes, not names. Its key is the package set, the
//! exact `(package_id, version, component_digest)` triples of the release, and
//! the `@sha256` digests of the host, gates and identity images, with the
//! registry names ignored. `publish-qualified-release` records a passing file
//! in `catalog.qualifications`, and `env apply` reads it by the package set
//! and the host image digest of the release chart.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde::Serialize;
use tokio_postgres::Transaction;
use wamn_catalog::ServingManifest;

use super::Qualification;

/// One `(package_id, version, component_digest)` triple of a release.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PackageTriple {
    pub package_id: String,
    pub version: String,
    pub component_digest: String,
}

/// The bytes a qualification proves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualificationKey {
    /// Sorted triples of the release.
    pub package_set: Vec<PackageTriple>,
    /// `host`, `gates` and `identity` to `sha256:<hex>`, for the images given.
    pub image_digests: BTreeMap<&'static str, String>,
}

impl QualificationKey {
    fn new(
        manifest: &ServingManifest,
        host: &str,
        gates: Option<&str>,
        identity: Option<&str>,
    ) -> anyhow::Result<Self> {
        let mut image_digests = BTreeMap::from([("host", image_digest(host)?)]);
        for (name, image) in [("gates", gates), ("identity", identity)] {
            if let Some(image) = image {
                image_digests.insert(name, image_digest(image)?);
            }
        }
        Ok(Self {
            package_set: package_set(manifest)?,
            image_digests,
        })
    }
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
                        component.component, component.package_id
                    )
                })?;
            Ok(PackageTriple {
                package_id: component.package_id.clone(),
                version: (*version).to_owned(),
                component_digest: component.digest.as_str().to_owned(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    triples.sort();
    triples.dedup();
    Ok(triples)
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

/// Record a passing qualification in `catalog.qualifications` on the
/// control transaction, and return its SHA-256. A recorded file is kept.
pub(super) async fn record(
    control: &Transaction<'_>,
    qualification: &Qualification,
    bytes: &[u8],
) -> anyhow::Result<String> {
    qualification.require_pass()?;
    let (qualified, _) = qualification.candidate.manifest()?;
    let candidate = &qualification.candidate;
    let key = QualificationKey::new(
        &qualified,
        &candidate.host_image,
        candidate.gates_image.as_deref(),
        candidate.identity_image.as_deref(),
    )?;
    let sha256 = format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
    );
    control
        .execute(
            "INSERT INTO catalog.qualifications \
               (qualification_sha256, package_set, image_digests) \
             VALUES ($1, $2::text::jsonb, $3::text::jsonb) ON CONFLICT DO NOTHING",
            &[
                &sha256,
                &serde_json::to_string(&key.package_set)?,
                &serde_json::to_string(&key.image_digests)?,
            ],
        )
        .await
        .context("record the qualification")?;
    Ok(sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod vector {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs"
        ));
    }

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

    #[test]
    fn the_key_ignores_names_and_registries() {
        let (manifest, manifest_digest) =
            ServingManifest::from_canonical_bytes(vector::CANONICAL_BYTES).unwrap();
        assert_eq!(manifest_digest.as_str(), vector::DIGEST);
        let digest = "b".repeat(64);
        let key = QualificationKey::new(
            &manifest,
            &format!("one.example/host@sha256:{digest}"),
            None,
            None,
        )
        .unwrap();
        let other = QualificationKey::new(
            &manifest,
            &format!("two.example/other/host@sha256:{digest}"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(key, other);
        assert!(!key.package_set.is_empty());
    }
}
