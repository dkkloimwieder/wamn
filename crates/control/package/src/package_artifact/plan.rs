use super::{
    BTreeMap, ComponentConnection, EMPTY_CONFIG, EMPTY_CONFIG_MEDIA_TYPE, PACKAGE_ARTIFACT_TYPE,
    PackageManifest, PackedLayer, Path, PathBuf, bail, ensure, sha256_digest,
};
use anyhow::Context as _;

pub(crate) fn component_connection_names(
    manifest: &PackageManifest,
    component: &str,
    connections: &[ComponentConnection],
) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    names.extend(
        connections
            .iter()
            .map(|connection| connection.store_alias.clone()),
    );
    if let Some(declaration) = manifest.components.get(component) {
        names.extend(
            declaration
                .connections
                .iter()
                .filter(|name| manifest.connections.contains_key(*name))
                .cloned(),
        );
    }
    names.into_iter().collect()
}

pub(super) fn digest_refusal(
    path: &Path,
    recorded: &str,
    tag: &str,
    digest: &str,
) -> anyhow::Result<()> {
    let Some((recorded_tag, recorded_digest)) = recorded
        .strip_suffix('\n')
        .and_then(|line| line.split_once(' '))
    else {
        bail!("{} must hold one line `<tag> <digest>`", path.display());
    };
    ensure!(
        recorded_tag != tag || recorded_digest == digest,
        "package {tag} packs to {digest}, but {} records {recorded_digest} for the same tag; \
         registry tags are immutable, so bump the version in wamn.k and run wamn build \
         --record-digest",
        path.display()
    );
    ensure!(
        recorded_tag == tag,
        "package {tag} packs to {digest}, but {} records {recorded_tag}; run wamn build \
         --record-digest",
        path.display()
    );
    Ok(())
}

/// The canonical image manifest bytes of `layers`.
pub(super) fn image_manifest(
    layers: &[PackedLayer],
    annotations: &BTreeMap<String, String>,
    image_media_type: &str,
) -> Vec<u8> {
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": image_media_type,
        "artifactType": PACKAGE_ARTIFACT_TYPE,
        "config": {
            "mediaType": EMPTY_CONFIG_MEDIA_TYPE,
            "digest": sha256_digest(EMPTY_CONFIG),
            "size": EMPTY_CONFIG.len(),
        },
        "layers": layers
            .iter()
            .map(|layer| serde_json::json!({
                "mediaType": layer.media_type,
                "digest": sha256_digest(&layer.bytes),
                "size": layer.bytes.len(),
                "annotations": layer.annotations,
            }))
            .collect::<Vec<_>>(),
        "annotations": annotations,
    });
    wamn_execution_contract::canonical_json_bytes(&manifest)
}

/// The package id and version of a package manifest.
pub(super) fn package_coordinate(manifest: &[u8]) -> anyhow::Result<(String, String)> {
    let document: serde_json::Value = serde_json::from_slice(manifest)?;
    let field = |pointer: &str| {
        document
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("the manifest has no {pointer}"))
    };
    Ok((field("/package/id")?, field("/package/version")?))
}

/// One deterministic tar of `entries`, keyed by package-relative path.
pub(super) fn tar_layer(
    title: &str,
    entries: &BTreeMap<String, Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        builder
            .append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("add {name} to the {title} layer"))?;
    }
    builder
        .into_inner()
        .with_context(|| format!("finish the {title} layer"))
}

/// The file of the package-relative `logical` path. A `generated/` path names
/// a file of the package's output root.
pub(super) fn physical_path(root: &Path, logical: &str) -> PathBuf {
    let logical = Path::new(logical);
    if let Ok(output) = logical.strip_prefix("generated") {
        wamn_schema_generator::output_root(root).join(output)
    } else {
        root.join(logical)
    }
}

/// Refuse package declarations before the reader drops retired fields.
pub(super) fn refuse_workflow_declarations(bytes: &[u8]) -> anyhow::Result<()> {
    let manifest: serde_json::Value = serde_json::from_slice(bytes)?;
    ensure!(
        manifest.get("workflows").is_none(),
        "a package cannot declare workflows; author workflows in the environment"
    );
    if let Some(operations) = manifest
        .get("custom_operations")
        .and_then(serde_json::Value::as_object)
    {
        for (name, operation) in operations {
            ensure!(
                operation.get("registration").is_none(),
                "operation {name:?} cannot declare a registration; author an event workflow in the environment"
            );
        }
    }
    Ok(())
}
