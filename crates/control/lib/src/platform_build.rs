//! The platform build step (docs/plan/platform-deploy.md §9.3, R15).
//!
//! It builds the host, runner, gates and identity images and the two role
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
    spend_usd: f64,
    cap_usd: f64,
) -> BuildLocation {
    let busy = processes
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
            return argument == "build-platform"
                && !arguments.any(|argument| argument == "--stamp-only");
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
    if build_location(&processes, std::process::id(), 0.0, 3.0) != BuildLocation::Local {
        return cloud_build(request, &processes).await;
    }
    let roles = std::env::temp_dir().join(format!("wamn-roles-{}", std::process::id()));
    let pushed = build_then_push(request, &roles, output).await;
    let _ = std::fs::remove_dir_all(&roles);
    let pushed = pushed?;
    stamp_chart(request, pushed, &ui_receipt(&request.repository)?)
}

/// Stamp the registry's images and role components of [`CHART_VERSION`] into
/// a copy of the release chart at `request.out`. It builds and pushes
/// nothing; when any of the six is missing, it refuses and names them.
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
    for target in ["host", "runner", "gates", "identity"] {
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
    let [host, runner, gates, identity, http, materializer] =
        <[String; 6]>::try_from(pinned).map_err(|_| anyhow::anyhow!("six images were resolved"))?;
    drop(stage);
    stamp_chart(
        request,
        ([host, runner, gates, identity], (http, materializer)),
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

/// Stamp the host, runner, gates and identity images and the http and
/// materializer components into a copy of the checkout's release chart.
fn stamp_chart(
    request: &BuildPlatformRequest,
    ([host, runner, gates, identity], (http, materializer)): ([String; 4], (String, String)),
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

/// Build the host, runner, gates and identity images and export the role
/// components to `roles`, each through `run`; only when every build
/// succeeded, push them and return them pinned by digest.
async fn build_then_push(
    request: &BuildPlatformRequest,
    roles: &Path,
    mut run: impl AsyncFnMut(&mut Command) -> anyhow::Result<String>,
) -> anyhow::Result<([String; 4], (String, String))> {
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
    let mut references = Vec::new();
    for target in ["host", "runner", "gates", "identity"] {
        let _stage = start_stage(format!("image-build-{target}"));
        let reference = format!("{}/wamn-{target}:{CHART_VERSION}", request.registry);
        run(docker(&request.repository)?
            .args(["--target", target, "--tag", &reference])
            .arg(format!("--label=wamn.dev/source-head={}", head.trim()))
            .arg("--label=wamn.dev/build-profile=release")
            .arg(&request.repository))
        .await?;
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
    let mut components = Vec::new();
    for (file, role) in [
        ("http_route.wasm", "http"),
        ("materializer.wasm", "materializer"),
    ] {
        let bytes = std::fs::read(roles.join(file)).with_context(|| format!("read {file}"))?;
        components.push((role, bytes));
    }
    let mut pushed = Vec::new();
    for (target, reference) in &references {
        let _stage = start_stage(format!("push-{target}"));
        pushed.push(push_image(&mut run, &request.registry, target, reference).await?);
    }
    let images = <[String; 4]>::try_from(pushed)
        .map_err(|_| anyhow::anyhow!("four platform images were pushed"))?;
    Ok((images, push_roles(&request.registry, components).await?))
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
/// and return each pinned by digest.
async fn push_roles(
    registry: &str,
    components: Vec<(&str, Vec<u8>)>,
) -> anyhow::Result<(String, String)> {
    let insecure = loopback(registry);
    let mut pushed = Vec::new();
    for (role, bytes) in components {
        let _stage = start_stage(format!("push-{role}"));
        let digest = wash_runtime::oci::push_component(
            &format!("{registry}/wamn/{role}:{CHART_VERSION}"),
            &bytes,
            wash_runtime::oci::OciConfig {
                insecure,
                ..wash_runtime::oci::OciConfig::default()
            },
            None,
        )
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
        counter.spend_usd,
        cluster.build_cloud_cap_usd,
    ) {
        anyhow::bail!(
            "Cloud Build refused: spend USD {spend_usd:.4} is at or above cap USD {cap_usd:.4}"
        );
    }
    let build = submit_cloud_build(request, &cluster.project_id, &cluster.buckets.state).await?;
    // Record failed builds before reporting their outcome.
    add_cloud_minutes(&stores, &day, cloud_minutes(&build)?).await?;
    ensure!(
        build.status == "SUCCESS",
        "Cloud Build {} finished with {}",
        build.id,
        build.status
    );
    stamp_only(request).await
}

async fn submit_cloud_build(
    request: &BuildPlatformRequest,
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
    let config = cloud_configuration(request, &supplement, head.trim())?;
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

fn cloud_configuration(
    request: &BuildPlatformRequest,
    supplement: &str,
    head: &str,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    let mut steps = Vec::new();
    let key = checkout_key(&request.repository)?;
    let base = docker_arguments(&key, "Dockerfile", &format!("{supplement}/apps"));
    for target in ["host", "runner", "gates", "identity", "roles"] {
        let mut args = base.clone();
        args.extend(["--target".into(), target.into()]);
        if target == "roles" {
            args.extend(["--output".into(), "type=local,dest=/workspace/roles".into()]);
        } else {
            args.extend([
                "--tag".into(),
                format!("{}/wamn-{target}:{CHART_VERSION}", request.registry),
                format!("--label=wamn.dev/source-head={head}"),
                "--label=wamn.dev/build-profile=release".into(),
            ]);
        }
        args.push(".".into());
        steps.push(json!({"name": "gcr.io/cloud-builders/docker", "args": args, "env": ["DOCKER_BUILDKIT=1"]}));
    }
    // Every build finishes before the first push, as on the local path.
    for target in ["host", "runner", "gates", "identity"] {
        steps.push(json!({"name": "gcr.io/cloud-builders/docker", "args": ["push", format!("{}/wamn-{target}:{CHART_VERSION}", request.registry)]}));
    }
    // The token is captured in memory, then written privately to the shared
    // credential volume. Neither the token nor its encoded form is logged.
    let auth = r"import base64, json, os, subprocess, sys
os.umask(0o077)
token = subprocess.check_output(['gcloud', 'auth', 'print-access-token'], stderr=subprocess.DEVNULL).decode().strip()
with open('/cloud-auth/config.json', 'w') as output:
    json.dump({'auths': {sys.argv[1]: {'auth': base64.b64encode(('oauth2accesstoken:' + token).encode()).decode()}}}, output)
";
    let volume = json!([{"name": "cloud-auth", "path": "/cloud-auth"}]);
    steps.push(json!({
        "name": "gcr.io/google.com/cloudsdktool/cloud-sdk:slim", "entrypoint": "python3",
        "args": ["-c", auth, request.registry.split('/').next().context("registry host")?],
        "volumes": volume,
    }));
    for (file, role) in [
        ("http_route.wasm", "http"),
        ("materializer.wasm", "materializer"),
    ] {
        steps.push(json!({
            // This release uses the same wash-runtime revision as Cargo.toml.
            "name": "ghcr.io/wasmcloud/wash:2.10.3", "entrypoint": "/usr/local/bin/wash",
            "args": ["oci", "push", format!("{}/wamn/{role}:{CHART_VERSION}", request.registry), format!("/workspace/roles/{file}")],
            "env": ["DOCKER_CONFIG=/cloud-auth"], "volumes": volume,
        }));
    }
    Ok(json!({"steps": steps, "options": {"machineType": "E2_HIGHCPU_8"}}))
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
        ];
        assert_eq!(
            build_location(&processes, 10, 3.0, 3.0),
            BuildLocation::Local
        );
        processes.push(process(
            14,
            &["wamn-ctl", "--log-level=info", "build-platform"],
        ));
        assert_eq!(
            build_location(&processes, 10, 2.0, 3.0),
            BuildLocation::Cloud
        );
        for spend_usd in [3.0, 3.1] {
            assert_eq!(
                build_location(&processes, 10, spend_usd, 3.0),
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

    /// `program args…` of each command `build_then_push` runs; `fail` fails
    /// the command that contains it, and `image inspect` reports no digest.
    async fn record(roles: &Path, fail: &str) -> (anyhow::Error, Vec<String>) {
        let mut calls = Vec::new();
        let error = build_then_push(
            &BuildPlatformRequest {
                cluster: None,
                repository: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
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
                calls.push(call.clone());
                ensure!(!call.contains(fail), "{call} failed");
                Ok(String::new())
            },
        )
        .await
        .expect_err("the run fails");
        (error, calls)
    }

    /// Stamp from a fake registry whose tags all resolve except those that
    /// contain `absent`, and return the result with every command run.
    async fn stamp_recorded(absent: &str, out: &Path) -> (anyhow::Result<PathBuf>, Vec<String>) {
        let digest = format!("sha256:{}", "a".repeat(64));
        let mut calls = Vec::new();
        let stamped = stamp_from_registry(
            &BuildPlatformRequest {
                cluster: None,
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
    async fn stamp_only_stamps_the_six_registry_digests_and_builds_nothing() {
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
        assert_eq!(calls.len(), 4, "{calls:?}");
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
        let roles =
            std::env::temp_dir().join(format!("wamn-build-platform-order-{}", std::process::id()));
        std::fs::create_dir_all(&roles).expect("create the roles directory");
        for file in ["http_route.wasm", "materializer.wasm"] {
            std::fs::write(roles.join(file), b"\0asm").expect("write a role component");
        }
        let (error, calls) = record(&roles, "\0").await;
        let _ = std::fs::remove_dir_all(&roles);
        assert!(error.to_string().contains("no pushed digest"), "{error}");
        let first_push = calls
            .iter()
            .position(|call| call.starts_with("docker push"))
            .expect("a push ran");
        let builds = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.starts_with("docker build"))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(builds.len(), 5, "{calls:?}");
        assert!(builds.iter().all(|index| *index < first_push), "{calls:?}");
        let key = checkout_key(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")).unwrap();
        assert!(
            builds.iter().all(
                |index| calls[*index].contains(&format!("--build-arg WAMN_CHECKOUT_KEY={key}"))
            ),
            "{calls:?}"
        );
    }
}
