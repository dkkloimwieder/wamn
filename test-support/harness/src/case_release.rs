//! One cluster case's environment on the release chart (wamn-ps2tj.1,
//! docs/plan/platform-deploy.md §9.2, R12, R15).
//!
//! The qualifying cases deploy the host and the roles the way `env apply`
//! does: a copy of `deploy/platform/release` stamped with the platform image
//! set, and the values `release_chart::values` renders. A case runs only with
//! a candidate and takes the role images from it, copied by digest into its
//! own registry, so the qualification key names the bytes the case ran (R15).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::manifest::{IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference, RegistryOperation};
use serde_yaml::{Mapping, Value};
use wamn_control::delivery::Candidate;
use wamn_control::release_chart::{self, ImageSet, Target, ValuesInput};
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
            &source_reference,
            &from.auth()?,
            &[OCI_IMAGE_MEDIA_TYPE, IMAGE_MANIFEST_MEDIA_TYPE],
        )
        .await
        .with_context(|| format!("read the manifest of {source}"))?;
    ensure!(
        found == digest && wamn_package_artifact::sha256_digest(&bytes) == digest,
        "the manifest of {source} does not hash to its digest"
    );
    let manifest: OciImageManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse the manifest of {source}"))?;
    let target =
        Reference::with_digest(to.authority.clone(), repository.to_owned(), digest.clone());
    client
        .auth(&target, &to.auth()?, RegistryOperation::Push)
        .await
        .with_context(|| format!("log in to registry {}", to.authority))?;
    for blob in std::iter::once(&manifest.config).chain(&manifest.layers) {
        let mut data = Vec::new();
        client
            .pull_blob(&source_reference, blob, &mut data)
            .await
            .with_context(|| format!("read blob {} of {source}", blob.digest))?;
        client
            .push_blob(&target, data, &blob.digest)
            .await
            .with_context(|| format!("write blob {} to {}", blob.digest, to.authority))?;
    }
    let media_type = manifest
        .media_type
        .as_deref()
        .unwrap_or(OCI_IMAGE_MEDIA_TYPE);
    client
        .push_manifest_raw(&target, bytes, media_type.parse()?)
        .await
        .with_context(|| format!("write the manifest of {source} to {}", to.authority))?;
    Ok(format!("{}/{repository}@{digest}", to.authority))
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
    pub repository: &'a Path,
    pub cluster: &'a str,
    /// The directory that holds `kubeconfig`, and the stamped chart and values.
    pub work: &'a Path,
}

/// Stamp the release chart with `images`, render the values of `input`, and
/// install the release in the operator namespace, as `env apply` does (R6).
/// Returns the release name, which is the host group, and the `wamn.release`
/// label of the manifest digest, which the roles' `hostSelector` names.
///
/// # Errors
///
/// When the chart cannot be stamped, the values do not render, or Helm fails.
pub async fn install(
    at: &Install<'_>,
    images: &ImageSet,
    input: &ValuesInput,
) -> anyhow::Result<(String, String)> {
    let label = wamn_engine::release_manifest::release_label(&input.manifest_digest)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let chart = release_chart::stamp(
        &at.repository.join("deploy/platform/release"),
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
