//! Registry transport and image archives for test fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::manifest::{
    IMAGE_MANIFEST_LIST_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_INDEX_MEDIA_TYPE,
    OCI_IMAGE_MEDIA_TYPE,
};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use serde_yaml::{Mapping, Value};
use wamn_package_artifact::registry_credentials::read_registry_credentials;

/// The operator release's namespace, where every environment release and
/// its hosts live (R6).
pub const NAMESPACE: &str = "wamn-system";
/// The host group keys and variables that the release chart derives.
const DERIVED_KEYS: [&str; 6] = [
    "name",
    "namespace",
    "service",
    "extraArgs",
    "image",
    "plugins",
];
const DERIVED_VARIABLES: [&str; 8] = [
    "WAMN_ORG",
    "WAMN_PROJECT",
    "WAMN_DRAIN_BOUND_SECONDS",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_SERVICE_NAME",
    "OTEL_TRACES_SAMPLER",
    "OTEL_TRACES_SAMPLER_ARG",
    "WASMCLOUD_HOST_ENVIRONMENT",
];

/// One registry a role image is read from or written to.
#[derive(Debug, Clone)]
pub struct Registry {
    /// `host:port`.
    pub authority: String,
    /// A Docker configuration with this registry's credential; anonymous without.
    pub auth_file: Option<PathBuf>,
    /// Plain HTTP.
    pub insecure: bool,
    pub ca_paths: Vec<PathBuf>,
}

impl Registry {
    fn auth(&self) -> anyhow::Result<RegistryAuth> {
        let Some(file) = &self.auth_file else {
            return Ok(RegistryAuth::Anonymous);
        };
        let credentials = read_registry_credentials(file, &self.authority)
            .with_context(|| format!("read the credential of registry {}", self.authority))?;
        Ok(RegistryAuth::Basic(
            credentials.username().to_owned(),
            credentials.password().to_owned(),
        ))
    }
}

impl Registry {
    /// The loopback registry that holds `image`, `<authority>/...`: plain
    /// HTTP with no login.
    ///
    /// # Errors
    ///
    /// When the image names no loopback registry.
    pub fn loopback(image: &str) -> anyhow::Result<Self> {
        let authority = authority(image)?;
        ensure!(
            is_loopback(authority),
            "the image {image} is not in a loopback registry"
        );
        Ok(Self {
            authority: authority.to_owned(),
            auth_file: None,
            insecure: true,
            ca_paths: Vec::new(),
        })
    }
}

fn authority(image: &str) -> anyhow::Result<&str> {
    image
        .split_once('/')
        .map(|(authority, _)| authority)
        .with_context(|| format!("the image {image} names no registry"))
}

pub(crate) fn is_loopback(authority: &str) -> bool {
    matches!(authority.split(':').next(), Some("localhost" | "127.0.0.1"))
}

/// The registry client that reaches each of `registries` with its CAs, and
/// over plain HTTP where it is insecure.
fn client(registries: &[&Registry]) -> anyhow::Result<Client> {
    let mut certificates = Vec::new();
    for path in registries.iter().flat_map(|registry| &registry.ca_paths) {
        certificates.push(Certificate {
            encoding: CertificateEncoding::Pem,
            data: fs::read(path).with_context(|| format!("read CA {}", path.display()))?,
        });
    }
    let insecure = registries
        .iter()
        .filter(|registry| registry.insecure)
        .map(|registry| registry.authority.clone())
        .collect();
    // `TryFrom`, not `Client::new`, which drops a rejected trust root.
    Client::try_from(ClientConfig {
        protocol: ClientProtocol::HttpsExcept(insecure),
        extra_root_certificates: certificates,
        read_timeout: Some(Duration::from_secs(60)),
        connect_timeout: Some(Duration::from_secs(30)),
        ..ClientConfig::default()
    })
    .context("build the registry client")
}

/// The OCI media types of an image index and of an image manifest.
const MANIFEST_MEDIA_TYPES: [&str; 4] = [
    OCI_IMAGE_INDEX_MEDIA_TYPE,
    IMAGE_MANIFEST_LIST_MEDIA_TYPE,
    OCI_IMAGE_MEDIA_TYPE,
    IMAGE_MANIFEST_MEDIA_TYPE,
];

/// Write the image `image` (`<repository>[:<tag>]@sha256:<hex>`) of `from`
/// into the OCI archive `archive`: its manifest, every manifest an index
/// names, and their configs and layers, each by digest. The archive names
/// the image by its tag, when it has one, and by its pinned digest, because
/// CRI reports a repository digest only from a digest name.
///
/// # Errors
///
/// When the image has no digest, the registry refuses, a manifest or blob
/// does not hash to its digest, or the archive cannot be written.
pub async fn image_archive(image: &str, from: &Registry, archive: &Path) -> anyhow::Result<()> {
    let reference: Reference = image
        .parse()
        .with_context(|| format!("parse the image reference {image}"))?;
    let digest = reference
        .digest()
        .with_context(|| format!("image {image} is not pinned by digest"))?
        .to_owned();
    let client = client(&[from])?;
    let auth = from.auth()?;
    let mut blobs = std::collections::BTreeMap::new();
    let mut manifests = vec![digest.clone()];
    while let Some(manifest) = manifests.pop() {
        let pinned = Reference::with_digest(
            reference.registry().to_owned(),
            reference.repository().to_owned(),
            manifest.clone(),
        );
        let (bytes, _) = client
            .pull_manifest_raw(&pinned, &auth, &MANIFEST_MEDIA_TYPES)
            .await
            .with_context(|| format!("read the manifest {manifest} of {image}"))?;
        ensure!(
            wamn_digest::bytes_sha256(&bytes) == manifest,
            "the manifest {manifest} of {image} does not match its digest"
        );
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse the manifest {manifest} of {image}"))?;
        manifests.extend(descriptor_digests(&document["manifests"]));
        for blob in std::iter::once(&document["config"]["digest"])
            .filter_map(serde_json::Value::as_str)
            .chain(
                descriptor_digests(&document["layers"])
                    .iter()
                    .map(String::as_str),
            )
        {
            if blobs.contains_key(blob) {
                continue;
            }
            let mut data = Vec::new();
            client
                .pull_blob(&reference, blob, &mut data)
                .await
                .with_context(|| format!("read blob {blob} of {image}"))?;
            ensure!(
                wamn_digest::bytes_sha256(&data) == blob,
                "the blob {blob} of {image} does not match its digest"
            );
            blobs.insert(blob.to_owned(), data);
        }
        blobs.insert(manifest, bytes.to_vec());
    }
    let top = &blobs[&digest];
    let media_type = serde_json::from_slice::<serde_json::Value>(top)?["mediaType"]
        .as_str()
        .with_context(|| format!("the manifest of {image} names no media type"))?
        .to_owned();
    let index = archive_index(&reference, &media_type, top.len())?;
    let mut builder = tar::Builder::new(
        fs::File::create(archive)
            .with_context(|| format!("create the archive {}", archive.display()))?,
    );
    let entries = [
        (
            "oci-layout".to_owned(),
            br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
        ),
        ("index.json".to_owned(), serde_json::to_vec(&index)?),
    ]
    .into_iter()
    .chain(blobs.into_iter().map(|(digest, data)| {
        (
            format!("blobs/sha256/{}", digest.trim_start_matches("sha256:")),
            data,
        )
    }));
    for (path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(u64::try_from(data.len())?);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, &path, data.as_slice())
            .with_context(|| format!("write {path} to {}", archive.display()))?;
    }
    builder
        .into_inner()
        .with_context(|| format!("finish the archive {}", archive.display()))?;
    Ok(())
}

fn descriptor_digests(descriptors: &serde_json::Value) -> Vec<String> {
    descriptors
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|descriptor| descriptor["digest"].as_str().map(str::to_owned))
        .collect()
}

/// The `index.json` of the archive of `image`, pinned by digest, whose
/// manifest has `media_type` and `size`: one entry for each name, the tag
/// when the image has one and the pinned digest.
fn archive_index(
    image: &Reference,
    media_type: &str,
    size: usize,
) -> anyhow::Result<serde_json::Value> {
    let digest = image.digest().context("the image is pinned by digest")?;
    let repository = format!("{}/{}", image.registry(), image.repository());
    let mut names = vec![format!("{repository}@{digest}")];
    if let Some(tag) = image.tag() {
        names.insert(0, format!("{repository}:{tag}"));
    }
    Ok(serde_json::json!({
        "schemaVersion": 2,
        "manifests": names.iter().map(|name| serde_json::json!({
            "mediaType": media_type,
            "digest": digest,
            "size": size,
            "annotations": {"io.containerd.image.name": name},
        })).collect::<Vec<_>>(),
    }))
}

/// The host group body of the release chart: the one group of a rendered host
/// overlay, less what the chart derives.
///
/// # Errors
///
/// When the overlay does not have exactly one host group with an `env` list.
pub fn host_group(overlay: &str) -> anyhow::Result<Mapping> {
    let values: Value = serde_yaml::from_str(overlay).context("parse the host overlay")?;
    let groups = values["runtime"]["hostGroups"]
        .as_sequence()
        .context("the host overlay has host groups")?;
    ensure!(groups.len() == 1, "the host overlay has one host group");
    let mut group = groups[0]
        .as_mapping()
        .context("the host group is a mapping")?
        .clone();
    for key in DERIVED_KEYS {
        group.remove(key);
    }
    group
        .get_mut("env")
        .and_then(Value::as_sequence_mut)
        .context("the host group has env")?
        .retain(|entry| {
            entry["name"]
                .as_str()
                .is_none_or(|name| !DERIVED_VARIABLES.contains(&name))
        });
    Ok(group)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_group_keeps_only_what_the_chart_does_not_derive() {
        let group = host_group(
            "runtime: {hostGroups: [{name: default, namespace: n, replicas: 3, \
             extraArgs: [--x], env: [{name: WAMN_ORG, value: a}, {name: WAMN_PG_URL, value: u}, \
             {name: WAMN_DRAIN_BOUND_SECONDS, value: '70'}]}]}",
        )
        .unwrap();
        assert_eq!(
            Value::Mapping(group),
            serde_yaml::from_str::<Value>("{replicas: 3, env: [{name: WAMN_PG_URL, value: u}]}")
                .unwrap()
        );
        assert!(host_group("runtime: {hostGroups: []}").is_err());
    }

    /// The archive names a tagged image by its tag and its pinned digest,
    /// and an untagged one by its pinned digest alone.
    #[test]
    fn the_archive_names_the_tag_and_the_pinned_digest() {
        let digest = format!("sha256:{}", "b".repeat(64));
        let names = |image: String| {
            archive_index(&image.parse().unwrap(), OCI_IMAGE_MEDIA_TYPE, 7).unwrap()["manifests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    assert_eq!(entry["digest"], digest.as_str());
                    assert_eq!(entry["size"], 7);
                    entry["annotations"]["io.containerd.image.name"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(format!("registry.example/wamn/host:run@{digest}")),
            [
                "registry.example/wamn/host:run".to_owned(),
                format!("registry.example/wamn/host@{digest}"),
            ]
        );
        assert_eq!(
            names(format!("registry.example/wamn/guest@{digest}")),
            [format!("registry.example/wamn/guest@{digest}")]
        );
    }
}
