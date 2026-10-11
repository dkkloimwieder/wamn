//! One cluster case's environment on the release chart (wamn-ps2tj.1,
//! docs/plan/platform-deploy.md §9.2, R12, R15).
//!
//! The qualifying cases deploy the host and the roles the way `env apply`
//! does: a copy of `deploy/platform/release` stamped with the platform image
//! set, and the values `release_chart::values` renders. A case that qualifies
//! a candidate takes the role images from the candidate, copied by digest into
//! its own registry, so the qualification key names the bytes the case ran. A
//! run without a candidate pushes this checkout's role components there.

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
/// The candidate's registry is its owned native registry over plain HTTP, or
/// a registry this machine's Docker login reads.
///
/// # Errors
///
/// When an image cannot be copied.
pub async fn copy_roles(candidate: &Candidate, to: &Registry) -> anyhow::Result<(String, String)> {
    let (authority, _) = candidate
        .http_image
        .split_once('/')
        .context("the candidate's http image names its registry")?;
    let docker = std::env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".docker")))
        .map(|directory| directory.join("config.json"));
    let from = Registry {
        authority: authority.to_owned(),
        auth_file: docker
            .filter(|file| candidate.native_registry_endpoint.is_none() && file.is_file()),
        insecure: candidate.native_registry_insecure,
        ca_paths: Vec::new(),
    };
    Ok((
        copy_image(&candidate.http_image, &from, "wamn/http", to).await?,
        copy_image(
            &candidate.materializer_image,
            &from,
            "wamn/materializer",
            to,
        )
        .await?,
    ))
}

/// Push this checkout's http and materializer role components, built in
/// `target`, to `wamn/http` and `wamn/materializer` of `to` under `tag`, and
/// return each as `<authority>/<repository>@sha256:<hex>`. This is the role
/// build of a run that has no candidate; each push result is written to
/// `evidence` as `<role>-push.json`.
///
/// # Errors
///
/// When wash cannot be installed, or a push fails or returns no digest.
pub async fn push_roles(
    repository: &Path,
    target: &Path,
    to: &Registry,
    tag: &str,
    evidence: &Path,
) -> anyhow::Result<(String, String)> {
    let installed = tokio::process::Command::new(repository.join("tools/install-wash"))
        .kill_on_drop(true)
        .output()
        .await
        .context("run install-wash")?;
    ensure!(installed.status.success(), "install-wash failed");
    let wash = String::from_utf8(installed.stdout)?.trim().to_owned();
    ensure!(!wash.is_empty(), "install-wash returned no executable path");
    let mut pushed = Vec::new();
    for (file, role) in [
        ("http_route.wasm", "http"),
        ("materializer.wasm", "materializer"),
    ] {
        let reference = format!("{}/wamn/{role}:{tag}", to.authority);
        let mut push = tokio::process::Command::new(&wash);
        push.args(["-o", "json", "oci", "push"])
            .args(to.insecure.then_some("--insecure"))
            .arg(&reference)
            .arg(target.join("wasm32-wasip2/release").join(file))
            .kill_on_drop(true);
        if let Some(file) = &to.auth_file {
            push.env(
                "DOCKER_CONFIG",
                file.parent()
                    .context("the registry credential has a directory")?,
            );
        }
        let output = push.output().await.context("run wash oci push")?;
        ensure!(
            output.status.success(),
            "the {role} role push failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let body: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("read the role push result")?;
        let digest = body["data"]["digest"]
            .as_str()
            .filter(|digest| {
                body["success"] == true
                    && body["data"]["success"] == true
                    && digest.strip_prefix("sha256:").is_some_and(|hex| {
                        hex.len() == 64
                            && hex
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
            })
            .with_context(|| format!("the {role} role push returned no canonical digest"))?;
        fs::write(
            evidence.join(format!("{role}-push.json")),
            serde_json::to_vec_pretty(&body)?,
        )?;
        pushed.push(format!("{}/wamn/{role}@{digest}", to.authority));
    }
    let materializer = pushed.pop().context("the materializer image")?;
    let http = pushed.pop().context("the http image")?;
    Ok((http, materializer))
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
