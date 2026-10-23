//! The platform build step (docs/plan/platform-deploy.md §9.3, R15).
//!
//! It builds the host, runner, gates, identity and edge images and the two role
//! components from the root `Dockerfile`. Only when every build succeeded
//! does it push them to one registry. It then stamps their digests into a
//! copy of the release chart's defaults. That chart is the one source of an
//! image set.
//!
//! A reinstall at an unchanged platform version builds nothing: the stamp-only
//! step reads the digests of the chart version's tags from the registry and
//! stamps those (owner ruling R15, 2026-10-14). Both modes record the digest
//! of the local platform UI in `platform-build.json` beside the chart defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, ensure};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use wamn_execution_contract::canonical_json_bytes;

use crate::release_chart::{CHART_VERSION, ImageSet, QualificationImages, stamp};

mod inputs;

/// The Dockerfile targets that build-platform pushes as images.
const IMAGES: [&str; 5] = ["host", "runner", "gates", "identity", "edge-web"];

/// The files of the roles target, each with the role it is pushed as.
const ROLES: [(&str, &str); 2] = [
    ("http_route.wasm", "http"),
    ("materializer.wasm", "materializer"),
];

/// Print the elapsed time when a build stage ends, including on failure.
pub(crate) struct Stage {
    name: String,
    start: Instant,
}

pub(crate) fn start_stage(name: impl Into<String>) -> Stage {
    let name = name.into();
    eprintln!(
        "build-platform {name} start {}",
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    Stage {
        name,
        start: Instant::now(),
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        eprintln!(
            "build-platform {} end {} elapsed {:.3}s",
            self.name,
            Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            self.start.elapsed().as_secs_f64()
        );
    }
}

/// The content of the platform UI produced by one platform build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformBuildReceipt {
    /// SHA-256 of the canonical map of relative file paths to file digests.
    pub platform_ui: String,
}

/// The platform build record beside the stamped chart's defaults.
pub const RECEIPT_FILE: &str = "platform-build.json";

/// The local static bundle, independent of package build output.
pub fn platform_ui_dist(repository: &Path) -> PathBuf {
    repository.join("apps/platform_ui/dist")
}

fn ui_receipt(repository: &Path) -> anyhow::Result<PlatformBuildReceipt> {
    let dist = platform_ui_dist(repository);
    ensure!(
        dist.is_dir(),
        "the platform UI build {} is absent; run `pnpm --filter @wamn/platform-ui build`",
        dist.display()
    );
    Ok(PlatformBuildReceipt {
        platform_ui: platform_ui_digest(&dist)?,
    })
}

/// Digest the built platform UI, including its index and every asset.
pub fn platform_ui_digest(dist: &Path) -> anyhow::Result<String> {
    let mut files = Vec::new();
    crate::web_upload::collect(dist, &mut files)?;
    ensure!(
        files.iter().any(|file| file == &dist.join("index.html")),
        "the platform UI build {} has no index.html",
        dist.display()
    );
    let mut digests = BTreeMap::new();
    for file in files {
        let relative = file
            .strip_prefix(dist)?
            .to_str()
            .context("a platform UI file name is UTF-8")?
            .to_owned();
        digests.insert(relative, sha256(&std::fs::read(file)?));
    }
    Ok(sha256(&canonical_json_bytes(&serde_json::to_value(
        digests,
    )?)))
}

fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
    )
}

/// The first 16 SHA-256 hex digits of the checkout's canonical absolute path.
fn checkout_key(repository: &Path) -> anyhow::Result<String> {
    let repository =
        std::fs::canonicalize(repository).context("find the checkout for its cache")?;
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        repository.as_os_str().as_encoded_bytes(),
    );
    Ok(hex::encode(digest.as_ref())[..16].to_owned())
}

/// The inputs of one platform build.
#[derive(Debug, Clone)]
pub struct BuildPlatformRequest {
    /// The checkout whose `Dockerfile` and release chart are built.
    pub repository: PathBuf,
    /// `<host>[:<port>]` of the registry the images are pushed to. A loopback
    /// registry is reached over plain HTTP.
    pub registry: String,
    /// The directory the stamped chart is written to. It must not exist.
    pub out: PathBuf,
    /// Optional cluster inputs for cloud builds.
    pub cluster: Option<crate::platform::document::ClusterDocument>,
    /// Use Cloud Build even when no other build runs, under the same daily cap.
    pub cloud: bool,
}

/// Where a build can run given other build processes and today's spend.
#[derive(Debug, PartialEq)]
enum BuildLocation {
    Local,
    Cloud,
    Refused { spend_usd: f64, cap_usd: f64 },
}

fn build_location(
    processes: &[(u32, Vec<String>)],
    pid: u32,
    cloud: bool,
    spend_usd: f64,
    cap_usd: f64,
) -> BuildLocation {
    let busy = cloud
        || processes
            .iter()
            .any(|(other_pid, args)| *other_pid != pid && builds_platform(args));
    if !busy {
        BuildLocation::Local
    } else if spend_usd < cap_usd {
        BuildLocation::Cloud
    } else {
        BuildLocation::Refused { spend_usd, cap_usd }
    }
}

fn builds_platform(args: &[String]) -> bool {
    let Some(program) = args.first() else {
        return false;
    };
    if Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("wamn-ctl")
    {
        return false;
    }
    // Skip the CLI's global log option before finding its subcommand.
    let mut arguments = args[1..].iter();
    while let Some(argument) = arguments.next() {
        if argument == "--log-level" {
            arguments.next();
        } else if !argument.starts_with("--log-level=") {
            // Neither a stamp nor a cloud build runs Docker on this machine.
            return argument == "build-platform"
                && !arguments.any(|argument| argument == "--stamp-only" || argument == "--cloud");
        }
    }
    false
}

/// Build every platform image and role component, then push them, then stamp
/// them into a copy of the release chart at `request.out`. A failed build
/// pushes nothing. Every image is tagged with the chart version and pinned by
/// its pushed digest.
pub async fn build_platform(request: &BuildPlatformRequest) -> anyhow::Result<PathBuf> {
    ensure!(
        !request.out.exists(),
        "the chart output {} exists",
        request.out.display()
    );
    let apps_target = request.repository.join("apps/target/wamn");
    ensure!(
        apps_target.is_dir(),
        "{} does not exist; run `wamn build` on the apps first",
        apps_target.display()
    );
    let processes = process_list()?;
    if build_location(&processes, std::process::id(), request.cloud, 0.0, 3.0)
        != BuildLocation::Local
    {
        return cloud_build(request, &processes).await;
    }
    let roles = std::env::temp_dir().join(format!("wamn-roles-{}", std::process::id()));
    let mut components = OciComponents {
        insecure: loopback(&request.registry),
    };
    let pushed = build_then_push(request, &roles, output, &mut components).await;
    let _ = std::fs::remove_dir_all(&roles);
    let pushed = pushed?;
    stamp_chart(request, pushed, &ui_receipt(&request.repository)?)
}

/// Stamp the registry's images and role components of [`CHART_VERSION`] into
/// a copy of the release chart at `request.out`. It builds and pushes
/// nothing; when any of the seven is missing, it refuses and names them.
pub async fn stamp_only(request: &BuildPlatformRequest) -> anyhow::Result<PathBuf> {
    let receipt = ui_receipt(&request.repository)?;
    let insecure = loopback(&request.registry);
    stamp_from_registry(request, &receipt, output, async |reference: &str| {
        let (_, digest) = wash_runtime::oci::pull_component(
            reference,
            wash_runtime::oci::OciConfig {
                insecure,
                ..wash_runtime::oci::OciConfig::default()
            },
            wash_runtime::oci::OciPullPolicy::Always,
        )
        .await?;
        Ok(digest)
    })
    .await
}

/// Resolve the six tags of [`CHART_VERSION`]: the images through `run`'s
/// `docker buildx imagetools inspect`, the role components through `pull`,
/// which returns the manifest digest. Then stamp them.
async fn stamp_from_registry(
    request: &BuildPlatformRequest,
    receipt: &PlatformBuildReceipt,
    mut run: impl AsyncFnMut(&mut Command) -> anyhow::Result<String>,
    mut pull: impl AsyncFnMut(&str) -> anyhow::Result<String>,
) -> anyhow::Result<PathBuf> {
    ensure!(
        !request.out.exists(),
        "the chart output {} exists",
        request.out.display()
    );
    let stage = start_stage("registry-reads");
    let registry = &request.registry;
    let mut pinned = Vec::new();
    let mut missing = Vec::new();
    for target in ["host", "runner", "gates", "identity", "edge-web"] {
        let reference = format!("{registry}/wamn-{target}:{CHART_VERSION}");
        let digest = run(Command::new("docker").args([
            "buildx",
            "imagetools",
            "inspect",
            "--format",
            "{{json .Manifest}}",
            &reference,
        ]))
        .await
        .and_then(|json| manifest_digest(&json));
        match digest {
            Ok(digest) => pinned.push(format!("{reference}@{digest}")),
            Err(error) => missing.push(format!("{reference} ({error:#})")),
        }
    }
    for role in ["http", "materializer"] {
        let reference = format!("{registry}/wamn/{role}:{CHART_VERSION}");
        match pull(&reference).await {
            Ok(digest) if digest.starts_with("sha256:") => {
                pinned.push(format!("{registry}/wamn/{role}@{digest}"));
            }
            Ok(digest) => missing.push(format!("{reference} (digest {digest})")),
            Err(error) => missing.push(format!("{reference} ({error:#})")),
        }
    }
    ensure!(
        missing.is_empty(),
        "the registry does not hold {} of the {CHART_VERSION} images: {}",
        missing.len(),
        missing.join("; ")
    );
    let [host, runner, gates, identity, edge, http, materializer] = <[String; 7]>::try_from(pinned)
        .map_err(|_| anyhow::anyhow!("seven images were resolved"))?;
    drop(stage);
    stamp_chart(
        request,
        ([host, runner, gates, identity, edge], (http, materializer)),
        receipt,
    )
}

/// The `sha256:<hex>` digest of the manifest descriptor `json`.
fn manifest_digest(json: &str) -> anyhow::Result<String> {
    let descriptor: serde_json::Value =
        serde_json::from_str(json).context("parse the manifest descriptor")?;
    descriptor["digest"]
        .as_str()
        .filter(|digest| digest.starts_with("sha256:"))
        .map(str::to_owned)
        .context("the manifest descriptor has no sha256 digest")
}

/// Stamp the host, runner, gates, identity and edge images and the http and
/// materializer components into a copy of the checkout's release chart.
fn stamp_chart(
    request: &BuildPlatformRequest,
    ([host, runner, gates, identity, edge], (http, materializer)): ([String; 5], (String, String)),
    receipt: &PlatformBuildReceipt,
) -> anyhow::Result<PathBuf> {
    let _stage = start_stage("stamp");
    let chart = stamp(
        &request.repository.join("deploy/platform/release"),
        &ImageSet {
            host,
            http,
            materializer,
        },
        Some(&QualificationImages {
            runner,
            gates,
            identity,
        }),
        &request.out,
    )?;
    let path = chart.join("values.yaml");
    let mut defaults: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&path)?)?;
    ensure!(
        edge.contains("@sha256:"),
        "the edge image is not pinned by digest"
    );
    defaults["platform"]["edge"]["image"] = edge.into();
    std::fs::write(path, serde_yaml::to_string(&defaults)?)?;
    std::fs::write(
        chart.join(RECEIPT_FILE),
        canonical_json_bytes(&serde_json::to_value(receipt)?),
    )?;
    Ok(chart)
}

/// A loopback registry is reached over plain HTTP.
fn loopback(registry: &str) -> bool {
    let host = registry.split(':').next().unwrap_or_default();
    matches!(host, "localhost" | "127.0.0.1")
}

/// The role component registry: the registry over OCI, or a fake in tests.
trait Components {
    /// Push `bytes` as a new component and return its manifest digest.
    async fn push(&mut self, reference: &str, bytes: &[u8]) -> anyhow::Result<String>;
    /// Every tag of `repository`.
    async fn tags(&mut self, repository: &str) -> anyhow::Result<Vec<String>>;
    /// The raw manifest of `reference` and its media type.
    async fn manifest(&mut self, reference: &str) -> anyhow::Result<(Vec<u8>, String)>;
    /// Store the raw `manifest` under `reference`, byte for byte.
    async fn put_manifest(
        &mut self,
        reference: &str,
        manifest: &[u8],
        media_type: &str,
    ) -> anyhow::Result<()>;
}

/// The role components of the registry, over OCI.
struct OciComponents {
    insecure: bool,
}

impl OciComponents {
    /// A client for `reference` with the credential that the Docker
    /// configuration, or its credential helper, holds for its registry.
    fn client(
        &self,
        reference: &str,
    ) -> anyhow::Result<(
        oci_client::Client,
        oci_client::Reference,
        oci_client::secrets::RegistryAuth,
    )> {
        use docker_credential::{CredentialRetrievalError, DockerCredential};
        use oci_client::secrets::RegistryAuth;
        let reference: oci_client::Reference = reference
            .parse()
            .with_context(|| format!("parse {reference}"))?;
        let (client, _) =
            crate::component_admission::registry_client(&reference, self.insecure, &[], None)?;
        let auth = match docker_credential::get_credential(reference.resolve_registry()) {
            Ok(DockerCredential::UsernamePassword(user, password)) => {
                RegistryAuth::Basic(user, password)
            }
            Ok(DockerCredential::IdentityToken(_)) => {
                anyhow::bail!("an identity token is not a registry credential")
            }
            Err(
                CredentialRetrievalError::ConfigNotFound
                | CredentialRetrievalError::ConfigReadError
                | CredentialRetrievalError::NoCredentialConfigured,
            ) => RegistryAuth::Anonymous,
            Err(error) => anyhow::bail!("read the registry credential: {error}"),
        };
        Ok((client, reference, auth))
    }
}

impl Components for OciComponents {
    async fn push(&mut self, reference: &str, bytes: &[u8]) -> anyhow::Result<String> {
        wash_runtime::oci::push_component(
            reference,
            bytes,
            wash_runtime::oci::OciConfig {
                insecure: self.insecure,
                ..wash_runtime::oci::OciConfig::default()
            },
            None,
        )
        .await
    }

    async fn tags(&mut self, repository: &str) -> anyhow::Result<Vec<String>> {
        let (client, reference, auth) = self.client(repository)?;
        let mut tags = Vec::new();
        loop {
            let page = client
                .list_tags(
                    &reference,
                    &auth,
                    Some(1000),
                    tags.last().map(String::as_str),
                )
                .await
                .with_context(|| format!("list the tags of {repository}"))?;
            let full = page.tags.len() == 1000;
            tags.extend(page.tags);
            if !full {
                return Ok(tags);
            }
        }
    }

    async fn manifest(&mut self, reference: &str) -> anyhow::Result<(Vec<u8>, String)> {
        let (client, parsed, auth) = self.client(reference)?;
        let (manifest, _) = client
            .pull_manifest_raw(
                &parsed,
                &auth,
                &[oci_client::manifest::OCI_IMAGE_MEDIA_TYPE],
            )
            .await
            .with_context(|| format!("read the manifest of {reference}"))?;
        let value: serde_json::Value =
            serde_json::from_slice(&manifest).context("parse the role manifest")?;
        let media_type = value["mediaType"]
            .as_str()
            .unwrap_or(oci_client::manifest::OCI_IMAGE_MEDIA_TYPE)
            .to_owned();
        Ok((manifest.to_vec(), media_type))
    }

    async fn put_manifest(
        &mut self,
        reference: &str,
        manifest: &[u8],
        media_type: &str,
    ) -> anyhow::Result<()> {
        let (client, parsed, auth) = self.client(reference)?;
        client
            .auth(&parsed, &auth, oci_client::RegistryOperation::Push)
            .await
            .with_context(|| format!("authenticate the push of {reference}"))?;
        client
            .push_manifest_raw(&parsed, manifest.to_vec(), media_type.parse()?)
            .await
            .with_context(|| format!("store the manifest of {reference}"))?;
        Ok(())
    }
}

/// The highest `x.y.z` tag below `current`.
fn previous_version(tags: &[String], current: &str) -> Option<String> {
    let parse = |tag: &str| -> Option<(u64, u64, u64)> {
        let mut parts = tag.split('.').map(|part| part.parse().ok());
        let version = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    };
    let current = parse(current)?;
    tags.iter()
        .filter_map(|tag| parse(tag).map(|version| (version, tag)))
        .filter(|(version, _)| *version < current)
        .max()
        .map(|(_, tag)| tag.clone())
}

/// The digest of the single layer of a role manifest: the SHA-256 of its wasm.
fn wasm_layer(manifest: &[u8]) -> anyhow::Result<String> {
    let value: serde_json::Value =
        serde_json::from_slice(manifest).context("parse the role manifest")?;
    match value["layers"].as_array().map(Vec::as_slice) {
        Some([layer]) => layer["digest"]
            .as_str()
            .map(str::to_owned)
            .context("the role layer has no digest"),
        _ => anyhow::bail!("a role manifest has one layer"),
    }
}

/// When the role of the previous chart version has the wasm layer `layer`,
/// store its manifest unchanged under the chart version and return the role
/// pinned by that manifest's digest (owner ruling wamn-vcpvf: detection by
/// layer digest, never by manifest digest).
async fn reuse_role(
    components: &mut impl Components,
    registry: &str,
    role: &str,
    layer: &str,
) -> anyhow::Result<Option<String>> {
    let repository = format!("{registry}/wamn/{role}");
    let tags = components.tags(&repository).await?;
    let Some(previous) = previous_version(&tags, CHART_VERSION) else {
        return Ok(None);
    };
    let (manifest, media_type) = components
        .manifest(&format!("{repository}:{previous}"))
        .await?;
    if wasm_layer(&manifest)? != layer {
        eprintln!("build-platform {role} layer {layer} differs from {previous}");
        return Ok(None);
    }
    components
        .put_manifest(
            &format!("{repository}:{CHART_VERSION}"),
            &manifest,
            &media_type,
        )
        .await?;
    eprintln!("build-platform {role} layer {layer} copy {previous}");
    Ok(Some(format!("{repository}@{}", sha256(&manifest))))
}

/// How each image reaches the chart version tag: built, or copied from the
/// tag that carries its input hash (wamn-vcpvf). The role components are
/// always built.
struct Plan {
    /// The hex input hash of each image.
    hashes: BTreeMap<&'static str, String>,
    /// The manifest digest of each image whose input tag exists.
    images: BTreeMap<&'static str, String>,
}

/// The tag that carries the input hash of an image.
fn input_tag(registry: &str, target: &str, hash: &str) -> String {
    format!("{registry}/wamn-{target}:input-{hash}")
}

/// Look up the manifest digest of the input tag of `target`. A tag that is
/// not found gives `None`. Any other error is retried once; if the retry
/// fails too, the error is printed and the result is `None`.
async fn lookup(
    run: &impl AsyncFn(&mut Command) -> anyhow::Result<String>,
    registry: &str,
    target: &str,
    hash: &str,
) -> Option<String> {
    let reference = input_tag(registry, target, hash);
    let mut retried = false;
    loop {
        let digest = run(Command::new("docker").args([
            "buildx",
            "imagetools",
            "inspect",
            "--format",
            "{{json .Manifest}}",
            &reference,
        ]))
        .await
        .and_then(|json| manifest_digest(&json));
        match digest {
            Ok(digest) => return Some(digest),
            Err(error) if format!("{error:#}").contains(&format!("{reference}: not found")) => {
                return None;
            }
            Err(error) if retried => {
                eprintln!("build-platform {target} input-{hash} lookup failed: {error:#}");
                return None;
            }
            Err(_) => retried = true,
        }
    }
}

/// Hash the inputs of every image, then look up the input tags together. An
/// image whose tag is not found, or whose lookup fails twice, is built.
async fn plan(
    request: &BuildPlatformRequest,
    run: &impl AsyncFn(&mut Command) -> anyhow::Result<String>,
) -> anyhow::Result<Plan> {
    let mut hashes = BTreeMap::new();
    {
        let _stage = start_stage("input-hashes");
        let mut graphs = Vec::new();
        for manifest in ["Cargo.toml", "apps/Cargo.toml"] {
            let metadata = run(Command::new("cargo")
                .current_dir(&request.repository)
                .args([
                    "metadata",
                    "--format-version",
                    "1",
                    "--locked",
                    "--all-features",
                    "--manifest-path",
                    manifest,
                ]))
            .await?;
            graphs.push(inputs::parse_metadata(
                &serde_json::from_str(&metadata).context("parse cargo metadata")?,
            )?);
        }
        for target in IMAGES {
            let files = inputs::input_files(&request.repository, target, &graphs[0], &graphs[1])?;
            hashes.insert(target, inputs::input_hash(target, &files));
        }
    }
    let _stage = start_stage("input-lookups");
    let registry = &request.registry;
    let lookups = IMAGES.map(|target| lookup(run, registry, target, &hashes[target]));
    let digests = futures_util::future::join_all(lookups).await;
    let mut images = BTreeMap::new();
    for (target, digest) in IMAGES.into_iter().zip(digests) {
        if let Some(digest) = digest {
            images.insert(target, digest);
        }
    }
    for (target, hash) in &hashes {
        let action = if images.contains_key(target) {
            "copy"
        } else {
            "build"
        };
        eprintln!("build-platform {target} input-{hash} {action}");
    }
    Ok(Plan { hashes, images })
}

/// Point the chart version tag of `target` at the image of its input tag,
/// in the registry, and return it pinned by that image's digest.
async fn copy_image(
    run: &mut impl AsyncFnMut(&mut Command) -> anyhow::Result<String>,
    registry: &str,
    target: &str,
    hash: &str,
    digest: &str,
) -> anyhow::Result<String> {
    let _stage = start_stage(format!("copy-{target}"));
    let reference = format!("{registry}/wamn-{target}:{CHART_VERSION}");
    // With one source, --prefer-index=false copies the manifest as it is, so
    // the digest holds.
    run(Command::new("docker").args([
        "buildx",
        "imagetools",
        "create",
        "--prefer-index=false",
        "--tag",
        &reference,
        &input_tag(registry, target, hash),
    ]))
    .await?;
    Ok(format!("{reference}@{digest}"))
}

/// Build the host, runner, gates, identity and edge images, each through `run`
/// unless its input tag exists, and export the role components to `roles`.
/// Only when every build succeeded, push the built images under the chart
/// version and input tags, copy the others, push the role components, and
/// return them all pinned by digest.
async fn build_then_push(
    request: &BuildPlatformRequest,
    roles: &Path,
    mut run: impl AsyncFn(&mut Command) -> anyhow::Result<String>,
    components: &mut impl Components,
) -> anyhow::Result<([String; 5], (String, String))> {
    {
        let _stage = start_stage("ui-build");
        run(Command::new("pnpm").current_dir(&request.repository).args([
            "--filter",
            "@wamn/platform-ui",
            "build",
        ]))
        .await?;
    }
    let head = run(Command::new("git").current_dir(&request.repository).args([
        "rev-parse",
        "--verify",
        "HEAD",
    ]))
    .await?;
    let Plan { hashes, images } = plan(request, &run).await?;
    let registry = &request.registry;
    let mut references = Vec::new();
    for target in IMAGES {
        let reference = format!("{registry}/wamn-{target}:{CHART_VERSION}");
        let hash = &hashes[target];
        if !images.contains_key(target) {
            let _stage = start_stage(format!("image-build-{target}"));
            run(docker(&request.repository)?
                .args(["--target", target, "--tag", &reference, "--tag"])
                .arg(input_tag(registry, target, hash))
                .arg(format!("--label=wamn.dev/source-head={}", head.trim()))
                .arg("--label=wamn.dev/build-profile=release")
                .arg(format!("--label=wamn.dev/input-hash={hash}"))
                .arg(&request.repository))
            .await?;
        }
        references.push((target, reference));
    }
    {
        let _stage = start_stage("roles-export");
        run(docker(&request.repository)?
            .args(["--target", "roles", "--output"])
            .arg(format!("type=local,dest={}", roles.display()))
            .arg(&request.repository))
        .await?;
    }
    let mut role_components = Vec::new();
    for (file, role) in ROLES {
        let bytes = std::fs::read(roles.join(file)).with_context(|| format!("read {file}"))?;
        role_components.push((role, bytes));
    }
    let mut pushed = Vec::new();
    for (target, reference) in &references {
        let hash = &hashes[target];
        if let Some(digest) = images.get(target) {
            pushed.push(copy_image(&mut run, registry, target, hash, digest).await?);
            continue;
        }
        let _stage = start_stage(format!("push-{target}"));
        pushed.push(push_image(&mut run, registry, target, reference).await?);
        run(Command::new("docker").args(["push", &input_tag(registry, target, hash)])).await?;
    }
    let images = <[String; 5]>::try_from(pushed)
        .map_err(|_| anyhow::anyhow!("five platform images were pushed"))?;
    Ok((
        images,
        push_roles(registry, role_components, components).await?,
    ))
}

/// `docker build` of one target of the root `Dockerfile`, for amd64. The
/// runner's cases compile against the package outputs of `wamn build`, the
/// named context `apps-target`.
fn docker(repository: &Path) -> anyhow::Result<Command> {
    let mut command = Command::new("docker");
    command.args(docker_arguments(
        &checkout_key(repository)?,
        &repository.join("Dockerfile").to_string_lossy(),
        &repository.join("apps/target/wamn").to_string_lossy(),
    ));
    Ok(command)
}

/// Push the built `reference` and return it pinned by the digest the
/// registry holds.
async fn push_image(
    run: &mut impl AsyncFnMut(&mut Command) -> anyhow::Result<String>,
    registry: &str,
    target: &str,
    reference: &str,
) -> anyhow::Result<String> {
    run(Command::new("docker").args(["push", reference])).await?;
    let digests = run(Command::new("docker").args([
        "image",
        "inspect",
        "--format",
        "{{range .RepoDigests}}{{println .}}{{end}}",
        reference,
    ]))
    .await?;
    let prefix = format!("{registry}/wamn-{target}@");
    let digest = digests
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .filter(|digest| digest.starts_with("sha256:"))
        .with_context(|| format!("docker reports no pushed digest for {reference}"))?;
    Ok(format!("{reference}@{digest}"))
}

/// Push the http and materializer components as `<registry>/wamn/<role>`,
/// and return each pinned by digest. A role whose wasm layer equals that of
/// the previous chart version keeps that version's manifest and digest.
async fn push_roles(
    registry: &str,
    role_components: Vec<(&str, Vec<u8>)>,
    components: &mut impl Components,
) -> anyhow::Result<(String, String)> {
    let mut pushed = Vec::new();
    for (role, bytes) in role_components {
        let _stage = start_stage(format!("push-{role}"));
        // A lookup that fails, for any reason, leaves the role to be pushed.
        if let Ok(Some(pinned)) = reuse_role(components, registry, role, &sha256(&bytes)).await {
            pushed.push(pinned);
            continue;
        }
        let digest = components
            .push(&format!("{registry}/wamn/{role}:{CHART_VERSION}"), &bytes)
            .await
            .with_context(|| format!("push the {role} role component"))?;
        ensure!(
            digest.starts_with("sha256:"),
            "the {role} push returned {digest}"
        );
        pushed.push(format!("{registry}/wamn/{role}@{digest}"));
    }
    let materializer = pushed.pop().context("the materializer component")?;
    let http = pushed.pop().context("the http component")?;
    Ok((http, materializer))
}

/// After Cloud Build pushed the roles, point each chart version tag back at
/// the previous version's manifest when the wasm layer is the same.
async fn reuse_pushed_roles(
    registry: &str,
    components: &mut impl Components,
) -> anyhow::Result<()> {
    for (_, role) in ROLES {
        let _stage = start_stage(format!("reuse-{role}"));
        let (manifest, _) = components
            .manifest(&format!("{registry}/wamn/{role}:{CHART_VERSION}"))
            .await?;
        let layer = wasm_layer(&manifest)?;
        // A failed lookup keeps the manifest that Cloud Build pushed.
        let _ = reuse_role(components, registry, role, &layer).await;
    }
    Ok(())
}

fn process_list() -> anyhow::Result<Vec<(u32, Vec<String>)>> {
    let mut processes = Vec::new();
    for entry in std::fs::read_dir("/proc").context("read the process list")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse() else {
            continue;
        };
        let bytes = match std::fs::read(entry.path().join("cmdline")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read process arguments before building"),
        };
        let args = bytes
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        processes.push((pid, args));
    }
    Ok(processes)
}

/// An owned source supplement: package output is a named Docker context,
/// not a Cargo target cache. All target directories stay out of the upload.
struct CloudSource(PathBuf);

impl Drop for CloudSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_package_output(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let destination = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_package_output(&entry.path(), &destination)?;
        } else {
            ensure!(
                entry.file_type()?.is_file(),
                "package output must contain regular files"
            );
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudBuild {
    id: String,
    status: String,
    start_time: Option<chrono::DateTime<Utc>>,
    finish_time: Option<chrono::DateTime<Utc>>,
}

/// e2-highcpu-8, default pool, US regions, USD per build-minute.
/// https://cloud.google.com/build/pricing (read 2026-10-20).
const E2_HIGHCPU_8_USD_PER_MINUTE: f64 = 0.0156;

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct DailyCounter {
    cloud_minutes: u64,
    spend_usd: f64,
}

fn agent_configuration() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLOUDSDK_CONFIG") {
        return Ok(path.into());
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME is absent")?)
            .join(".config/gcloud-wamn-agent"),
    )
}

type CounterStore = std::sync::Arc<dyn object_store::ObjectStore>;

/// Opens the daily counter bucket with a fresh gcloud access token for each operation.
struct CounterStores<F> {
    gcloud: std::ffi::OsString,
    configuration: PathBuf,
    bucket: String,
    open: F,
}

impl<F> CounterStores<F>
where
    F: Fn(&str, String) -> anyhow::Result<CounterStore>,
{
    // Tokens expire after an hour, so no token outlives one counter operation.
    async fn store(&self) -> anyhow::Result<CounterStore> {
        let result = Command::new(&self.gcloud)
            .args(["auth", "print-access-token"])
            .env("CLOUDSDK_CONFIG", &self.configuration)
            .kill_on_drop(true)
            .output()
            .await
            .context("start gcloud auth print-access-token")?;
        ensure!(
            result.status.success(),
            "gcloud auth print-access-token failed with {}",
            result.status
        );
        let token = String::from_utf8(result.stdout)
            .map_err(|_| anyhow::anyhow!("gcloud auth print-access-token wrote non-UTF-8"))?;
        (self.open)(&self.bucket, token.trim().to_owned())
    }
}

fn gcs_store(bucket: &str, bearer: String) -> anyhow::Result<CounterStore> {
    let store = object_store::gcp::GoogleCloudStorageBuilder::new()
        .with_bucket_name(bucket)
        .with_credentials(std::sync::Arc::new(
            object_store::StaticCredentialProvider::new(object_store::gcp::GcpCredential {
                bearer,
            }),
        ))
        .build()
        .map_err(|_| {
            anyhow::anyhow!("configure the build counter with the agent's access token")
        })?;
    Ok(std::sync::Arc::new(store))
}

async fn read_counter<F>(
    stores: &CounterStores<F>,
    path: &object_store::path::Path,
) -> anyhow::Result<(DailyCounter, object_store::PutMode)>
where
    F: Fn(&str, String) -> anyhow::Result<CounterStore>,
{
    use object_store::ObjectStoreExt as _;
    let bucket = &stores.bucket;
    let object = match stores.store().await?.get(path).await {
        Ok(object) => object,
        Err(object_store::Error::NotFound { .. }) => {
            return Ok((DailyCounter::default(), object_store::PutMode::Create));
        }
        Err(_) => anyhow::bail!("read gs://{bucket}/{path}; check the agent's storage access"),
    };
    let mode = object_store::PutMode::Update(object_store::UpdateVersion {
        e_tag: object.meta.e_tag.clone(),
        version: object.meta.version.clone(),
    });
    let bytes = object
        .bytes()
        .await
        .map_err(|_| anyhow::anyhow!("read the build counter bytes"))?;
    let counter: DailyCounter =
        serde_json::from_slice(&bytes).context("decode the daily build counter")?;
    ensure!(
        counter.spend_usd.is_finite() && counter.spend_usd >= 0.0,
        "the daily build counter has an invalid spend"
    );
    Ok((counter, mode))
}

async fn add_cloud_minutes<F>(
    stores: &CounterStores<F>,
    path: &object_store::path::Path,
    minutes: u32,
) -> anyhow::Result<()>
where
    F: Fn(&str, String) -> anyhow::Result<CounterStore>,
{
    let bucket = &stores.bucket;
    loop {
        let (mut counter, mode) = read_counter(stores, path).await?;
        counter.cloud_minutes = counter
            .cloud_minutes
            .checked_add(u64::from(minutes))
            .context("cloud build minutes overflow")?;
        counter.spend_usd += f64::from(minutes) * E2_HIGHCPU_8_USD_PER_MINUTE;
        let payload = serde_json::to_vec(&counter)?.into();
        match stores
            .store()
            .await?
            .put_opts(path, payload, mode.into())
            .await
        {
            Ok(_) => return Ok(()),
            Err(
                object_store::Error::Precondition { .. }
                | object_store::Error::AlreadyExists { .. },
            ) => {}
            Err(_) => anyhow::bail!(
                "write gs://{bucket}/{path}; the cloud build's {minutes} minutes remain unrecorded"
            ),
        }
    }
}

fn cloud_minutes(build: &CloudBuild) -> anyhow::Result<u32> {
    match (build.start_time, build.finish_time) {
        (Some(start), Some(finish)) => {
            let nanos = (finish - start)
                .num_nanoseconds()
                .context("cloud build duration is too large")?;
            let nanos = u64::try_from(nanos).context("cloud build finished before it started")?;
            u32::try_from(nanos.div_ceil(60_000_000_000)).context("cloud build minutes overflow")
        }
        // No start means no worker ran, including a failure before execution.
        (None, _) => Ok(0),
        _ => anyhow::bail!(
            "Cloud Build {} lacks start or finish times; its spend cannot be recorded",
            build.id
        ),
    }
}

/// Cloud command diagnostics exclude the arguments and stdout. They carry the
/// last 20 lines of stderr (owner ruling 2026-10-21).
async fn cloud_output(command: &mut Command) -> anyhow::Result<String> {
    let result = command
        .kill_on_drop(true)
        .output()
        .await
        .context("start gcloud")?;
    ensure!(
        result.status.success(),
        "gcloud failed with {}:\n{}",
        result.status,
        stderr_tail(&result.stderr, 20)
    );
    String::from_utf8(result.stdout).context("gcloud returned non-UTF-8 output")
}

fn stderr_tail(stderr: &[u8], lines: usize) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let all: Vec<_> = stderr.trim_end().lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn gcloud(project: &str) -> anyhow::Result<Command> {
    let mut command = Command::new("gcloud");
    command.args(["--quiet", "--project", project]);
    command.env("CLOUDSDK_CONFIG", agent_configuration()?);
    if std::env::var_os("KUBECONFIG").is_none() {
        command.env(
            "KUBECONFIG",
            PathBuf::from(std::env::var_os("HOME").context("HOME is absent")?)
                .join(".kube/wamn-dev"),
        );
    }
    Ok(command)
}

async fn cloud_build(
    request: &BuildPlatformRequest,
    processes: &[(u32, Vec<String>)],
) -> anyhow::Result<PathBuf> {
    let cluster = request
        .cluster
        .as_ref()
        .context("a busy local builder requires --cluster <cluster.k> for Cloud Build")?;
    ensure!(
        cluster.provider == crate::platform::document::Provider::Gcp,
        "Cloud Build requires a GCP --cluster document"
    );
    {
        let _stage = start_stage("ui-build");
        output(Command::new("pnpm").current_dir(&request.repository).args([
            "--filter",
            "@wamn/platform-ui",
            "build",
        ]))
        .await?;
    }
    let mut run = output;
    let plan = plan(request, &run).await?;
    let stores = CounterStores {
        gcloud: "gcloud".into(),
        configuration: agent_configuration()?,
        bucket: cluster.buckets.state.clone(),
        open: gcs_store,
    };
    // Charge the UTC submission day, even when a build finishes after midnight.
    let day =
        object_store::path::Path::from(format!("build/{}.json", Utc::now().format("%Y-%m-%d")));
    let (counter, _) = read_counter(&stores, &day).await?;
    if let BuildLocation::Refused { spend_usd, cap_usd } = build_location(
        processes,
        std::process::id(),
        request.cloud,
        counter.spend_usd,
        cluster.build_cloud_cap_usd,
    ) {
        anyhow::bail!(
            "Cloud Build refused: spend USD {spend_usd:.4} is at or above cap USD {cap_usd:.4}"
        );
    }
    let build =
        submit_cloud_build(request, &plan, &cluster.project_id, &cluster.buckets.state).await?;
    // Record failed builds before reporting their outcome.
    add_cloud_minutes(&stores, &day, cloud_minutes(&build)?).await?;
    ensure!(
        build.status == "SUCCESS",
        "Cloud Build {} finished with {}",
        build.id,
        build.status
    );
    for (target, digest) in &plan.images {
        copy_image(
            &mut run,
            &request.registry,
            target,
            &plan.hashes[target],
            digest,
        )
        .await?;
    }
    reuse_pushed_roles(
        &request.registry,
        &mut OciComponents {
            insecure: loopback(&request.registry),
        },
    )
    .await?;
    stamp_only(request).await
}

async fn submit_cloud_build(
    request: &BuildPlatformRequest,
    plan: &Plan,
    project: &str,
    state_bucket: &str,
) -> anyhow::Result<CloudBuild> {
    let _stage = start_stage("cloud-submit");
    let supplement = format!(".wamn-cloud-build-{}", std::process::id());
    let path = request.repository.join(&supplement);
    std::fs::create_dir(&path).context("create the cloud source supplement")?;
    let source = CloudSource(path);
    copy_package_output(
        &request.repository.join("apps/target/wamn"),
        &source.0.join("apps"),
    )?;
    let head = output(Command::new("git").current_dir(&request.repository).args([
        "rev-parse",
        "--verify",
        "HEAD",
    ]))
    .await?;
    let config = cloud_configuration(request, &supplement, head.trim(), plan)?;
    let config_file = source.0.join("cloudbuild.json");
    std::fs::write(&config_file, serde_json::to_vec(&config)?)?;
    let submitted = cloud_output(
        gcloud(project)?
            .args([
                "builds",
                "submit",
                "--async",
                "--format=json",
                "--region=us-central1",
                "--config",
            ])
            .arg(&config_file)
            // The agent already writes below build/ in the state bucket.
            .arg(format!(
                "--gcs-source-staging-dir=gs://{state_bucket}/build/source"
            ))
            .arg("--ignore-file")
            .arg(request.repository.join(".gcloudignore"))
            .arg(&request.repository),
    )
    .await?;
    let mut build: CloudBuild =
        serde_json::from_str(&submitted).context("decode the submitted build")?;
    drop(source);
    loop {
        match build.status.as_str() {
            "PENDING" | "QUEUED" | "WORKING" => {}
            "SUCCESS" | "FAILURE" | "INTERNAL_ERROR" | "TIMEOUT" | "CANCELLED" | "EXPIRED" => {
                return Ok(build);
            }
            _ => anyhow::bail!("Cloud Build {} returned an unknown status", build.id),
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let described = cloud_output(gcloud(project)?.args([
            "builds",
            "describe",
            &build.id,
            "--region=us-central1",
            "--format=json",
        ]))
        .await?;
        build = serde_json::from_str(&described).context("decode Cloud Build status")?;
    }
}

/// The Docker CLI with buildx, the version that the runner image pins.
const CLOUD_DOCKER_IMAGE: &str = "docker:28.5.1-cli";

fn cloud_configuration(
    request: &BuildPlatformRequest,
    supplement: &str,
    head: &str,
    plan: &Plan,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    let mut steps = Vec::new();
    let key = checkout_key(&request.repository)?;
    let base = docker_arguments(&key, "Dockerfile", &format!("{supplement}/apps"));
    // The token is captured in memory, then written privately to the shared
    // credential volume. Neither the token nor its encoded form is logged.
    // A token lasts an hour, so each push step gets a fresh one.
    let auth = r"import base64, json, os, subprocess, sys
os.umask(0o077)
token = subprocess.check_output(['gcloud', 'auth', 'print-access-token'], stderr=subprocess.DEVNULL).decode().strip()
with open('/cloud-auth/config.json', 'w') as output:
    json.dump({'auths': {sys.argv[1]: {'auth': base64.b64encode(('oauth2accesstoken:' + token).encode()).decode()}}}, output)
";
    let volume = json!([{"name": "cloud-auth", "path": "/cloud-auth"}]);
    let auth_step = json!({
        "name": "gcr.io/google.com/cloudsdktool/cloud-sdk:slim", "entrypoint": "python3",
        "args": ["-c", auth, request.registry.split('/').next().context("registry host")?],
        "volumes": volume,
    });
    // The worker daemon's BuildKit lacks named build contexts, so buildx runs
    // its own BuildKit in a container (docker-container driver).
    steps.push(json!({
        "name": CLOUD_DOCKER_IMAGE,
        "args": ["buildx", "create", "--name", "wamn", "--driver", "docker-container"],
        "env": ["DOCKER_CONFIG=/cloud-auth"], "volumes": volume,
    }));
    let registry = &request.registry;
    for target in IMAGES.into_iter().chain(["roles"]) {
        // An image whose input tag exists is copied after the build.
        if plan.images.contains_key(target) {
            continue;
        }
        let mut args: Vec<String> = vec!["buildx".into(), "--builder".into(), "wamn".into()];
        args.extend(base.iter().cloned());
        args.extend(["--target".into(), target.into()]);
        if target == "roles" {
            args.extend(["--output".into(), "type=local,dest=/workspace/roles".into()]);
        } else {
            let hash = &plan.hashes[target];
            steps.push(auth_step.clone());
            args.extend([
                "--tag".into(),
                format!("{registry}/wamn-{target}:{CHART_VERSION}"),
                "--tag".into(),
                input_tag(registry, target, hash),
                format!("--label=wamn.dev/source-head={head}"),
                "--label=wamn.dev/build-profile=release".into(),
                format!("--label=wamn.dev/input-hash={hash}"),
                // Owner ruling 2026-10-21: buildx pushes each image directly.
                "--push".into(),
            ]);
        }
        args.push(".".into());
        steps.push(json!({
            "name": CLOUD_DOCKER_IMAGE, "args": args,
            "env": ["DOCKER_CONFIG=/cloud-auth"], "volumes": volume,
        }));
    }
    steps.push(auth_step);
    for (file, role) in ROLES {
        steps.push(json!({
            // This release uses the same wash-runtime revision as Cargo.toml.
            "name": "ghcr.io/wasmcloud/wash:2.11.0", "entrypoint": "/usr/local/bin/wash",
            "args": ["oci", "push", format!("{registry}/wamn/{role}:{CHART_VERSION}"), format!("/workspace/roles/{file}")],
            "env": ["DOCKER_CONFIG=/cloud-auth"], "volumes": volume,
        }));
    }
    // Cloud Logging is the log store that the agent can read.
    Ok(
        json!({"steps": steps, "options": {"machineType": "E2_HIGHCPU_8", "logging": "CLOUD_LOGGING_ONLY"}}),
    )
}

fn docker_arguments(key: &str, file: &str, apps: &str) -> Vec<String> {
    vec![
        "build".into(),
        "--platform=linux/amd64".into(),
        "--provenance=false".into(),
        "--build-arg".into(),
        format!("WAMN_CHECKOUT_KEY={key}"),
        "--build-context".into(),
        format!("apps-target={apps}"),
        "--file".into(),
        file.into(),
    ]
}

/// Run `command` and return its standard output; a failure names its error output.
async fn output(command: &mut Command) -> anyhow::Result<String> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("start {program}"))?;
    ensure!(
        output.status.success(),
        "{program} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).with_context(|| format!("{program} wrote text"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn build_location_uses_other_builds_and_the_daily_counter() {
        fn process(pid: u32, args: &[&str]) -> (u32, Vec<String>) {
            (pid, args.iter().map(|arg| (*arg).to_owned()).collect())
        }
        let mut processes = vec![
            process(10, &["/bin/wamn-ctl", "build-platform"]),
            process(11, &["wamn-ctl", "build-platform", "--stamp-only"]),
            process(12, &["sh", "-c", "wamn-ctl build-platform"]),
            process(13, &["wamn-ctl", "env", "build-platform"]),
            process(15, &["wamn-ctl", "build-platform", "--cloud"]),
        ];
        assert_eq!(
            build_location(&processes, 10, false, 3.0, 3.0),
            BuildLocation::Local
        );
        // --cloud forces Cloud Build under the same cap.
        assert_eq!(
            build_location(&processes, 10, true, 2.0, 3.0),
            BuildLocation::Cloud
        );
        assert_eq!(
            build_location(&processes, 10, true, 3.0, 3.0),
            BuildLocation::Refused {
                spend_usd: 3.0,
                cap_usd: 3.0
            }
        );
        processes.push(process(
            14,
            &["wamn-ctl", "--log-level=info", "build-platform"],
        ));
        assert_eq!(
            build_location(&processes, 10, false, 2.0, 3.0),
            BuildLocation::Cloud
        );
        for spend_usd in [3.0, 3.1] {
            assert_eq!(
                build_location(&processes, 10, false, spend_usd, 3.0),
                BuildLocation::Refused {
                    spend_usd,
                    cap_usd: 3.0
                }
            );
        }
    }

    type Opened = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    // A fake gcloud that prints a new token on each call, and a store that
    // records the bearer each operation opened it with.
    fn fake_stores(
        name: &str,
        store: std::sync::Arc<object_store::memory::InMemory>,
    ) -> (
        CounterStores<impl Fn(&str, String) -> anyhow::Result<CounterStore>>,
        Opened,
    ) {
        use std::os::unix::fs::PermissionsExt as _;
        let directory =
            std::env::temp_dir().join(format!("wamn-counter-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let gcloud = directory.join("gcloud");
        std::fs::write(
            &gcloud,
            "#!/bin/sh\n[ \"$*\" = \"auth print-access-token\" ] || exit 2\necho call >> \"$CLOUDSDK_CONFIG/calls\"\necho \"token-$(wc -l < \"$CLOUDSDK_CONFIG/calls\" | tr -d ' ')\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&gcloud, std::fs::Permissions::from_mode(0o755)).unwrap();
        let opened = Opened::default();
        let recorded = opened.clone();
        let stores = CounterStores {
            gcloud: gcloud.into(),
            configuration: directory,
            bucket: "test".into(),
            open: move |bucket: &str, bearer: String| {
                assert_eq!(bucket, "test");
                recorded.lock().unwrap().push(bearer);
                Ok(store.clone() as CounterStore)
            },
        };
        (stores, opened)
    }

    #[test]
    fn cloud_builds_use_buildx_and_cloud_logging() {
        let request = BuildPlatformRequest {
            repository: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            registry: "us-central1-docker.pkg.dev/test/wamn".into(),
            out: PathBuf::from("/nonexistent"),
            cluster: None,
            cloud: true,
        };
        let hashes: BTreeMap<_, _> = IMAGES
            .into_iter()
            .chain(["roles"])
            .map(|target| (target, format!("{target}-hash")))
            .collect();
        let mut plan = Plan {
            hashes,
            images: BTreeMap::new(),
        };
        let config = cloud_configuration(&request, ".supplement", "head", &plan).unwrap();
        assert_eq!(config["options"]["logging"], "CLOUD_LOGGING_ONLY");
        let steps = config["steps"].as_array().unwrap();
        assert_eq!(steps[0]["args"][5], "docker-container");
        let builds: Vec<_> = steps
            .iter()
            .filter(|step| step["name"] == CLOUD_DOCKER_IMAGE && step["args"][3] == "build")
            .collect();
        assert_eq!(builds.len(), 6);
        let pushes = |step: &serde_json::Value| {
            step["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "--push")
        };
        assert_eq!(
            builds.iter().filter(|step| pushes(step)).count(),
            5,
            "every image target pushes, and roles exports locally"
        );
        // Each image build follows its own token step.
        for (index, step) in steps.iter().enumerate() {
            if pushes(step) {
                assert_eq!(steps[index - 1]["entrypoint"], "python3");
            }
        }
        assert!(!steps.iter().any(|step| step["args"][0] == "push"));
        let has = |step: &serde_json::Value, text: &str| {
            step["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg.as_str() == Some(text))
        };
        assert!(builds.iter().filter(|step| pushes(step)).all(|step| {
            IMAGES.iter().any(|target| {
                has(
                    step,
                    &format!(
                        "us-central1-docker.pkg.dev/test/wamn/wamn-{target}:input-{target}-hash"
                    ),
                ) && has(step, &format!("--label=wamn.dev/input-hash={target}-hash"))
            })
        }));
        let wash = |steps: &[serde_json::Value]| {
            steps
                .iter()
                .filter(|step| step["entrypoint"] == "/usr/local/bin/wash")
                .count()
        };
        assert_eq!(
            wash(steps),
            2,
            "each role pushes its chart version tag once"
        );
        // Copied targets are left out of the submission.
        plan.images.insert("host", "sha256:host".into());
        let config = cloud_configuration(&request, ".supplement", "head", &plan).unwrap();
        let steps = config["steps"].as_array().unwrap();
        let builds: Vec<_> = steps
            .iter()
            .filter(|step| step["name"] == CLOUD_DOCKER_IMAGE && step["args"][3] == "build")
            .collect();
        assert_eq!(builds.len(), 5);
        assert!(!builds.iter().any(|step| has(step, "host")));
        assert!(builds.iter().any(|step| has(step, "roles")));
        assert_eq!(wash(steps), 2);
    }

    #[test]
    fn gcloud_errors_carry_the_last_stderr_lines() {
        let stderr = (1..=25)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = stderr_tail(stderr.as_bytes(), 20);
        assert_eq!(tail.lines().count(), 20);
        assert!(tail.starts_with("line 6\n") && tail.ends_with("line 25"));
        assert_eq!(stderr_tail(b"one\n", 20), "one");
    }

    #[tokio::test]
    async fn daily_counter_fetches_a_token_for_each_read_and_write() {
        let (stores, opened) = fake_stores("token", std::sync::Arc::default());
        let path = object_store::path::Path::from("build/2026-10-21.json");
        read_counter(&stores, &path).await.unwrap();
        assert_eq!(*opened.lock().unwrap(), ["token-1"]);
        // A read and a write, each with its own token, as after a cloud build.
        add_cloud_minutes(&stores, &path, 5).await.unwrap();
        assert_eq!(*opened.lock().unwrap(), ["token-1", "token-2", "token-3"]);
        assert_eq!(
            read_counter(&stores, &path).await.unwrap().0.cloud_minutes,
            5
        );
        let failing = CounterStores {
            gcloud: "false".into(),
            ..stores
        };
        assert!(
            read_counter(&failing, &path)
                .await
                .unwrap_err()
                .to_string()
                .contains("gcloud auth print-access-token failed")
        );
        std::fs::remove_dir_all(&failing.configuration).unwrap();
    }

    #[tokio::test]
    async fn daily_counter_creates_and_accumulates_success_and_failure_minutes() {
        use object_store::ObjectStore as _;
        let store = std::sync::Arc::new(object_store::memory::InMemory::new());
        let (stores, _) = fake_stores("minutes", store.clone());
        let path = object_store::path::Path::from("build/2026-10-20.json");
        assert_eq!(
            read_counter(&stores, &path).await.unwrap().0,
            DailyCounter::default()
        );
        for (status, finish, expected_minutes) in [
            ("SUCCESS", "2026-10-20T01:01:00Z", 1),
            ("FAILURE", "2026-10-20T01:01:00.000000001Z", 2),
        ] {
            let build = CloudBuild {
                id: "test".into(),
                status: status.into(),
                start_time: Some("2026-10-20T01:00:00Z".parse().unwrap()),
                finish_time: Some(finish.parse().unwrap()),
            };
            let minutes = cloud_minutes(&build).unwrap();
            assert_eq!(minutes, expected_minutes);
            add_cloud_minutes(&stores, &path, minutes).await.unwrap();
        }
        let (counter, stale) = read_counter(&stores, &path).await.unwrap();
        assert_eq!(counter.cloud_minutes, 3);
        assert!((counter.spend_usd - 0.0468).abs() < 1e-12);
        add_cloud_minutes(&stores, &path, 1).await.unwrap();
        assert!(matches!(
            store
                .put_opts(
                    &path,
                    serde_json::to_vec(&counter).unwrap().into(),
                    stale.into()
                )
                .await,
            Err(object_store::Error::Precondition { .. })
        ));
        assert_eq!(
            read_counter(&stores, &path).await.unwrap().0.cloud_minutes,
            4
        );
        std::fs::remove_dir_all(&stores.configuration).unwrap();
    }

    #[test]
    fn checkout_key_matches_the_shell_hash_of_a_canonical_path() {
        // printf '%s' "$(realpath /)" | sha256sum | cut -c1-16
        assert_eq!(checkout_key(Path::new("/")).unwrap(), "8a5edab282632443");
        assert_eq!(checkout_key(Path::new("/./")).unwrap(), "8a5edab282632443");
        assert!(checkout_key(Path::new("/nonexistent/wamn-checkout")).is_err());
    }

    #[test]
    fn lane_start_prints_only_the_checkout_environment() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let key = checkout_key(&repository).unwrap();
        let output = std::process::Command::new(repository.join("tools/lane-start"))
            .arg("--print-env")
            .arg(&repository)
            .env("HOME", "/var/tmp/wamn-lane-env-test")
            .output()
            .expect("print the lane environment without building");
        assert!(output.status.success(), "{:?}", output.stderr);
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "export CARGO_TARGET_DIR=/var/tmp/wamn-lane-env-test/.cache/wamn-target/{key}\n\
                 export WAMN_CHECKOUT_KEY={key}\n"
            )
        );
    }

    #[test]
    fn the_receipt_digest_covers_paths_and_bytes_of_the_ui() {
        let dist =
            std::env::temp_dir().join(format!("wamn-platform-ui-digest-{}", std::process::id()));
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), b"index").unwrap();
        std::fs::write(dist.join("assets/app.js"), b"app").unwrap();
        let receipt = PlatformBuildReceipt {
            platform_ui: platform_ui_digest(&dist).unwrap(),
        };
        assert_eq!(
            receipt.platform_ui,
            "sha256:5a8575268118ba787b9258f2e2277418d1139a5cc0ba08f3c7bf5e6b0fd04e45"
        );
        let bytes = serde_json::to_vec(&receipt).unwrap();
        assert_eq!(
            serde_json::from_slice::<PlatformBuildReceipt>(&bytes).unwrap(),
            receipt
        );
        std::fs::rename(dist.join("assets/app.js"), dist.join("assets/new.js")).unwrap();
        assert_ne!(platform_ui_digest(&dist).unwrap(), receipt.platform_ui);
        std::fs::rename(dist.join("assets/new.js"), dist.join("assets/app.js")).unwrap();
        std::fs::write(dist.join("assets/app.js"), b"changed").unwrap();
        assert_ne!(platform_ui_digest(&dist).unwrap(), receipt.platform_ui);
        std::fs::remove_file(dist.join("index.html")).unwrap();
        assert!(platform_ui_digest(&dist).is_err());
        std::fs::remove_dir_all(dist).unwrap();
    }

    #[tokio::test]
    async fn a_failed_ui_build_pushes_nothing() {
        let (error, calls) = record(Path::new("/nonexistent"), "@wamn/platform-ui").await;
        assert!(error.to_string().contains("failed"), "{error}");
        assert_eq!(calls, ["pnpm --filter @wamn/platform-ui build"]);
    }

    #[tokio::test]
    async fn a_checkout_without_package_outputs_is_refused_before_any_build() {
        let repository = std::env::temp_dir().join(format!(
            "wamn-build-platform-no-apps-target-{}",
            std::process::id()
        ));
        let error = build_platform(&BuildPlatformRequest {
            cluster: None,
            cloud: false,
            out: repository.join("chart"),
            repository,
            registry: "localhost:5000".to_owned(),
        })
        .await
        .expect_err("a checkout without apps/target/wamn is refused");
        assert!(error.to_string().contains("run `wamn build`"), "{error}");
    }

    #[tokio::test]
    async fn stamp_only_requires_the_local_ui_before_reading_the_registry() {
        let repository =
            std::env::temp_dir().join(format!("wamn-stamp-no-ui-{}", std::process::id()));
        let error = stamp_only(&BuildPlatformRequest {
            cluster: None,
            cloud: false,
            out: repository.join("chart"),
            repository: repository.clone(),
            registry: "must-not-be-contacted.invalid".into(),
        })
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "the platform UI build {} is absent; run `pnpm --filter @wamn/platform-ui build`",
                platform_ui_dist(&repository).display()
            )
        );
    }

    /// A fake registry of role components. Each push writes a manifest
    /// with a new creation time, as oci-wasm does.
    #[derive(Default)]
    struct FakeComponents {
        manifests: BTreeMap<String, Vec<u8>>,
        pushed: Vec<String>,
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the fake answers at once, and the trait is async for the registry"
    )]
    impl Components for FakeComponents {
        async fn push(&mut self, reference: &str, bytes: &[u8]) -> anyhow::Result<String> {
            let created = format!("created {}", self.manifests.len());
            let manifest = serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "config": {"digest": sha256(created.as_bytes())},
                "layers": [{"mediaType": "application/wasm", "digest": sha256(bytes)}],
            }))?;
            let digest = sha256(&manifest);
            self.manifests.insert(reference.to_owned(), manifest);
            self.pushed.push(reference.to_owned());
            Ok(digest)
        }

        async fn tags(&mut self, repository: &str) -> anyhow::Result<Vec<String>> {
            let prefix = format!("{repository}:");
            Ok(self
                .manifests
                .keys()
                .filter_map(|reference| reference.strip_prefix(&prefix))
                .map(str::to_owned)
                .collect())
        }

        async fn manifest(&mut self, reference: &str) -> anyhow::Result<(Vec<u8>, String)> {
            let manifest = self
                .manifests
                .get(reference)
                .with_context(|| format!("{reference}: not found"))?;
            Ok((
                manifest.clone(),
                "application/vnd.oci.image.manifest.v1+json".into(),
            ))
        }

        async fn put_manifest(
            &mut self,
            reference: &str,
            manifest: &[u8],
            _: &str,
        ) -> anyhow::Result<()> {
            self.manifests
                .insert(reference.to_owned(), manifest.to_vec());
            Ok(())
        }
    }

    type Pinned = ([String; 5], (String, String));

    /// Run `build_then_push` on `checkout` against a fake registry that holds
    /// the image tags `images`, and return its result with `program args…` of
    /// each command. `fail` fails the command that contains it.
    async fn record_in(
        checkout: &(PathBuf, String, String),
        roles: &Path,
        fail: &str,
        images: &BTreeSet<String>,
        components: &mut FakeComponents,
    ) -> (anyhow::Result<Pinned>, Vec<String>) {
        let (repository, root, apps) = checkout;
        let calls = std::cell::RefCell::new(Vec::new());
        let result = build_then_push(
            &BuildPlatformRequest {
                cluster: None,
                cloud: false,
                repository: repository.clone(),
                registry: "localhost:5000".to_owned(),
                out: PathBuf::from("/chart"),
            },
            roles,
            async |command: &mut Command| {
                let std = command.as_std();
                let call = std::iter::once(std.get_program())
                    .chain(std.get_args())
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ");
                calls.borrow_mut().push(call.clone());
                if call.starts_with("cargo metadata") {
                    let apps_workspace = call.ends_with("apps/Cargo.toml");
                    return Ok((if apps_workspace { apps } else { root }).clone());
                }
                ensure!(!call.contains(fail), "{call} failed");
                let inspect = "docker buildx imagetools inspect --format {{json .Manifest}} ";
                if let Some(reference) = call.strip_prefix(inspect) {
                    ensure!(images.contains(reference), "ERROR: {reference}: not found");
                    return Ok(format!(
                        r#"{{"digest":"sha256:{}","size":1}}"#,
                        "c".repeat(64)
                    ));
                }
                let pushed =
                    "docker image inspect --format {{range .RepoDigests}}{{println .}}{{end}} ";
                if let Some(reference) = call.strip_prefix(pushed) {
                    let (repository, _) = reference.rsplit_once(':').unwrap();
                    return Ok(format!("{repository}@sha256:{}\n", "a".repeat(64)));
                }
                Ok(String::new())
            },
            components,
        )
        .await;
        (result, calls.into_inner())
    }

    /// Run `plan` on a fresh fake checkout named `name`. The inspect calls
    /// of the host get `host` in turn: `NOT_FOUND`, `TIMEOUT`, or the manifest
    /// JSON. Every other inspect is not found. Return the plan and the number
    /// of host inspect calls.
    async fn plan_with(name: &str, host: &[&str]) -> (Plan, usize) {
        let (repository, root, apps) = inputs::tests::fake_checkout(name);
        let inspects = std::cell::Cell::new(0);
        let result = plan(
            &BuildPlatformRequest {
                cluster: None,
                cloud: false,
                repository: repository.clone(),
                registry: "localhost:5000".to_owned(),
                out: PathBuf::from("/chart"),
            },
            &async |command: &mut Command| {
                let std = command.as_std();
                let call = std::iter::once(std.get_program())
                    .chain(std.get_args())
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ");
                if call.starts_with("cargo metadata") {
                    let apps_workspace = call.ends_with("apps/Cargo.toml");
                    return Ok((if apps_workspace { &apps } else { &root }).clone());
                }
                let reference = call.rsplit(' ').next().unwrap();
                ensure!(
                    reference.starts_with("localhost:5000/wamn-host:"),
                    "ERROR: {reference}: not found"
                );
                let answer = host[inspects.get()];
                inspects.set(inspects.get() + 1);
                ensure!(answer != NOT_FOUND, "ERROR: {reference}: not found");
                ensure!(answer != TIMEOUT, "ERROR: {TIMEOUT}");
                Ok(answer.to_owned())
            },
        )
        .await;
        let _ = std::fs::remove_dir_all(&repository);
        (result.expect("the plan succeeds"), inspects.get())
    }

    const NOT_FOUND: &str = "not found";
    const TIMEOUT: &str = "net/http: TLS handshake timeout";

    #[tokio::test]
    async fn a_host_input_tag_that_is_not_found_is_built_without_a_retry() {
        let (plan, inspects) = plan_with("lookup-not-found", &[NOT_FOUND]).await;
        assert_eq!(inspects, 1);
        assert!(!plan.images.contains_key("host"));
    }

    #[tokio::test]
    async fn a_host_lookup_that_fails_once_is_retried_and_copied() {
        let json = format!(r#"{{"digest":"sha256:{}","size":1}}"#, "c".repeat(64));
        let (plan, inspects) = plan_with("lookup-retry", &[TIMEOUT, &json]).await;
        assert_eq!(inspects, 2);
        assert_eq!(plan.images["host"], format!("sha256:{}", "c".repeat(64)));
    }

    #[tokio::test]
    async fn a_host_lookup_that_fails_twice_is_built() {
        let (plan, inspects) = plan_with("lookup-fails", &[TIMEOUT, TIMEOUT]).await;
        assert_eq!(inspects, 2);
        assert!(!plan.images.contains_key("host"));
    }

    /// Run `build_then_push` on a fresh fake checkout with an empty registry;
    /// the run must fail.
    async fn record(roles: &Path, fail: &str) -> (anyhow::Error, Vec<String>) {
        let checkout = inputs::tests::fake_checkout(&format!("record-{}", fail.len()));
        let (result, calls) = record_in(
            &checkout,
            roles,
            fail,
            &BTreeSet::new(),
            &mut FakeComponents::default(),
        )
        .await;
        let _ = std::fs::remove_dir_all(&checkout.0);
        (result.expect_err("the run fails"), calls)
    }

    /// Write both role files to a fresh directory named `name`.
    fn exported_roles(name: &str) -> PathBuf {
        let roles =
            std::env::temp_dir().join(format!("wamn-build-platform-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&roles).expect("create the roles directory");
        for (file, _) in ROLES {
            std::fs::write(roles.join(file), b"\0asm").expect("write a role component");
        }
        roles
    }

    #[tokio::test]
    async fn an_unchanged_target_is_copied_and_a_changed_one_is_built() {
        let checkout = inputs::tests::fake_checkout("copy-or-build");
        let roles = exported_roles("copy-or-build");
        let mut components = FakeComponents::default();
        // No input tag exists, so every target builds as before.
        let (first, calls) =
            record_in(&checkout, &roles, "\0", &BTreeSet::new(), &mut components).await;
        first.expect("the first build succeeds");
        let builds = |calls: &[String]| {
            calls
                .iter()
                .filter(|call| call.starts_with("docker build "))
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(builds(&calls).len(), 6, "{calls:?}");
        assert!(
            !calls.iter().any(|call| call.contains("imagetools create")),
            "{calls:?}"
        );
        let tags: BTreeSet<String> = calls
            .iter()
            .filter_map(|call| call.strip_prefix("docker push "))
            .filter(|reference| reference.contains(":input-"))
            .map(str::to_owned)
            .collect();
        assert_eq!(tags.len(), 5, "{calls:?}");
        assert!(
            builds(&calls)
                .iter()
                .filter(|call| !call.contains("--target roles"))
                .all(|call| call.contains("--label=wamn.dev/input-hash=")),
            "{calls:?}"
        );
        assert_eq!(components.pushed.len(), 2, "{:?}", components.pushed);
        // Only the runner reads the package outputs.
        std::fs::write(
            checkout.0.join("apps/target/wamn/wamn_wms/build.json"),
            r#"{"changed":true}"#,
        )
        .unwrap();
        components.pushed.clear();
        let (second, calls) = record_in(&checkout, &roles, "\0", &tags, &mut components).await;
        let _ = std::fs::remove_dir_all(&checkout.0);
        let _ = std::fs::remove_dir_all(&roles);
        let ([host, runner, ..], _) = second.expect("the second build succeeds");
        // The roles always build; their manifests are compared by wasm layer.
        let built = builds(&calls);
        assert_eq!(built.len(), 2, "{calls:?}");
        assert!(built[0].contains("--target runner"), "{built:?}");
        assert!(built[1].contains("--target roles"), "{built:?}");
        let copies: Vec<_> = calls
            .iter()
            .filter(|call| call.starts_with("docker buildx imagetools create --prefer-index=false"))
            .collect();
        assert_eq!(copies.len(), 4, "{calls:?}");
        assert!(
            copies.iter().all(|call| !call.contains("wamn-runner")),
            "{copies:?}"
        );
        assert_eq!(
            host,
            format!(
                "localhost:5000/wamn-host:{CHART_VERSION}@sha256:{}",
                "c".repeat(64)
            )
        );
        assert_eq!(
            runner,
            format!(
                "localhost:5000/wamn-runner:{CHART_VERSION}@sha256:{}",
                "a".repeat(64)
            )
        );
    }

    #[tokio::test]
    async fn an_unchanged_role_keeps_the_previous_manifest_and_a_changed_one_is_pushed() {
        let checkout = inputs::tests::fake_checkout("roles");
        let roles = exported_roles("roles");
        let mut components = FakeComponents::default();
        // The previous chart holds the same http wasm and another materializer.
        let http = "localhost:5000/wamn/http";
        let materializer = "localhost:5000/wamn/materializer";
        let previous = components
            .push(&format!("{http}:0.0.1"), b"\0asm")
            .await
            .unwrap();
        let older = components
            .push(&format!("{materializer}:0.0.1"), b"\0asm old")
            .await
            .unwrap();
        components.pushed.clear();
        let (result, _) =
            record_in(&checkout, &roles, "\0", &BTreeSet::new(), &mut components).await;
        let _ = std::fs::remove_dir_all(&checkout.0);
        let _ = std::fs::remove_dir_all(&roles);
        let (_, (http_pinned, materializer_pinned)) = result.expect("the build succeeds");
        assert_eq!(http_pinned, format!("{http}@{previous}"));
        assert_eq!(
            components.manifests[&format!("{http}:{CHART_VERSION}")],
            components.manifests[&format!("{http}:0.0.1")]
        );
        assert_ne!(materializer_pinned, format!("{materializer}@{older}"));
        assert_eq!(
            components.pushed,
            [format!("{materializer}:{CHART_VERSION}")]
        );
        let manifest = &components.manifests[&format!("{materializer}:{CHART_VERSION}")];
        assert_eq!(
            materializer_pinned,
            format!("{materializer}@{}", sha256(manifest))
        );
        assert_eq!(wasm_layer(manifest).unwrap(), sha256(b"\0asm"));
    }

    #[tokio::test]
    async fn a_role_that_cloud_build_pushed_returns_to_the_previous_manifest_when_its_wasm_is_unchanged()
     {
        let mut components = FakeComponents::default();
        let http = "localhost:5000/wamn/http";
        let materializer = "localhost:5000/wamn/materializer";
        components
            .push(&format!("{http}:0.0.1"), b"same")
            .await
            .unwrap();
        components
            .push(&format!("{materializer}:0.0.1"), b"old")
            .await
            .unwrap();
        components
            .push(&format!("{http}:{CHART_VERSION}"), b"same")
            .await
            .unwrap();
        let pushed = components
            .push(&format!("{materializer}:{CHART_VERSION}"), b"new")
            .await
            .unwrap();
        let before = components.manifests[&format!("{http}:{CHART_VERSION}")].clone();
        reuse_pushed_roles("localhost:5000", &mut components)
            .await
            .unwrap();
        let after = &components.manifests[&format!("{http}:{CHART_VERSION}")];
        assert_ne!(&before, after);
        assert_eq!(after, &components.manifests[&format!("{http}:0.0.1")]);
        let materializer_now = &components.manifests[&format!("{materializer}:{CHART_VERSION}")];
        assert_eq!(sha256(materializer_now), pushed);
    }

    #[test]
    fn the_previous_version_is_the_highest_release_tag_below_the_chart() {
        let tags = [
            "0.1.9", "0.1.96", "0.1.100", "input-x", "0.1.97", "latest", "0.2",
        ]
        .map(str::to_owned);
        assert_eq!(previous_version(&tags, "0.1.97").as_deref(), Some("0.1.96"));
        assert_eq!(
            previous_version(&tags, "0.1.200").as_deref(),
            Some("0.1.100")
        );
        assert_eq!(previous_version(&tags, "0.1.9"), None);
    }

    /// Stamp from a fake registry whose tags all resolve except those that
    /// contain `absent`, and return the result with every command run.
    async fn stamp_recorded(absent: &str, out: &Path) -> (anyhow::Result<PathBuf>, Vec<String>) {
        let digest = format!("sha256:{}", "a".repeat(64));
        let mut calls = Vec::new();
        let stamped = stamp_from_registry(
            &BuildPlatformRequest {
                cluster: None,
                cloud: false,
                repository: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
                registry: "localhost:5000".to_owned(),
                out: out.to_owned(),
            },
            &PlatformBuildReceipt {
                platform_ui: digest.clone(),
            },
            async |command: &mut Command| {
                let std = command.as_std();
                let call = std::iter::once(std.get_program())
                    .chain(std.get_args())
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ");
                calls.push(call.clone());
                ensure!(!call.contains(absent), "ERROR: {call}: not found");
                Ok(format!(r#"{{"digest":"{digest}","size":1}}"#))
            },
            async |reference: &str| {
                ensure!(!reference.contains(absent), "{reference}: not found");
                Ok(digest.clone())
            },
        )
        .await;
        (stamped, calls)
    }

    #[tokio::test]
    async fn stamp_only_stamps_the_seven_registry_digests_and_builds_nothing() {
        let out = std::env::temp_dir().join(format!("wamn-stamp-only-{}", std::process::id()));
        let (stamped, calls) = stamp_recorded("\0", &out).await;
        let receipt: PlatformBuildReceipt =
            serde_json::from_slice(&std::fs::read(out.join(RECEIPT_FILE)).unwrap()).unwrap();
        assert_eq!(receipt.platform_ui, format!("sha256:{}", "a".repeat(64)));
        let values = stamped
            .and_then(|chart| Ok(std::fs::read_to_string(chart.join("values.yaml"))?))
            .map(|text| serde_yaml::from_str::<serde_yaml::Value>(&text));
        let _ = std::fs::remove_dir_all(&out);
        let values = values
            .expect("the chart is stamped")
            .expect("parse the values");
        let digest = format!("sha256:{}", "a".repeat(64));
        let image = &values["runtime-operator"]["runtime"]["image"];
        assert_eq!(
            image["repository"].as_str(),
            Some("localhost:5000/wamn-host")
        );
        assert_eq!(
            image["tag"].as_str(),
            Some(&*format!("{CHART_VERSION}@{digest}"))
        );
        let platform = &values["platform"];
        assert_eq!(
            platform["roles"]["http"]["image"].as_str(),
            Some(&*format!("localhost:5000/wamn/http@{digest}"))
        );
        assert_eq!(
            platform["gates"]["image"].as_str(),
            Some(&*format!(
                "localhost:5000/wamn-gates:{CHART_VERSION}@{digest}"
            ))
        );
        assert_eq!(
            platform["edge"]["image"].as_str(),
            Some(&*format!(
                "localhost:5000/wamn-edge-web:{CHART_VERSION}@{digest}"
            ))
        );
        assert_eq!(calls.len(), 5, "{calls:?}");
        assert!(
            calls
                .iter()
                .all(|call| call.starts_with("docker buildx imagetools inspect")),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn stamp_only_refuses_and_names_a_missing_image() {
        let out =
            std::env::temp_dir().join(format!("wamn-stamp-only-missing-{}", std::process::id()));
        let (stamped, calls) = stamp_recorded("wamn/materializer", &out).await;
        let error = stamped.expect_err("a missing role component is refused");
        assert!(!out.exists());
        let message = error.to_string();
        assert!(
            message.contains(&format!("localhost:5000/wamn/materializer:{CHART_VERSION}")),
            "{message}"
        );
        assert!(!message.contains("wamn-host"), "{message}");
        assert!(
            !calls
                .iter()
                .any(|call| call.starts_with("docker build ") || call.starts_with("docker push")),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_build_pushes_nothing() {
        let (error, calls) = record(Path::new("/nonexistent"), "--target runner").await;
        assert!(error.to_string().contains("failed"), "{error}");
        assert!(
            calls
                .last()
                .is_some_and(|call| call.contains("--target runner"))
        );
        assert!(
            !calls.iter().any(|call| call.contains("docker push")),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn every_build_runs_before_the_first_push() {
        let roles = exported_roles("order");
        let checkout = inputs::tests::fake_checkout("order");
        let (result, calls) = record_in(
            &checkout,
            &roles,
            "\0",
            &BTreeSet::new(),
            &mut FakeComponents::default(),
        )
        .await;
        let _ = std::fs::remove_dir_all(&roles);
        result.expect("the build succeeds");
        let first_push = calls
            .iter()
            .position(|call| call.starts_with("docker push"))
            .expect("a push ran");
        let builds = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.starts_with("docker build "))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(builds.len(), 6, "{calls:?}");
        assert!(builds.iter().all(|index| *index < first_push), "{calls:?}");
        let key = checkout_key(&checkout.0).unwrap();
        let _ = std::fs::remove_dir_all(&checkout.0);
        assert!(
            builds.iter().all(
                |index| calls[*index].contains(&format!("--build-arg WAMN_CHECKOUT_KEY={key}"))
            ),
            "{calls:?}"
        );
    }
}
