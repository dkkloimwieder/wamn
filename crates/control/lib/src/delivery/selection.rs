//! The qualification that a select uses (owner rulings of 2026-10-02 on
//! `wamn-zua8.3`, [release qualification](../../../../../docs/plan/release-qualification.md)).
//!
//! A qualification proves bytes, not names. Its key is the package set, the
//! exact `(package_id, version, component_digest)` triples of the release, and
//! the `@sha256` digests of the host, gates and identity images, with the
//! registry names ignored. A select with a qualification file records the file
//! in `catalog.qualifications`. A select without one reuses a recorded
//! qualification whose key equals the key of the release and its images.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, bail, ensure};
use serde::Serialize;
use tokio_postgres::Transaction;
use wamn_catalog::ServingManifest;

use super::Qualification;

/// Where a select takes its qualification from.
#[derive(Debug, Clone)]
pub enum QualificationSource {
    /// A qualification file. A passing file is recorded for reuse.
    File(PathBuf),
    /// The images of the candidate. A recorded qualification with the same
    /// package set and image digests is reused.
    Images {
        /// The host image, `repository@sha256:<digest>`.
        host: String,
        /// The gates image, `repository@sha256:<digest>`.
        gates: Option<String>,
        /// The identity image, `repository@sha256:<digest>`.
        identity: Option<String>,
    },
}

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

/// Resolve the qualification of `source` for the release `manifest`, on the
/// control transaction, and return its SHA-256.
pub(super) async fn resolve(
    control: &Transaction<'_>,
    source: &QualificationSource,
    manifest: &ServingManifest,
) -> anyhow::Result<String> {
    match source {
        QualificationSource::File(path) => {
            let bytes = std::fs::read(path).context("read the qualification result")?;
            let qualification: Qualification =
                serde_json::from_slice(&bytes).context("decode the qualification result")?;
            qualification.require_pass()?;
            let (qualified, _) = qualification.candidate.manifest()?;
            let candidate = &qualification.candidate;
            let key = QualificationKey::new(
                &qualified,
                &candidate.host_image,
                candidate.gates_image.as_deref(),
                candidate.identity_image.as_deref(),
            )?;
            ensure!(
                key.package_set == package_set(manifest)?,
                "the qualification's package set differs from the release's"
            );
            let sha256 = format!(
                "sha256:{}",
                hex::encode(ring::digest::digest(&ring::digest::SHA256, &bytes))
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
        QualificationSource::Images {
            host,
            gates,
            identity,
        } => {
            let key = QualificationKey::new(manifest, host, gates.as_deref(), identity.as_deref())?;
            let row = control
                .query_opt(
                    "SELECT qualification_sha256 FROM catalog.qualifications \
                      WHERE package_set = $1::text::jsonb AND image_digests = $2::text::jsonb \
                      ORDER BY recorded_at DESC LIMIT 1",
                    &[
                        &serde_json::to_string(&key.package_set)?,
                        &serde_json::to_string(&key.image_digests)?,
                    ],
                )
                .await
                .context("look up a recorded qualification")?;
            let Some(row) = row else {
                bail!(
                    "no recorded qualification has the package set and image digests of this release"
                );
            };
            Ok(row.get(0))
        }
    }
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
        let mut renamed = manifest.clone();
        renamed.release.environment = "another-environment".to_owned();
        renamed.release.tenant_id = "another-tenant".to_owned();
        let other = QualificationKey::new(
            &renamed,
            &format!("two.example/other/host@sha256:{digest}"),
            None,
            None,
        )
        .unwrap();
        assert_eq!(key, other);
        assert!(!key.package_set.is_empty());
    }
}
