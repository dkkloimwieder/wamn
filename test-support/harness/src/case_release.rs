//! One cluster case's environment of the candidate release (wamn-ps2tj.1,
//! wamn-ps2tj.4, docs/plan/platform-deploy.md §8.2, §9.2, §10.2, R12, R15).
//!
//! A case installs the candidate's packages through the package stage of
//! `apply`, from the package repository beside the release repository, and
//! publishes nothing. It copies the release manifest, its components and the
//! candidate's role images by digest into its own registry, which its hosts
//! read, so the qualification key names the bytes the case ran. It deploys
//! the host and the roles the way `env apply` does: a copy of the release
//! chart stamped with the image set, and the values `release_chart::values`
//! renders.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::manifest::{
    IMAGE_MANIFEST_LIST_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_INDEX_MEDIA_TYPE,
    OCI_IMAGE_MEDIA_TYPE, OciImageManifest,
};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference, RegistryOperation};
use serde_yaml::{Mapping, Value};
use wamn_control::delivery::{Candidate, sibling_bases};
use wamn_control::environment::analyse::Installed;
use wamn_control::environment::document::{DeclaredRelease, EnvironmentDocument};
use wamn_control::environment::{Platform, stage};
use wamn_control::package_artifact::{OpenedPackage, PackageRegistry, open_package};
use wamn_control::release_chart::{self, ImageSet, Target, ValuesInput};
use wamn_control_registry::Triple;
use wamn_package_artifact::registry_credentials::read_registry_credentials;

/// The operator release's namespace, where every environment release and
/// its hosts live (R6).
pub const NAMESPACE: &str = "wamn-system";
/// The host group keys and variables that the release chart derives.
const DERIVED_KEYS: [&str; 5] = ["name", "namespace", "service", "extraArgs", "image"];
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
/// The collector of a kind cluster (`deploy/infra/otel-collector.yaml`).
const KIND_OTLP_ENDPOINT: &str = "http://otel-collector.wamn-system.svc.cluster.local:4317";

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

    /// The registry that holds `image` of `candidate`: plain HTTP with no
    /// login for a loopback authority, and otherwise the access of the
    /// candidate's release registry, the pull credential and CAs that
    /// `qualify-release` names and mounts in the runner (wamn-5v6ps).
    ///
    /// # Errors
    ///
    /// When the image or the release repository names no registry authority.
    pub fn of(image: &str, candidate: &Candidate) -> anyhow::Result<Self> {
        let authority = authority(image)?;
        if is_loopback(authority) {
            return Self::loopback(image);
        }
        let releases = Self::releases(candidate)?;
        if releases.authority == authority {
            return Ok(releases);
        }
        Ok(Self {
            authority: authority.to_owned(),
            insecure: false,
            ..releases
        })
    }
}

fn authority(image: &str) -> anyhow::Result<&str> {
    image
        .split_once('/')
        .map(|(authority, _)| authority)
        .with_context(|| format!("the image {image} names no registry"))
}

impl Registry {
    /// The registry of the candidate's release repository, with its access.
    ///
    /// # Errors
    ///
    /// When the release repository names no registry authority.
    pub fn releases(candidate: &Candidate) -> anyhow::Result<Self> {
        let releases = &candidate.releases;
        let (authority, _) = releases
            .artifact_base
            .split_once('/')
            .context("the release repository names a registry")?;
        Ok(Self {
            authority: authority.to_owned(),
            auth_file: Some(releases.registry_auth_file.clone()),
            insecure: releases.insecure_registry,
            ca_paths: releases.oci_ca_paths.clone(),
        })
    }
}

fn is_loopback(authority: &str) -> bool {
    matches!(authority.split(':').next(), Some("localhost" | "127.0.0.1"))
}

/// The `<container>:<port>` at which kind nodes reach a loopback registry
/// `authority`, `localhost:<port>`: the container that publishes the port,
/// on the kind network. `None` for any other authority, which the nodes reach
/// as it is.
///
/// # Errors
///
/// When no container publishes the port, or it is not on the kind network.
pub async fn registry_endpoint(authority: &str) -> anyhow::Result<Option<String>> {
    if !is_loopback(authority) {
        return Ok(None);
    }
    let port = authority
        .rsplit_once(':')
        .map(|(_, port)| port)
        .with_context(|| format!("the loopback registry {authority} names no port"))?;
    let names = docker(&[
        "container",
        "ls",
        "--filter",
        &format!("publish={port}"),
        "--format",
        "{{.Names}}",
    ])
    .await?;
    let name = names
        .lines()
        .next()
        .with_context(|| format!("no container publishes the registry port {port}"))?;
    let settings: serde_json::Value = serde_json::from_str(
        &docker(&[
            "container",
            "inspect",
            "--format",
            "{{json .NetworkSettings}}",
            name,
        ])
        .await?,
    )?;
    ensure!(
        settings["Networks"].get("kind").is_some(),
        "the registry container {name} is not on the kind network"
    );
    let inner = settings["Ports"]
        .as_object()
        .and_then(|ports| {
            ports.iter().find_map(|(inner, bindings)| {
                bindings
                    .as_array()?
                    .iter()
                    .any(|binding| binding["HostPort"] == port)
                    .then(|| inner.trim_end_matches("/tcp").to_owned())
            })
        })
        .with_context(|| format!("the container {name} publishes no port {port}"))?;
    Ok(Some(format!("{name}:{inner}")))
}

/// The files that a cluster's lifecycle tool reads to pull `host_image` onto
/// its nodes from the loopback registry that kind reaches at `endpoint`:
/// `native-registry-authority`, `native-registry-hosts.toml` and
/// `native-host-image`, in `work`.
///
/// # Errors
///
/// When the image names no registry or a file cannot be written.
pub fn registry_files(endpoint: &str, host_image: &str, work: &Path) -> anyhow::Result<()> {
    let (authority, _) = host_image
        .split_once('/')
        .context("the host image has a registry authority")?;
    let endpoint = serde_json::to_string(&format!("http://{endpoint}"))?;
    fs::write(work.join("native-registry-authority"), authority)?;
    fs::write(
        work.join("native-registry-hosts.toml"),
        format!(
            "server = {endpoint}\n[host.{endpoint}]\n  capabilities = [\"pull\", \"resolve\"]\n"
        ),
    )?;
    fs::write(work.join("native-host-image"), host_image)?;
    Ok(())
}

async fn docker(arguments: &[&str]) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("docker")
        .args(arguments)
        .kill_on_drop(true)
        .output()
        .await
        .context("run docker")?;
    ensure!(
        output.status.success(),
        "docker {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

/// Copy the image `source` (`<authority>/<repository>@sha256:<hex>`) from
/// `from` into `repository` of `to`, by digest, and return
/// `<to authority>/<repository>@sha256:<hex>`. The manifest bytes are copied
/// unchanged, so the digest is the source's.
///
/// # Errors
///
/// When the source has no digest, a registry refuses, or the bytes do not
/// hash to the digest.
pub async fn copy_image(
    source: &str,
    from: &Registry,
    repository: &str,
    to: &Registry,
) -> anyhow::Result<String> {
    let source_reference: Reference = source
        .parse()
        .with_context(|| format!("parse the image reference {source}"))?;
    let digest = source_reference
        .digest()
        .with_context(|| format!("image {source} is not pinned by digest"))?
        .to_owned();
    let target =
        Reference::with_digest(to.authority.clone(), repository.to_owned(), digest.clone());
    let copied = copy(&source_reference, from, &target, to).await?;
    ensure!(
        copied == digest,
        "the manifest of {source} does not hash to its digest"
    );
    Ok(format!("{}/{repository}@{digest}", to.authority))
}

/// Copy the manifest `source` of `from`, its config and its layers, to
/// `target` of `to`, and return the manifest's digest. The manifest bytes are
/// copied unchanged.
async fn copy(
    source: &Reference,
    from: &Registry,
    target: &Reference,
    to: &Registry,
) -> anyhow::Result<String> {
    let client = client(&[from, to])?;
    let (bytes, found) = client
        .pull_manifest_raw(
            source,
            &from.auth()?,
            &[OCI_IMAGE_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE],
        )
        .await
        .with_context(|| format!("read the manifest of {source}"))?;
    ensure!(
        wamn_package_artifact::sha256_digest(&bytes) == found,
        "the manifest of {source} does not hash to the digest the registry names"
    );
    let manifest: OciImageManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse the manifest of {source}"))?;
    client
        .auth(target, &to.auth()?, RegistryOperation::Push)
        .await
        .with_context(|| format!("log in to registry {}", to.authority))?;
    for blob in std::iter::once(&manifest.config).chain(&manifest.layers) {
        let mut data = Vec::new();
        client
            .pull_blob(source, blob, &mut data)
            .await
            .with_context(|| format!("read blob {} of {source}", blob.digest))?;
        client
            .push_blob(target, data, &blob.digest)
            .await
            .with_context(|| format!("write blob {} to {}", blob.digest, to.authority))?;
    }
    let media_type = manifest
        .media_type
        .as_deref()
        .unwrap_or(OCI_IMAGE_MEDIA_TYPE);
    client
        .push_manifest_raw(target, bytes, media_type.parse()?)
        .await
        .with_context(|| format!("write the manifest of {source} to {}", to.authority))?;
    Ok(found)
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

/// The archive in `work` of the image `<repository>[:<tag>]@sha256:<hex>`
/// that a cluster's lifecycle tool imports on each node:
/// `node-image-<hex>.tar`.
fn node_image_archive(work: &Path, image: &str) -> anyhow::Result<PathBuf> {
    let (_, hex) = image
        .rsplit_once("@sha256:")
        .with_context(|| format!("the image {image} is not pinned by digest"))?;
    Ok(work.join(format!("node-image-{hex}.tar")))
}

/// Write each of `images`, pinned by digest in a registry that is not
/// loopback, into its archive in `work` ([`node_image_archive`]), which the
/// cluster's lifecycle tool imports on each node. The registry stays
/// private: the archives are read with the access of [`Registry::of`], and
/// no node gets a credential (docs/operations/gcp.md §3.13).
///
/// # Errors
///
/// When an image cannot be read or an archive cannot be written.
pub async fn node_image_archives(
    candidate: &Candidate,
    images: &[&str],
    work: &Path,
) -> anyhow::Result<()> {
    for image in images {
        image_archive(
            image,
            &Registry::of(image, candidate)?,
            &node_image_archive(work, image)?,
        )
        .await?;
    }
    Ok(())
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
            wamn_package_artifact::sha256_digest(&bytes) == manifest,
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
                wamn_package_artifact::sha256_digest(&data) == blob,
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

/// Copy the candidate's release manifest and every component it names from
/// the release repository and the component repository beside it into
/// `wamn/releases` and `wamn/components` of `to`, each under the tag of its
/// digest, the references the hosts pull.
///
/// # Errors
///
/// When an artifact cannot be copied.
pub async fn copy_release(candidate: &Candidate, to: &Registry) -> anyhow::Result<()> {
    let from = Registry::releases(candidate)?;
    let (_, components) = sibling_bases(&candidate.releases.artifact_base)?;
    let (manifest, digest) = candidate.manifest()?;
    let artifacts = std::iter::once((
        candidate.releases.artifact_base.as_str(),
        "releases",
        digest.as_str(),
    ))
    .chain(manifest.components.iter().map(|component| {
        (
            components.as_str(),
            "components",
            component.descriptor.component_digest.as_str(),
        )
    }));
    for (base, repository, digest) in artifacts {
        let source = tagged(base, digest)?;
        let target = tagged(&format!("{}/wamn/{repository}", to.authority), digest)?;
        copy(&source, &from, &target, to).await?;
    }
    Ok(())
}

/// The reference `<base>:<hex>` of the artifact that `digest` addresses.
fn tagged(base: &str, digest: &str) -> anyhow::Result<Reference> {
    let reference = wamn_catalog::component_artifact_reference(base, digest)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(Reference::with_tag(
        reference.registry().to_owned(),
        reference.repository().to_owned(),
        reference.tag().to_owned(),
    ))
}

/// The registry of the candidate's packages: the package repository beside
/// the release repository.
fn package_registry(candidate: &Candidate) -> anyhow::Result<PackageRegistry> {
    let (packages, _) = sibling_bases(&candidate.releases.artifact_base)?;
    Ok(PackageRegistry {
        artifact_base: packages,
        ..candidate.releases.clone()
    })
}

/// Open each package of the candidate's release from its artifact.
///
/// # Errors
///
/// When an artifact cannot be read.
pub async fn open_packages(candidate: &Candidate) -> anyhow::Result<Vec<OpenedPackage>> {
    let registry = package_registry(candidate)?;
    let (manifest, _) = candidate.manifest()?;
    let mut opened = Vec::new();
    for package in &manifest.release.packages {
        opened.push(
            open_package(&registry, package.package_id(), package.package_version())
                .await
                .with_context(|| {
                    format!(
                        "open package artifact {}@{}",
                        package.package_id(),
                        package.package_version()
                    )
                })?,
        );
    }
    Ok(opened)
}

/// Install the candidate's packages in the project database of `triple` as
/// a fresh install through the package stage of `apply` (§10.2, R17 (2)):
/// each package from its artifact, then the release cache row, the release's
/// connection requirements and the packages' data access.
///
/// # Errors
///
/// When the stage refuses or fails.
pub async fn install_candidate(
    candidate: &Candidate,
    system_database_url: &str,
    project_database_url: &str,
    triple: &Triple,
) -> anyhow::Result<()> {
    let (manifest, digest) = candidate.manifest()?;
    let registry = package_registry(candidate)?;
    // The stage reads the system database and the package registry only.
    let platform = Platform {
        auth_nats: None,
        system_database_url: system_database_url.to_owned(),
        target: Target {
            kubeconfig: PathBuf::new(),
            context: String::new(),
            namespace: NAMESPACE.to_owned(),
        },
        chart: PathBuf::new(),
        release_artifact_base: candidate.releases.artifact_base.clone(),
        registry_auth_file: registry.registry_auth_file,
        oci_ca_paths: registry.oci_ca_paths,
        package_artifact_base: Some(registry.artifact_base),
        database_namespace: NAMESPACE.to_owned(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_control::pat_client::PatIssuerConfig::default(),
        events: None,
    };
    let document = EnvironmentDocument {
        org: triple.org.clone(),
        project: triple.project.clone(),
        env: triple.env.as_str().to_owned(),
        release: DeclaredRelease::Digest(digest.as_str().to_owned()),
        route_host: String::new(),
        client_package: None,
        policy: String::new(),
        connections: std::collections::BTreeMap::new(),
        floors: std::collections::BTreeMap::new(),
    };
    stage::stage(
        &platform,
        &document,
        &std::collections::BTreeMap::<String, Installed>::new(),
        digest.as_str(),
        &manifest,
        project_database_url,
    )
    .await
    .context("install the candidate's packages through the package stage")?;
    Ok(())
}

/// Copy the candidate's http and materializer images by digest into
/// `wamn/http` and `wamn/materializer` of `to`, and return the copies.
///
/// # Errors
///
/// When an image cannot be copied.
pub async fn copy_roles(candidate: &Candidate, to: &Registry) -> anyhow::Result<(String, String)> {
    let images = &candidate.images;
    Ok((
        copy_image(
            &images.http,
            &Registry::of(&images.http, candidate)?,
            "wamn/http",
            to,
        )
        .await?,
        copy_image(
            &images.materializer,
            &Registry::of(&images.materializer, candidate)?,
            "wamn/materializer",
            to,
        )
        .await?,
    ))
}

/// The release chart that `WAMN_RELEASE_CHART` names, stamped by
/// `wamn-ctl build-platform` (docs/plan/platform-deploy.md §9.3).
///
/// # Errors
///
/// When the variable is not set.
pub fn release_chart() -> anyhow::Result<PathBuf> {
    std::env::var_os("WAMN_RELEASE_CHART")
        .map(PathBuf::from)
        .context("WAMN_RELEASE_CHART names the release chart that build-platform stamped")
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

/// One cluster a case installs its environment release into.
#[derive(Debug)]
pub struct Install<'a> {
    pub cluster: &'a str,
    /// The directory that holds `kubeconfig`, and the stamped chart and values.
    pub work: &'a Path,
}

/// Stamp the release chart that `WAMN_RELEASE_CHART` names with `images`,
/// render the values of `input`, and install the release in the operator
/// namespace, as `env apply` does (R6).
/// Returns the release name, which is the host group, and the `wamn.release`
/// label of the manifest digest, which the roles' `hostSelector` names.
///
/// # Errors
///
/// When `WAMN_RELEASE_CHART` is not set, the chart cannot be stamped, the
/// values do not render, or Helm fails.
pub async fn install(
    at: &Install<'_>,
    images: &ImageSet,
    input: &ValuesInput,
) -> anyhow::Result<(String, String)> {
    let label = wamn_engine::release_manifest::release_label(&input.manifest_digest)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let chart = release_chart::stamp(
        &release_chart()?,
        images,
        None,
        &at.work.join("release-chart"),
    )?;
    let name = release_chart::release_name(&input.org, &input.project, &input.env)?;
    let path = at.work.join(format!("values-{name}.yaml"));
    fs::write(&path, serde_yaml::to_string(&case_values(input)?)?)?;
    release_chart::upgrade(
        &Target {
            kubeconfig: at.work.join("kubeconfig"),
            context: format!("kind-{}", at.cluster),
            namespace: NAMESPACE.to_owned(),
        },
        &chart,
        &name,
        &path,
        Duration::from_secs(300),
        "cluster case",
    )
    .await?;
    Ok((name, label))
}

/// The values of `input` for the hosts of a case, which read the case's own
/// plain HTTP registry: `release_chart::values`, and
/// `--allow-insecure-registries` after the release arguments. The release
/// chart reads a registry over TLS only; without the flag every host exits at
/// start, when it cannot read the release manifest (wamn-k8127.6). The hosts
/// export to the kind cluster's own collector.
fn case_values(input: &ValuesInput) -> anyhow::Result<Value> {
    let mut values = release_chart::values(input)?;
    let group = &mut values["runtime-operator"]["runtime"]["hostGroups"][0];
    group["extraArgs"]
        .as_sequence_mut()
        .context("the release chart renders the host group's arguments")?
        .push("--allow-insecure-registries".into());
    let endpoint = group["env"]
        .as_sequence_mut()
        .context("the release chart renders the host group's variables")?
        .iter_mut()
        .find(|variable| variable["name"] == "OTEL_EXPORTER_OTLP_ENDPOINT")
        .context("the release chart renders the OTLP endpoint")?;
    endpoint["value"] = KIND_OTLP_ENDPOINT.into();
    Ok(values)
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

    /// A case reads a private image with the release registry's pull
    /// credential, never a Docker login of the machine (wamn-5v6ps).
    #[test]
    fn a_private_image_is_read_with_the_release_registry_access() {
        let image = |authority: &str| format!("{authority}/wamn/http@sha256:{}", "a".repeat(64));
        let candidate = Candidate {
            manifest_path: PathBuf::from("/manifest.json"),
            releases: PackageRegistry {
                artifact_base: "registry.example/acme/releases".to_owned(),
                registry_auth_file: PathBuf::from("/pull/config.json"),
                insecure_registry: true,
                oci_ca_paths: vec![PathBuf::from("/pull/ca.crt")],
            },
            images: ImageSet {
                host: image("registry.example"),
                http: image("registry.example"),
                materializer: image("registry.example"),
            },
            qualification: wamn_control::release_chart::QualificationImages {
                runner: image("registry.example"),
                gates: image("registry.example"),
                identity: image("registry.example"),
            },
        };
        let same = Registry::of(&image("registry.example"), &candidate).unwrap();
        assert_eq!(same.auth_file, Some(PathBuf::from("/pull/config.json")));
        assert!(same.insecure);
        let other = Registry::of(&image("other.example"), &candidate).unwrap();
        assert_eq!(other.authority, "other.example");
        assert_eq!(other.auth_file, Some(PathBuf::from("/pull/config.json")));
        assert_eq!(other.ca_paths, [PathBuf::from("/pull/ca.crt")]);
        assert!(!other.insecure);
        let loopback = Registry::of(&image("localhost:5000"), &candidate).unwrap();
        assert_eq!(loopback.auth_file, None);
        assert!(loopback.insecure);
        assert!(Registry::loopback(&image("registry.example")).is_err());
        assert_eq!(
            node_image_archive(Path::new("/work"), &image("registry.example")).unwrap(),
            Path::new("/work").join(format!("node-image-{}.tar", "a".repeat(64)))
        );
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

    /// The case's hosts read its plain HTTP registry (wamn-k8127.6).
    #[test]
    fn the_case_hosts_read_a_plain_http_registry() {
        let values = case_values(&ValuesInput {
            org: "acme".to_owned(),
            project: "wms".to_owned(),
            env: "dev".to_owned(),
            manifest_digest: format!("sha256:{}", "e".repeat(64)),
            artifact_base: "172.18.0.5:5000/wamn/releases".to_owned(),
            route_host: "wms.example".to_owned(),
            roles: Vec::new(),
            drain_bound_seconds: 70,
            actor: "case".to_owned(),
            host_group: Mapping::new(),
        })
        .unwrap();
        let arguments = values["runtime-operator"]["runtime"]["hostGroups"][0]["extraArgs"]
            .as_sequence()
            .unwrap();
        assert_eq!(
            arguments.last().and_then(Value::as_str),
            Some("--allow-insecure-registries")
        );
        assert_eq!(arguments.len(), 4);
    }
}
