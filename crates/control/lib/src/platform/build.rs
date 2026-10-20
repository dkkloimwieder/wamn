//! Build environment packages and stamp the platform chart in the install directory.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::delivery::environment::EnvironmentFile;

use super::document::{ClusterDocument, Provider};
use super::steps::read_platform_version;

/// Build or stamp step 4.6, unless the work directory already holds this image set.
///
/// # Errors
///
/// Returns the first file or build error, including the missing environment file.
pub async fn apply(
    document: &ClusterDocument,
    version: &str,
    repository: &Path,
    environments: &Path,
    directory: &Path,
) -> anyhow::Result<bool> {
    let packages = packages(environments)?;
    let chart = directory.join("release-chart");
    if stamped(&chart, &document.registry, version)? {
        return Ok(false);
    }
    let repository = std::fs::canonicalize(repository).context("find the repository")?;
    let directory = std::fs::canonicalize(directory).context("find the install directory")?;
    let docker = docker_configuration(document, &directory)?;
    build_packages(&repository, &packages, &docker, &document.cluster_name).await?;
    // Stamp-only reads the local UI bundle. The runbook builds it before stamping.
    run(Command::new("pnpm").current_dir(&repository).args([
        "--filter",
        "@wamn/platform-ui",
        "build",
    ]))
    .await?;
    let chart = directory.join("release-chart");
    if chart.exists() {
        std::fs::remove_dir_all(&chart).context("remove the old install chart")?;
    }
    let mut command = Command::new(repository.join("target/debug/wamn-ctl"));
    command
        .current_dir(&repository)
        .env("DOCKER_CONFIG", &docker)
        .args(platform_arguments(&document.registry, &chart, true));
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start build-platform --stamp-only from target/debug/wamn-ctl")?;
    if build_after_stamp(
        output.status.success(),
        &String::from_utf8_lossy(&output.stderr),
    ) {
        run(Command::new(repository.join("target/debug/wamn-ctl"))
            .current_dir(&repository)
            .env("DOCKER_CONFIG", &docker)
            .args(platform_arguments(&document.registry, &chart, false)))
        .await?;
    } else {
        successful(output, "build-platform --stamp-only")?;
    }
    ensure!(
        stamped(&chart, &document.registry, version)?,
        "build-platform did not stamp every image of {version} into {}",
        chart.display()
    );
    Ok(true)
}

fn platform_arguments(registry: &str, chart: &Path, stamp_only: bool) -> Vec<OsString> {
    let mut arguments = vec!["build-platform".into()];
    if stamp_only {
        arguments.push("--stamp-only".into());
    }
    arguments.extend([
        "--registry".into(),
        registry.into(),
        "--out".into(),
        chart.as_os_str().to_owned(),
    ]);
    arguments
}

fn build_after_stamp(success: bool, stderr: &str) -> bool {
    !success && stderr.contains("the registry does not hold ") && stderr.contains(" images:")
}

fn packages(environments: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(environments) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("read the environments directory"),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut packages = Vec::new();
    for path in paths {
        if path.extension().is_none_or(|extension| extension != "k") {
            continue;
        }
        let path = path.with_extension("json");
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read the required environment file {}", path.display()))?;
        let file: EnvironmentFile = serde_json::from_slice(&bytes)
            .with_context(|| format!("read the environment file {}", path.display()))?;
        for package in file.packages {
            if !packages.contains(&package) {
                packages.push(package);
            }
        }
    }
    Ok(packages)
}

fn docker_configuration(document: &ClusterDocument, directory: &Path) -> anyhow::Result<PathBuf> {
    let directory = directory.join("docker");
    std::fs::create_dir_all(&directory).context("create the Docker configuration directory")?;
    let value = match document.provider {
        Provider::Kind => json!({}),
        Provider::Gcp => {
            let host = document.registry.split('/').next().unwrap_or_default();
            json!({"credHelpers": {host: "gcloud"}})
        }
    };
    std::fs::write(directory.join("config.json"), serde_json::to_vec(&value)?)
        .context("write the Docker credential helper configuration")?;
    Ok(directory)
}

fn pinned(image: &str, reference: &str) -> bool {
    image
        .strip_prefix(&format!("{reference}@sha256:"))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn stamped(chart: &Path, registry: &str, version: &str) -> anyhow::Result<bool> {
    if !chart.join("Chart.yaml").is_file() || !chart.join("values.yaml").is_file() {
        return Ok(false);
    }
    if read_platform_version(&chart.join("Chart.yaml"))? != version {
        return Ok(false);
    }
    let values: Value = serde_yaml::from_slice(&std::fs::read(chart.join("values.yaml"))?)
        .context("decode the stamped chart values")?;
    Ok(images_match(&values, registry, version))
}

fn images_match(values: &Value, registry: &str, version: &str) -> bool {
    let host = &values["runtime-operator"]["runtime"]["image"];
    let host_registry = host["registry"].as_str().unwrap_or_default();
    let repository = host["repository"].as_str().unwrap_or_default();
    let tag = host["tag"].as_str().unwrap_or_default();
    let host = if host_registry.is_empty() {
        format!("{repository}:{tag}")
    } else {
        format!("{host_registry}/{repository}:{tag}")
    };
    pinned(&host, &format!("{registry}/wamn-host:{version}"))
        && ["runner", "gates", "identity"].iter().all(|name| {
            pinned(
                values["platform"][name]["image"]
                    .as_str()
                    .unwrap_or_default(),
                &format!("{registry}/wamn-{name}:{version}"),
            )
        })
        && ["http", "materializer"].iter().all(|name| {
            pinned(
                values["platform"]["roles"][name]["image"]
                    .as_str()
                    .unwrap_or_default(),
                &format!("{registry}/wamn/{name}"),
            )
        })
}

struct PostgresContainer {
    name: String,
    docker: PathBuf,
    active: bool,
}

impl Drop for PostgresContainer {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        // Also clean up when the install future is cancelled.
        let _ = std::process::Command::new("docker")
            .env("DOCKER_CONFIG", &self.docker)
            .args(["rm", "-f", &self.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

async fn build_packages(
    repository: &Path,
    packages: &[PathBuf],
    docker: &Path,
    cluster: &str,
) -> anyhow::Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    let name = format!("{cluster}-build-pg-{}", std::process::id());
    run(Command::new("docker").env("DOCKER_CONFIG", docker).args([
        "run",
        "-d",
        "--name",
        &name,
        "-e",
        "POSTGRES_HOST_AUTH_METHOD=trust",
        "-p",
        "127.0.0.1:0:5432",
        "postgres:18",
    ]))
    .await?;
    let mut guard = PostgresContainer {
        name,
        docker: docker.into(),
        active: true,
    };
    let built = async {
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            let ready = Command::new("docker")
                .env("DOCKER_CONFIG", docker)
                .args([
                    "exec",
                    &guard.name,
                    "pg_isready",
                    "-h",
                    "127.0.0.1",
                    "-U",
                    "postgres",
                ])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
                .context("wait for the package build database")?;
            if ready.status.success() {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "the package build database did not become ready in 300 seconds"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let output = run(Command::new("docker").env("DOCKER_CONFIG", docker).args([
            "port",
            &guard.name,
            "5432/tcp",
        ]))
        .await?;
        let binding = std::str::from_utf8(&output.stdout)?.trim();
        let port: u16 = binding
            .strip_prefix("127.0.0.1:")
            .context("the package build database has no loopback port")?
            .parse()?;
        run(Command::new(repository.join("target/debug/wamn"))
            .current_dir(repository)
            .env("DOCKER_CONFIG", docker)
            .env(
                "DATABASE_URL",
                format!("postgres://postgres@127.0.0.1:{port}/postgres"),
            )
            .arg("build")
            .args(packages))
        .await?;
        Ok(())
    }
    .await;
    let removed =
        run(Command::new("docker")
            .env("DOCKER_CONFIG", docker)
            .args(["rm", "-f", &guard.name]))
        .await;
    guard.active = removed.is_err();
    built?;
    removed?;
    Ok(())
}

async fn run(command: &mut Command) -> anyhow::Result<Output> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("start {program}"))?;
    successful(output, &program)
}

fn successful(output: Output, program: &str) -> anyhow::Result<Output> {
    ensure!(
        output.status.success(),
        "{program} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_arguments_keep_registry_and_output_as_single_arguments() {
        for stamp in [false, true] {
            let mut expected = vec!["build-platform"];
            if stamp {
                expected.push("--stamp-only");
            }
            expected.extend([
                "--registry",
                "localhost:5001/wamn",
                "--out",
                "/var/tmp/install space/release-chart",
            ]);
            assert_eq!(
                platform_arguments(
                    "localhost:5001/wamn",
                    Path::new("/var/tmp/install space/release-chart"),
                    stamp
                ),
                expected.into_iter().map(OsString::from).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn only_missing_registry_images_trigger_a_build() {
        let missing = "Error: the registry does not hold 1 of the 0.1.36 images: localhost/wamn-runner (not found)";
        assert!(build_after_stamp(false, missing));
        assert!(!build_after_stamp(true, missing));
        for error in [
            "the chart output exists",
            "the platform UI build is absent",
            "start docker: permission denied",
        ] {
            assert!(!build_after_stamp(false, error));
        }
    }

    #[test]
    fn skip_requires_every_pinned_image_of_this_registry_and_chart_version() {
        let digest = "a".repeat(64);
        let mut values = json!({
            "runtime-operator": {"runtime": {"image": {"repository":"localhost:5001/wamn-host","tag":format!("1.2.3@sha256:{digest}")}}},
            "platform": {}
        });
        for name in ["runner", "gates", "identity"] {
            values["platform"][name] =
                json!({"image":format!("localhost:5001/wamn-{name}:1.2.3@sha256:{digest}")});
        }
        values["platform"]["roles"] = json!({});
        for name in ["http", "materializer"] {
            values["platform"]["roles"][name] =
                json!({"image":format!("localhost:5001/wamn/{name}@sha256:{digest}")});
        }
        assert!(images_match(&values, "localhost:5001", "1.2.3"));
        assert!(!images_match(&values, "localhost:5002", "1.2.3"));
        assert!(!images_match(&values, "localhost:5001", "1.2.4"));
        for pointer in [
            "/runtime-operator/runtime/image/tag",
            "/platform/runner/image",
            "/platform/gates/image",
            "/platform/identity/image",
            "/platform/roles/http/image",
            "/platform/roles/materializer/image",
        ] {
            let mut missing = values.clone();
            *missing.pointer_mut(pointer).unwrap() = json!("");
            assert!(
                !images_match(&missing, "localhost:5001", "1.2.3"),
                "{pointer}"
            );
        }
    }

    #[test]
    fn packages_come_from_the_existing_json_type_and_missing_json_names_its_path() {
        let directory =
            std::env::temp_dir().join(format!("wamn-install-packages-{}", std::process::id()));
        assert!(packages(&directory).unwrap().is_empty());
        std::fs::create_dir(&directory).unwrap();
        let kcl = directory.join("test--receiving--dev.k");
        std::fs::write(&kcl, "not read by package discovery").unwrap();
        let error = packages(&directory).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(kcl.with_extension("json").to_str().unwrap())
        );
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../deploy/gcp/environments/dkk--receiving--dev.json");
        std::fs::copy(source, kcl.with_extension("json")).unwrap();
        assert_eq!(
            packages(&directory).unwrap(),
            [PathBuf::from("apps/wamn_receiving")]
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
