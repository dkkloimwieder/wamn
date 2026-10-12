//! The platform build step (docs/plan/platform-deploy.md §9.3, R15).
//!
//! It builds the host, runner, gates and identity images and the two role
//! components from the root `Dockerfile`, pushes each to one registry, and
//! stamps their digests into a copy of the release chart's defaults. That
//! chart is the one source of an image set.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio::process::Command;

use crate::release_chart::{CHART_VERSION, ImageSet, QualificationImages, stamp};

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

/// Build and push the platform images, then stamp them into a copy of the
/// release chart at `request.out`. Every image is tagged with the chart
/// version and pinned by its pushed digest.
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
    let head = output(Command::new("git").current_dir(&request.repository).args([
        "rev-parse",
        "--verify",
        "HEAD",
    ]))
    .await?;
    let mut pushed = Vec::new();
    for target in ["host", "runner", "gates", "identity"] {
        pushed.push(push_image(request, target, head.trim()).await?);
    }
    let [host, runner, gates, identity] = <[String; 4]>::try_from(pushed)
        .map_err(|_| anyhow::anyhow!("four platform images were pushed"))?;
    let roles = std::env::temp_dir().join(format!("wamn-roles-{}", std::process::id()));
    let built = output(
        docker(&request.repository)
            .args(["--target", "roles", "--output"])
            .arg(format!("type=local,dest={}", roles.display()))
            .arg(&request.repository),
    )
    .await;
    let pushed = match built {
        Ok(_) => push_roles(&request.registry, &roles).await,
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_dir_all(&roles);
    let (http, materializer) = pushed?;
    stamp(
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
    )
}

/// `docker build` of one target of the root `Dockerfile`, for amd64. The
/// runner's cases compile against the package outputs of `wamn build`, the
/// named context `apps-target`.
fn docker(repository: &Path) -> Command {
    let mut command = Command::new("docker");
    command
        .args(["build", "--platform=linux/amd64", "--provenance=false"])
        .arg("--build-context")
        .arg(format!(
            "apps-target={}",
            repository.join("apps/target/wamn").display()
        ))
        .arg("--file")
        .arg(repository.join("Dockerfile"));
    command
}

/// Build `target` as `<registry>/wamn-<target>:<chart version>`, push it and
/// return it pinned by the digest the registry holds.
async fn push_image(
    request: &BuildPlatformRequest,
    target: &str,
    head: &str,
) -> anyhow::Result<String> {
    let reference = format!("{}/wamn-{target}:{CHART_VERSION}", request.registry);
    output(
        docker(&request.repository)
            .args(["--target", target, "--tag", &reference])
            .arg(format!("--label=wamn.dev/source-head={head}"))
            .arg("--label=wamn.dev/build-profile=release")
            .arg(&request.repository),
    )
    .await?;
    output(Command::new("docker").args(["push", &reference])).await?;
    let digests = output(Command::new("docker").args([
        "image",
        "inspect",
        "--format",
        "{{range .RepoDigests}}{{println .}}{{end}}",
        &reference,
    ]))
    .await?;
    let prefix = format!("{}/wamn-{target}@", request.registry);
    let digest = digests
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .filter(|digest| digest.starts_with("sha256:"))
        .with_context(|| format!("docker reports no pushed digest for {reference}"))?;
    Ok(format!("{reference}@{digest}"))
}

/// Push the http and materializer components exported to `directory` as
/// `<registry>/wamn/<role>`, and return each pinned by digest.
async fn push_roles(registry: &str, directory: &Path) -> anyhow::Result<(String, String)> {
    let host = registry.split(':').next().unwrap_or_default();
    let insecure = matches!(host, "localhost" | "127.0.0.1");
    let mut pushed = Vec::new();
    for (file, role) in [
        ("http_route.wasm", "http"),
        ("materializer.wasm", "materializer"),
    ] {
        let bytes = std::fs::read(directory.join(file)).with_context(|| format!("read {file}"))?;
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
}
