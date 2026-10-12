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
use oci_client::manifest::{IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
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
const DERIVED_VARIABLES: [&str; 4] = [
    "WAMN_ORG",
    "WAMN_PROJECT",
    "WAMN_DRAIN_BOUND_SECONDS",
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
    /// The registry that holds `image`, `<authority>/...`, as this machine
    /// reaches it: plain HTTP for a loopback authority, and this machine's
    /// Docker login otherwise.
    ///
    /// # Errors
    ///
    /// When the image names no registry authority.
    pub fn of(image: &str) -> anyhow::Result<Self> {
        let (authority, _) = image
            .split_once('/')
            .with_context(|| format!("the image {image} names no registry"))?;
        let loopback = is_loopback(authority);
        let docker = std::env::var_os("DOCKER_CONFIG")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".docker")))
            .map(|directory| directory.join("config.json"));
        Ok(Self {
            authority: authority.to_owned(),
            auth_file: docker.filter(|file| !loopback && file.is_file()),
            insecure: loopback,
            ca_paths: Vec::new(),
        })
    }
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
    let mut certificates = Vec::new();
    for path in from.ca_paths.iter().chain(&to.ca_paths) {
        certificates.push(Certificate {
            encoding: CertificateEncoding::Pem,
            data: fs::read(path).with_context(|| format!("read CA {}", path.display()))?,
        });
    }
    let insecure = [from, to]
        .into_iter()
        .filter(|registry| registry.insecure)
        .map(|registry| registry.authority.clone())
        .collect();
    // `TryFrom`, not `Client::new`, which drops a rejected trust root.
    let client = Client::try_from(ClientConfig {
        protocol: ClientProtocol::HttpsExcept(insecure),
        extra_root_certificates: certificates,
        read_timeout: Some(Duration::from_secs(60)),
        connect_timeout: Some(Duration::from_secs(30)),
        ..ClientConfig::default()
    })
    .context("build the registry client")?;
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
        copy_image(&images.http, &Registry::of(&images.http)?, "wamn/http", to).await?,
        copy_image(
            &images.materializer,
            &Registry::of(&images.materializer)?,
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
    fs::write(
        &path,
        serde_yaml::to_string(&release_chart::values(input)?)?,
    )?;
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
}
