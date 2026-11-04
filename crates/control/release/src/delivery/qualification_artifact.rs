//! The qualification of a release as an OCI artifact
//! (docs/architecture/overview.md#release-identity).
//!
//! `qualify-release` pushes one artifact after every case of a release has
//! passed. It is an OCI image manifest of artifact type
//! [`QUALIFICATION_ARTIFACT_TYPE`] with the empty config, a `subject` that is
//! the release manifest's descriptor, and one layer: the canonical JSON
//! [`QualificationPayload`]. It lives in the release manifest repository
//! under [`qualification_tag`], derived from the release digest and the
//! platform image set, and it is immutable: another artifact under the tag
//! refuses. `env apply` derives the same tag from the release it applies and
//! the release chart's image set and reads the artifact from the registry.
//! `qualify-release` reads it the same way before any case runs, and reuses
//! a qualification it finds there.

use std::collections::BTreeMap;

use anyhow::{Context as _, bail, ensure};
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_package_artifact::{PackageRegistry, Repository, sha256_digest};
use wamn_runtime::release_manifest_artifact::RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE;

use crate::release_chart::ImageSet;

/// Artifact type of a release qualification.
pub const QUALIFICATION_ARTIFACT_TYPE: &str = "application/vnd.wamn.release-qualification.v1";

/// Media type of the one layer, the payload.
pub const QUALIFICATION_PAYLOAD_MEDIA_TYPE: &str =
    "application/vnd.wamn.release-qualification.payload.v1+json";

/// The annotation that names the release digest.
const RELEASE_DIGEST_ANNOTATION: &str = "wamn.release.digest";
/// The annotation that names the image set digest.
const IMAGE_SET_DIGEST_ANNOTATION: &str = "wamn.image-set.digest";

/// The OCI empty config of a data artifact.
const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
const EMPTY_CONFIG: &[u8] = b"{}";

/// The hex characters of the image set digest in the tag. An OCI tag has at
/// most 128 characters, so the tag carries the release digest whole and this
/// much of the image set digest; the reader checks the whole set in the
/// payload.
const IMAGE_SET_TAG_HEX: usize = 32;

/// The format of [`QualificationPayload`]. Format 2 drops the runner and the
/// chart version: the payload is a function of the release, the image set and
/// the cases, so a re-run on the same inputs writes the same bytes.
pub const QUALIFICATION_PAYLOAD_FORMAT: u32 = 2;

/// The payload of a qualification artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationPayload {
    pub format: u32,
    /// The release, `sha256:<hex>`.
    pub release_digest: String,
    /// `host`, `http` and `materializer` to `sha256:<hex>`.
    pub image_set: BTreeMap<String, String>,
    /// The cases that passed.
    pub cases: Vec<String>,
}

/// Whether the push wrote the artifact or found the same one under its tag,
/// or whether `qualify-release` found it before any case ran and reused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualificationPushDisposition {
    Pushed,
    AlreadyPresent,
    Reused,
}

/// A qualification read from its tag and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundQualification {
    /// `<release repository>:<tag>`.
    pub reference: String,
    /// The artifact's manifest digest.
    pub digest: String,
    pub payload: QualificationPayload,
}

/// The `sha256:<hex>` of each image of the platform image set: the host and
/// every role component. Registry names are not part of it.
///
/// # Errors
///
/// When an image is not pinned by digest.
pub fn image_set_digests(image_set: &ImageSet) -> anyhow::Result<BTreeMap<String, String>> {
    Ok(BTreeMap::from([
        ("host".to_owned(), image_digest(&image_set.host)?),
        ("http".to_owned(), image_digest(&image_set.http)?),
        (
            "materializer".to_owned(),
            image_digest(&image_set.materializer)?,
        ),
    ]))
}

/// The `sha256:<hex>` of an image reference. A reference without an
/// `@sha256` digest is refused, so the key is bytes.
fn image_digest(reference: &str) -> anyhow::Result<String> {
    let (repository, hex) = reference
        .rsplit_once("@sha256:")
        .with_context(|| format!("image {reference:?} has no @sha256 digest; a tag is refused"))?;
    ensure!(
        !repository.is_empty() && is_hex_digest(hex),
        "image {reference:?} has an invalid @sha256 digest"
    );
    Ok(format!("sha256:{hex}"))
}

fn is_hex_digest(hex: &str) -> bool {
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `sha256:<hex>` of the canonical JSON of `image_set`.
fn image_set_digest(image_set: &BTreeMap<String, String>) -> anyhow::Result<String> {
    Ok(sha256_digest(&canonical(image_set)?))
}

fn canonical(value: &impl Serialize) -> anyhow::Result<Vec<u8>> {
    Ok(wamn_execution_contract::canonical_json_bytes(
        &serde_json::to_value(value)?,
    ))
}

/// `qualification-<release hex>-<image set hex>` of the release
/// `release_digest` on `image_set`.
///
/// # Errors
///
/// When the release digest is not `sha256:<hex>`.
pub fn qualification_tag(
    release_digest: &str,
    image_set: &BTreeMap<String, String>,
) -> anyhow::Result<String> {
    let release = release_digest
        .strip_prefix("sha256:")
        .filter(|hex| is_hex_digest(hex))
        .with_context(|| format!("release {release_digest:?} is not sha256:<hex>"))?;
    let set = image_set_digest(image_set)?;
    let set = set.trim_start_matches("sha256:");
    Ok(format!(
        "qualification-{release}-{}",
        &set[..IMAGE_SET_TAG_HEX]
    ))
}

/// The canonical manifest bytes of the qualification `payload` of a release
/// manifest of `release_size` bytes.
fn manifest(
    payload: &[u8],
    qualification: &QualificationPayload,
    release_size: usize,
) -> anyhow::Result<Vec<u8>> {
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": OCI_IMAGE_MEDIA_TYPE,
        "artifactType": QUALIFICATION_ARTIFACT_TYPE,
        "config": {
            "mediaType": EMPTY_CONFIG_MEDIA_TYPE,
            "digest": sha256_digest(EMPTY_CONFIG),
            "size": EMPTY_CONFIG.len(),
        },
        "layers": [{
            "mediaType": QUALIFICATION_PAYLOAD_MEDIA_TYPE,
            "digest": sha256_digest(payload),
            "size": payload.len(),
        }],
        "subject": {
            "mediaType": RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE,
            "digest": qualification.release_digest,
            "size": release_size,
        },
        "annotations": {
            RELEASE_DIGEST_ANNOTATION: qualification.release_digest,
            IMAGE_SET_DIGEST_ANNOTATION: image_set_digest(&qualification.image_set)?,
        },
    });
    Ok(wamn_execution_contract::canonical_json_bytes(&manifest))
}

/// Push `payload` to the release repository of `releases`, whose
/// `artifact_base` is the release manifests' base. `release_size` is the
/// length of the release manifest bytes. Returns the artifact's manifest
/// digest and whether this push wrote it.
///
/// # Errors
///
/// When the tag holds another artifact, which names both digests, or the
/// registry refuses the push.
pub async fn push(
    releases: &PackageRegistry,
    payload: &QualificationPayload,
    release_size: usize,
) -> anyhow::Result<(String, QualificationPushDisposition)> {
    let tag = qualification_tag(&payload.release_digest, &payload.image_set)?;
    let repository = Repository::open(releases, &tag)?;
    let layer = canonical(payload)?;
    let manifest = manifest(&layer, payload, release_size)?;
    let digest = sha256_digest(&manifest);
    if let Some((_, found)) = repository.manifest().await? {
        ensure!(
            found == digest,
            "qualification {} holds {found}; this qualification is {digest}, and a \
             qualification tag is immutable",
            repository.reference
        );
        return Ok((digest, QualificationPushDisposition::AlreadyPresent));
    }
    for bytes in [layer.as_slice(), EMPTY_CONFIG] {
        repository
            .client
            .push_blob(&repository.reference, bytes.to_vec(), &sha256_digest(bytes))
            .await
            .with_context(|| format!("push a blob of qualification {}", repository.reference))?;
    }
    repository
        .client
        .push_manifest_raw(
            &repository.reference,
            manifest,
            reqwest::header::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE),
        )
        .await
        .with_context(|| format!("push qualification {}", repository.reference))?;
    Ok((digest, QualificationPushDisposition::Pushed))
}

/// Read the qualification of `release_digest` on `image_set` from the
/// release repository of `releases`, check it, and return its manifest
/// digest.
///
/// # Errors
///
/// When the tag is absent, which names it, or the artifact is not a
/// qualification of that release on that image set.
pub async fn find(
    releases: &PackageRegistry,
    release_digest: &str,
    image_set: &ImageSet,
) -> anyhow::Result<String> {
    let image_set = image_set_digests(image_set)?;
    let Some(found) = lookup(releases, release_digest, &image_set).await? else {
        let tag = qualification_tag(release_digest, &image_set)?;
        bail!(
            "release {release_digest} has no qualification on the chart's image set: \
             {} is absent",
            Repository::open(releases, &tag)?.reference
        );
    };
    Ok(found.digest)
}

/// Read the qualification of `release_digest` on the image set digests
/// `image_set` from the release repository of `releases`, with every check
/// of [`find`]. Returns `None` when the tag is absent.
///
/// # Errors
///
/// When the tag holds an artifact that is not a qualification of that
/// release on that image set, or one of another payload format.
pub async fn lookup(
    releases: &PackageRegistry,
    release_digest: &str,
    image_set: &BTreeMap<String, String>,
) -> anyhow::Result<Option<FoundQualification>> {
    let tag = qualification_tag(release_digest, image_set)?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let repository = Repository::open(releases, &tag)?;
    let Some((bytes, digest)) = repository.manifest().await? else {
        return Ok(None);
    };
    let manifest: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse qualification {}", repository.reference))?;
    ensure!(
        manifest["artifactType"] == QUALIFICATION_ARTIFACT_TYPE,
        "{} is not a {QUALIFICATION_ARTIFACT_TYPE} artifact",
        repository.reference
    );
    ensure!(
        manifest["subject"]["digest"] == release_digest,
        "the subject of qualification {} is not release {release_digest}",
        repository.reference
    );
    let layers = manifest["layers"].as_array().map_or(&[][..], Vec::as_slice);
    let [layer] = layers else {
        bail!(
            "qualification {} has {} layers, not one",
            repository.reference,
            layers.len()
        );
    };
    ensure!(
        layer["mediaType"] == QUALIFICATION_PAYLOAD_MEDIA_TYPE,
        "the layer of qualification {} is not its payload",
        repository.reference
    );
    let descriptor: oci_client::manifest::OciDescriptor = serde_json::from_value(layer.clone())
        .with_context(|| format!("read the layer of qualification {}", repository.reference))?;
    let payload = wamn_package_artifact::registry_pull::blob(
        &repository.client,
        &repository.reference,
        &descriptor,
        deadline,
    )
    .await
    .with_context(|| format!("pull qualification {}", repository.reference))?;
    ensure!(
        sha256_digest(&payload) == descriptor.digest,
        "the payload of qualification {} does not hash to {}",
        repository.reference,
        descriptor.digest
    );
    let payload: QualificationPayload = serde_json::from_slice(&payload)
        .with_context(|| format!("read qualification {}", repository.reference))?;
    ensure!(
        payload.format == QUALIFICATION_PAYLOAD_FORMAT,
        "qualification {} has payload format {}, not {QUALIFICATION_PAYLOAD_FORMAT}",
        repository.reference,
        payload.format
    );
    ensure!(
        payload.release_digest == release_digest && payload.image_set == *image_set,
        "qualification {} names release {} on another image set",
        repository.reference,
        payload.release_digest
    );
    Ok(Some(FoundQualification {
        reference: repository.reference.to_string(),
        digest,
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(registry: &str, http: char) -> ImageSet {
        ImageSet {
            host: format!("{registry}/host:1@sha256:{}", "b".repeat(64)),
            http: format!("{registry}/http@sha256:{}", http.to_string().repeat(64)),
            materializer: format!("{registry}/materializer@sha256:{}", "d".repeat(64)),
        }
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

    /// The tag ignores registry names, is an OCI tag, and changes with the
    /// release or any image digest.
    #[test]
    fn the_tag_is_the_release_and_the_image_set() {
        let release = format!("sha256:{}", "1".repeat(64));
        let tag = |release: &str, set: &ImageSet| {
            qualification_tag(release, &image_set_digests(set).unwrap()).unwrap()
        };
        let one = tag(&release, &set("one.example", 'c'));
        assert_eq!(one, tag(&release, &set("two.example/other", 'c')));
        assert!(one.starts_with(&format!("qualification-{}-", "1".repeat(64))));
        wamn_package_artifact::ensure_tag(&one).unwrap();
        assert_ne!(one, tag(&release, &set("one.example", 'e')));
        assert_ne!(
            one,
            tag(
                &format!("sha256:{}", "2".repeat(64)),
                &set("one.example", 'c')
            )
        );
        assert!(
            qualification_tag("sha256:abc", &image_set_digests(&set("r", 'c')).unwrap()).is_err()
        );
    }

    /// A platform version that copies the host under its own tag and keeps
    /// every digest keeps the tag. Another host digest is another tag.
    #[test]
    fn the_tag_is_the_host_digest_and_not_its_tag() {
        let release = format!("sha256:{}", "1".repeat(64));
        let tag =
            |set: &ImageSet| qualification_tag(&release, &image_set_digests(set).unwrap()).unwrap();
        let before = set("registry.example", 'c');
        let mut bumped = before.clone();
        bumped.host = format!("registry.example/host:0.1.98@sha256:{}", "b".repeat(64));
        assert_eq!(tag(&before), tag(&bumped));
        let mut rebuilt = bumped.clone();
        rebuilt.host = format!("registry.example/host:0.1.98@sha256:{}", "e".repeat(64));
        assert_ne!(tag(&bumped), tag(&rebuilt));
        let mut materializer = bumped;
        materializer.materializer =
            format!("registry.example/materializer@sha256:{}", "e".repeat(64));
        assert_ne!(tag(&before), tag(&materializer));
    }

    #[test]
    fn the_payload_is_canonical_json_with_the_spec_fields() {
        let payload = QualificationPayload {
            format: QUALIFICATION_PAYLOAD_FORMAT,
            release_digest: format!("sha256:{}", "1".repeat(64)),
            image_set: image_set_digests(&set("r", 'c')).unwrap(),
            cases: vec!["cluster::released_wms_routes".to_owned()],
        };
        let bytes = canonical(&payload).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert_eq!(
            text,
            format!(
                r#"{{"cases":["cluster::released_wms_routes"],"format":2,"image_set":{{"host":"sha256:{}","http":"sha256:{}","materializer":"sha256:{}"}},"release_digest":"sha256:{}"}}"#,
                "b".repeat(64),
                "c".repeat(64),
                "d".repeat(64),
                "1".repeat(64)
            )
        );
        assert_eq!(
            serde_json::from_slice::<QualificationPayload>(&bytes).unwrap(),
            payload
        );
        let manifest: Value =
            serde_json::from_slice(&manifest(&bytes, &payload, 42).unwrap()).unwrap();
        assert_eq!(manifest["artifactType"], QUALIFICATION_ARTIFACT_TYPE);
        assert_eq!(manifest["subject"]["digest"], payload.release_digest);
        assert_eq!(manifest["subject"]["size"], 42);
        assert_eq!(
            manifest["annotations"][RELEASE_DIGEST_ANNOTATION],
            payload.release_digest
        );
    }
}
