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

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use wamn_execution_contract::canonical_json_bytes;

use crate::release_chart::{CHART_VERSION, ImageSet, QualificationImages, stamp};

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
    run(Command::new("pnpm").current_dir(&request.repository).args([
        "--filter",
        "@wamn/platform-ui",
        "build",
    ]))
    .await?;
    let head = run(Command::new("git").current_dir(&request.repository).args([
        "rev-parse",
        "--verify",
        "HEAD",
    ]))
    .await?;
    let mut references = Vec::new();
    for target in ["host", "runner", "gates", "identity"] {
        let reference = format!("{}/wamn-{target}:{CHART_VERSION}", request.registry);
        run(docker(&request.repository)?
            .args(["--target", target, "--tag", &reference])
            .arg(format!("--label=wamn.dev/source-head={}", head.trim()))
            .arg("--label=wamn.dev/build-profile=release")
            .arg(&request.repository))
        .await?;
        references.push((target, reference));
    }
    run(docker(&request.repository)?
        .args(["--target", "roles", "--output"])
        .arg(format!("type=local,dest={}", roles.display()))
        .arg(&request.repository))
    .await?;
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
    command
        .args(["build", "--platform=linux/amd64", "--provenance=false"])
        .arg("--build-arg")
        .arg(format!("WAMN_CHECKOUT_KEY={}", checkout_key(repository)?))
        .arg("--build-context")
        .arg(format!(
            "apps-target={}",
            repository.join("apps/target/wamn").display()
        ))
        .arg("--file")
        .arg(repository.join("Dockerfile"));
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
