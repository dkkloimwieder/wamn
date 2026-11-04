use super::{
    BTreeMap, EMPTY_CONFIG, EMPTY_CONFIG_MEDIA_TYPE, IMAGE_SET_DIGEST_ANNOTATION,
    IMAGE_SET_TAG_HEX, ImageSet, QUALIFICATION_ARTIFACT_TYPE, QUALIFICATION_PAYLOAD_MEDIA_TYPE,
    QualificationPayload, RELEASE_DIGEST_ANNOTATION, Serialize, ensure, json, sha256_digest,
};
use anyhow::Context as _;

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
pub(super) fn image_digest(reference: &str) -> anyhow::Result<String> {
    let (repository, hex) = reference
        .rsplit_once("@sha256:")
        .with_context(|| format!("image {reference:?} has no @sha256 digest; a tag is refused"))?;
    ensure!(
        !repository.is_empty() && is_hex_digest(hex),
        "image {reference:?} has an invalid @sha256 digest"
    );
    Ok(format!("sha256:{hex}"))
}

pub(super) fn is_hex_digest(hex: &str) -> bool {
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `sha256:<hex>` of the canonical JSON of `image_set`.
pub(super) fn image_set_digest(image_set: &BTreeMap<String, String>) -> anyhow::Result<String> {
    Ok(sha256_digest(&canonical(image_set)?))
}

pub(super) fn canonical(value: &impl Serialize) -> anyhow::Result<Vec<u8>> {
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
pub(super) fn manifest(
    payload: &[u8],
    qualification: &QualificationPayload,
    release_size: usize,
    image_media_type: &str,
    release_media_type: &str,
) -> anyhow::Result<Vec<u8>> {
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": image_media_type,
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
            "mediaType": release_media_type,
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
