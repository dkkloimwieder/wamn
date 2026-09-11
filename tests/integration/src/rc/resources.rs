//! Exact ownership and cleanup of the RC cluster, images and private files.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use super::{CLUSTER, checked, kubectl, save};
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;

pub(super) struct Resources {
    pub repository: PathBuf,
    pub work: PathBuf,
    pub evidence: PathBuf,
    pub source: String,
    pub lifecycle: PathBuf,
    pub host_image: String,
    pub gates_image: String,
    pub postgres_image: String,
    pub owned: bool,
}

pub(super) async fn clean_source(repository: &Path) -> anyhow::Result<()> {
    let status = checked(Command::new("git").current_dir(repository).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--",
        ".",
        ":(exclude).beads/issues.jsonl",
        ":(exclude).beads/interactions.jsonl",
    ]))
    .await?;
    ensure!(
        status.is_empty(),
        "commit the final source before running RC"
    );
    Ok(())
}

pub(super) async fn prepare(
    repository: &Path,
    evidence: &Path,
    source: &str,
) -> anyhow::Result<Resources> {
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "RC debug images require an x86_64 build host"
    );
    clean_source(repository).await?;
    ensure!(
        evidence.is_absolute() && !evidence.exists(),
        "the RC evidence directory must be a new absolute path"
    );
    let common = String::from_utf8(
        checked(Command::new("git").current_dir(repository).args([
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ]))
        .await?,
    )?;
    let root = Path::new(common.trim())
        .parent()
        .context("Git common directory has a repository parent")?;
    let parent = evidence
        .parent()
        .context("evidence directory has a parent")?
        .canonicalize()?;
    ensure!(
        parent.starts_with(root.join("docs/perf").canonicalize()?),
        "RC evidence must be under the main repository docs/perf"
    );
    let evidence = parent.join(
        evidence
            .file_name()
            .context("evidence directory has a name")?,
    );
    let tag = format!("rc-{}-{}-debug", &source[..12], std::process::id());
    let lifecycle = repository.join("tools/rc-gate-run");
    let host_image = format!("wamn-host:{tag}");
    let gates_image = format!("wamn-gates:{tag}");
    let postgres_image = format!("wamn-postgres:rc-{}-{}", &source[..12], std::process::id());
    DirBuilder::new().mode(0o700).create(&evidence)?;
    let work = std::env::temp_dir().join(format!("wamn-rc-{}", uuid::Uuid::new_v4().simple()));
    DirBuilder::new().mode(0o700).create(&work)?;
    let resources = Resources {
        repository: repository.to_owned(),
        work,
        evidence,
        source: source.into(),
        lifecycle,
        host_image,
        gates_image,
        postgres_image,
        owned: false,
    };
    save(
        &resources,
        "source.json",
        &json!({"source":source,"cluster":CLUSTER,"build_profile":"debug"}),
    )?;
    recorded(
        &resources,
        "preflight-docker",
        Command::new(&resources.lifecycle).arg("docker-version"),
    )
    .await?;
    let clusters = recorded(
        &resources,
        "preflight-clusters",
        Command::new(&resources.lifecycle).arg("clusters"),
    )
    .await?;
    ensure!(
        !String::from_utf8_lossy(&clusters)
            .lines()
            .any(|line| line == CLUSTER),
        "refusing pre-existing RC cluster"
    );
    let containers = recorded(
        &resources,
        "preflight-containers",
        Command::new(&resources.lifecycle).arg("containers"),
    )
    .await?;
    for name in ["wamn-rc-postgres", "wamn-rc-nats"] {
        ensure!(
            !String::from_utf8_lossy(&containers)
                .lines()
                .any(|line| line == name),
            "refusing pre-existing RC container {name}"
        );
    }
    let images = recorded(
        &resources,
        "preflight-images",
        Command::new(&resources.lifecycle).arg("images"),
    )
    .await?;
    for image in [
        &resources.host_image,
        &resources.gates_image,
        &resources.postgres_image,
    ] {
        ensure!(
            !String::from_utf8_lossy(&images)
                .lines()
                .any(|line| line == image),
            "refusing pre-existing RC image {image}"
        );
    }
    Ok(resources)
}

pub(super) async fn diagnostics(resources: &Resources) {
    if !resources.owned {
        return;
    }
    for (name, args) in [
        ("failure-pods.json", vec!["get", "pods", "-A", "-o", "json"]),
        ("failure-jobs.json", vec!["get", "jobs", "-A", "-o", "json"]),
        (
            "failure-events.json",
            vec!["get", "events", "-A", "-o", "json"],
        ),
        (
            "failure-host.log",
            vec![
                "-n",
                CLUSTER,
                "logs",
                "deployment/hostgroup-default",
                "--all-containers=true",
                "--tail=200",
            ],
        ),
    ] {
        if let Ok(bytes) = checked(kubectl(resources).args(args)).await {
            let _ = write_private(&resources.evidence.join(name), &bytes);
        }
    }
}

pub(super) async fn cleanup(resources: &mut Resources) -> anyhow::Result<()> {
    if resources.owned {
        recorded(
            resources,
            "remove",
            Command::new(&resources.lifecycle)
                .arg("remove")
                .arg(CLUSTER)
                .arg(&resources.work)
                .arg(&resources.host_image)
                .arg(&resources.gates_image)
                .arg(&resources.postgres_image),
        )
        .await?;
        let clusters = recorded(
            resources,
            "cleanup-clusters",
            Command::new(&resources.lifecycle).arg("clusters"),
        )
        .await?;
        ensure!(
            !String::from_utf8_lossy(&clusters)
                .lines()
                .any(|line| line == CLUSTER),
            "RC cluster survived cleanup"
        );
        let containers = recorded(
            resources,
            "cleanup-containers",
            Command::new(&resources.lifecycle).arg("containers"),
        )
        .await?;
        for name in ["wamn-rc-postgres", "wamn-rc-nats"] {
            ensure!(
                !String::from_utf8_lossy(&containers)
                    .lines()
                    .any(|line| line == name),
                "owned container survived cleanup"
            );
        }
        let images = recorded(
            resources,
            "cleanup-images",
            Command::new(&resources.lifecycle).arg("images"),
        )
        .await?;
        for image in [
            &resources.host_image,
            &resources.gates_image,
            &resources.postgres_image,
        ] {
            ensure!(
                !String::from_utf8_lossy(&images)
                    .lines()
                    .any(|line| line == image),
                "owned image survived cleanup"
            );
        }
    }
    resources.owned = false;
    fs::remove_dir_all(&resources.work)?;
    save(
        resources,
        "cleanup.json",
        &json!({"passed":true,"cluster":CLUSTER,"images":"exact","containers":["wamn-rc-postgres","wamn-rc-nats"]}),
    )
}

impl Drop for Resources {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::process::Command::new(&self.lifecycle)
                .arg("remove")
                .arg(CLUSTER)
                .arg(&self.work)
                .arg(&self.host_image)
                .arg(&self.gates_image)
                .arg(&self.postgres_image)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.work);
    }
}
pub(super) fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

fn hash_evidence(directory: &Path, result: &[u8]) -> anyhow::Result<()> {
    use sha2::Digest as _;
    fn collect(
        root: &Path,
        directory: &Path,
        rows: &mut Vec<(PathBuf, String)>,
    ) -> anyhow::Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                collect(root, &path, rows)?;
            } else if path != root.join("result.json")
                && path
                    .file_name()
                    .is_some_and(|name| name != "evidence.sha256")
            {
                rows.push((
                    path.strip_prefix(root)?.to_owned(),
                    hex::encode(sha2::Sha256::digest(fs::read(&path)?)),
                ));
            }
        }
        Ok(())
    }
    let mut rows = Vec::new();
    collect(directory, directory, &mut rows)?;
    rows.push((
        PathBuf::from("result.json"),
        hex::encode(sha2::Sha256::digest(result)),
    ));
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let text = rows
        .into_iter()
        .map(|(path, digest)| format!("{digest}  {}\n", path.display()))
        .collect::<String>();
    write_private(&directory.join("evidence.sha256"), text.as_bytes())
}

pub(super) fn finish_result(directory: &Path, result: &Value) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(result)?;
    let hashes = hash_evidence(directory, &bytes);
    match &hashes {
        Ok(()) => write_private(&directory.join("result.json"), &bytes)?,
        Err(error) => {
            let mut failed = result.clone();
            failed["passed"] = json!(false);
            failed["capture_failure"] = json!(format!("{error:#}"));
            write_private(
                &directory.join("result.json"),
                &serde_json::to_vec_pretty(&failed)?,
            )?;
        }
    }
    hashes
}

pub(super) async fn recorded(
    resources: &Resources,
    name: &str,
    command: &mut Command,
) -> anyhow::Result<Vec<u8>> {
    let standard = command.as_std();
    let arguments = std::iter::once(standard.get_program())
        .chain(standard.get_args())
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let cwd = standard.get_current_dir();
    let stdout_path = resources.evidence.join(format!("{name}.log"));
    let stderr_path = resources.evidence.join(format!("{name}.stderr.log"));
    save(
        resources,
        &format!("{name}-command.json"),
        &json!({"argv":arguments,"cwd":cwd,"source":resources.source,
            "stdout":stdout_path,"stderr":stderr_path}),
    )?;
    let stdout = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&stdout_path)?;
    let stderr = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&stderr_path)?;
    let started = Instant::now();
    let status = command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true)
        .status()
        .await;
    save(
        resources,
        &format!("{name}-result.json"),
        &match &status {
            Ok(status) => json!({"exit_code":status.code(),"signal":status.signal(),
                "status":status.to_string(),"passed":status.success(),
                "elapsed_seconds":started.elapsed().as_secs_f64()}),
            Err(error) => json!({"exit_code":null,"passed":false,"failure":error.to_string(),
                "elapsed_seconds":started.elapsed().as_secs_f64()}),
        },
    )?;
    ensure!(
        status?.success(),
        "{name} failed; see its retained stdout and stderr logs"
    );
    Ok(fs::read(stdout_path)?)
}

#[cfg(test)]
mod tests;
