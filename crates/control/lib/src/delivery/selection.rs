//! The recorded qualification of a release (owner rulings of 2026-10-02 on
//! `wamn-zua8.3`, [release qualification](../../../../../docs/plan/release-qualification.md)).
//!
//! A qualification proves bytes, not names. Its key is the package set, the
//! exact `(package_id, version, component_digest)` triples of the release, and
//! the platform image set the qualification deployed through the release
//! chart: the `@sha256` digests of the candidate's host image and of each of
//! its role components (R12, R15), with the registry names ignored. The gates
//! and identity digests the qualification ran with are recorded beside them and
//! are not part of the key. `publish-qualified-release` records a passing file
//! in `catalog.qualifications` when the release chart names the same set, and
//! `env apply` reads it by the package set and every digest of the release
//! chart's image set.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde::Serialize;
use tokio_postgres::Transaction;
use wamn_catalog::ServingManifest;

use super::{Candidate, Qualification};
use crate::release_chart::ImageSet;

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
    /// `host`, `http`, `materializer`, `gates` and `identity` to
    /// `sha256:<hex>`, for the images given.
    pub image_digests: BTreeMap<&'static str, String>,
}

impl QualificationKey {
    /// The key of what `candidate` ran: its release and its images.
    fn new(candidate: &Candidate) -> anyhow::Result<Self> {
        let (manifest, _) = candidate.manifest()?;
        let mut image_digests = image_set_digests(&candidate_image_set(candidate))?;
        for (name, image) in [
            ("gates", candidate.gates_image.as_deref()),
            ("identity", candidate.identity_image.as_deref()),
        ] {
            if let Some(image) = image {
                image_digests.insert(name, image_digest(image)?);
            }
        }
        Ok(Self {
            package_set: package_set(&manifest)?,
            image_digests,
        })
    }
}

/// The platform image set the candidate's cases deployed through the release chart.
fn candidate_image_set(candidate: &Candidate) -> ImageSet {
    ImageSet {
        host: candidate.host_image.clone(),
        http: candidate.http_image.clone(),
        materializer: candidate.materializer_image.clone(),
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

/// Record a passing qualification in `catalog.qualifications` on the
/// control transaction, and return its SHA-256. A recorded file is kept.
///
/// The key's image digests are the candidate's. `image_set` is the release
/// chart's image set, which `env apply` reads; each of its digests must be the
/// one the qualification ran on, or the record would name bytes no case ran.
pub(super) async fn record(
    control: &Transaction<'_>,
    qualification: &Qualification,
    bytes: &[u8],
    image_set: &ImageSet,
) -> anyhow::Result<String> {
    qualification.require_pass()?;
    let candidate = &qualification.candidate;
    require_same_set(&candidate_image_set(candidate), image_set)?;
    let key = QualificationKey::new(candidate)?;
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

/// Refuse a release chart whose image set differs by any digest from the set
/// the qualification ran on, naming each image that differs.
fn require_same_set(qualified: &ImageSet, chart: &ImageSet) -> anyhow::Result<()> {
    let ran = image_set_digests(qualified)?;
    let named = image_set_digests(chart)?;
    let differing = ran
        .iter()
        .filter(|(image, digest)| named.get(*image) != Some(*digest))
        .map(|(image, digest)| format!("{image} ran {digest}, the chart names {}", named[image]))
        .collect::<Vec<_>>();
    ensure!(
        differing.is_empty(),
        "the release chart's image set is not the one the qualification ran on: {}",
        differing.join("; ")
    );
    Ok(())
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
    fn the_key_is_the_candidates_and_ignores_names_and_registries() {
        let (_, manifest_digest) =
            ServingManifest::from_canonical_bytes(vector::CANONICAL_BYTES).unwrap();
        assert_eq!(manifest_digest.as_str(), vector::DIGEST);
        let directory =
            std::env::temp_dir().join(format!("wamn-selection-key-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let manifest = directory.join("manifest.json");
        std::fs::write(&manifest, vector::CANONICAL_BYTES).unwrap();
        let candidate = |registry: &str| Candidate {
            org: "acme".into(),
            project: "wms".into(),
            environment: "dev".into(),
            tenant: "tenant".into(),
            route_host: "wms.example".into(),
            manifest_path: manifest.clone(),
            target_directory: directory.clone(),
            host_image: format!("{registry}/host:1@sha256:{}", "b".repeat(64)),
            http_image: format!("{registry}/http@sha256:{}", "c".repeat(64)),
            materializer_image: format!("{registry}/materializer@sha256:{}", "d".repeat(64)),
            gates_image: None,
            identity_image: None,
            deployment_files: Vec::new(),
            native_registry_endpoint: None,
            native_registry_insecure: false,
        };
        let key = QualificationKey::new(&candidate("one.example")).unwrap();
        let other = QualificationKey::new(&candidate("two.example/other")).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(key, other);
        assert!(!key.package_set.is_empty());
        assert_eq!(
            key.image_digests,
            BTreeMap::from([
                ("host", format!("sha256:{}", "b".repeat(64))),
                ("http", format!("sha256:{}", "c".repeat(64))),
                ("materializer", format!("sha256:{}", "d".repeat(64))),
            ])
        );
    }

    #[test]
    fn a_chart_that_names_another_role_digest_is_refused() {
        let set = |http: char| ImageSet {
            host: format!("one.example/host:1@sha256:{}", "b".repeat(64)),
            http: format!("one.example/http@sha256:{}", http.to_string().repeat(64)),
            materializer: format!("one.example/materializer@sha256:{}", "d".repeat(64)),
        };
        require_same_set(&set('c'), &set('c')).unwrap();
        let refused = require_same_set(&set('c'), &set('e'))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(&format!("http ran sha256:{}", "c".repeat(64)))
                && !refused.contains("host ran"),
            "{refused}"
        );
    }
}
