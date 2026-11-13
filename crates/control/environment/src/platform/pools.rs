//! Wake and sleep the GCP node pools (docs/operations/gcp.md sections 5.1 and 5.2).

use chrono::{SecondsFormat, Utc};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use tokio::process::Command;

use super::document::{ClusterDocument, Provider};
use super::project::ProjectDocument;
use super::provider;
use wamn_kubernetes::kubectl;

/// The pool size a verb sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Every pool gets its declared `size`, the pool that holds PostgreSQL first.
    Wake,
    /// Every pool gets 0 nodes.
    Sleep,
}

/// The PostgreSQL pool label. Wake resizes this pool first.
const POSTGRES_LABEL: &str = "role=postgres";

/// Resize every pool of `project_file` in one `gcloud` call for each pool.
///
/// After a wake, it waits for pod `wamn-pg-1` to be Ready (900 s) and for
/// statefulset `wamn-nats` to roll out (600 s). `progress` receives one line
/// for each pool and each wait.
///
/// # Errors
///
/// Returns an error for a type document, a project that names another
/// cluster, or a failed gcloud, kubectl, or progress write.
pub async fn resize(
    document: &ClusterDocument,
    project_file: &Path,
    direction: Direction,
    work_directory: &Path,
    mut progress: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    refuse_kind(document)?;
    let project = ProjectDocument::compile(project_file)?;
    let resizes = resize_arguments(document, &project, direction)?;
    let _lock = take_resize_lock(work_directory, &document.cluster_name)?;
    for (pool, size, arguments) in &resizes {
        let started = Instant::now();
        provider::run(Command::new("gcloud").args(arguments)).await?;
        progress(&format!(
            "pool {pool}: {size} nodes {:.1}s",
            started.elapsed().as_secs_f64()
        ))?;
    }
    match direction {
        Direction::Wake => {
            let kubeconfig = work_directory.join(format!("{}.kubeconfig", document.cluster_name));
            let context = document.provider.kubeconfig(document, &kubeconfig).await?;
            let started = Instant::now();
            postgres_ready(&kubeconfig, &context).await?;
            progress(&format!(
                "pod wamn-pg-1: Ready {:.1}s",
                started.elapsed().as_secs_f64()
            ))?;
            let started = Instant::now();
            kubectl::kubectl(
                &kubeconfig,
                &context,
                "platform",
                &[
                    "rollout",
                    "status",
                    "statefulset/wamn-nats",
                    "--timeout=600s",
                ],
                None,
            )
            .await?;
            progress(&format!(
                "statefulset wamn-nats: rolled out {:.1}s",
                started.elapsed().as_secs_f64()
            ))?;
        }
        Direction::Sleep => {
            let pools: Vec<_> = resizes.iter().map(|(pool, _, _)| pool.as_str()).collect();
            progress(&format!("pools set to 0: {}", pools.join(", ")))?;
        }
    }
    Ok(())
}

/// The `Cluster` keeps its old Ready condition while its pod is gone,
/// so wait for the pod itself, within one 900 s deadline.
async fn postgres_ready(kubeconfig: &Path, context: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_mins(15);
    while kubectl::get::<Value>(kubeconfig, context, "platform", "pod", "wamn-pg-1")
        .await?
        .is_none()
    {
        if Instant::now() >= deadline {
            bail!("pod platform/wamn-pg-1 does not exist after 900s");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
    kubectl::wait(
        kubeconfig,
        context,
        "platform",
        "pod",
        "wamn-pg-1",
        "condition=Ready",
        &format!("{}s", remaining.max(1)),
    )
    .await
}

fn refuse_kind(document: &ClusterDocument) -> anyhow::Result<()> {
    ensure!(
        document.provider == Provider::Gcp,
        "platform wake and sleep resize GCP node pools, and cluster {} is a kind cluster",
        document.cluster_name
    );
    Ok(())
}

/// Each pool's name, target size, and gcloud arguments, in resize order.
fn resize_arguments(
    document: &ClusterDocument,
    project: &ProjectDocument,
    direction: Direction,
) -> anyhow::Result<Vec<(String, u32, Vec<String>)>> {
    refuse_kind(document)?;
    ensure!(
        (&project.project_id, &project.zone, &project.cluster_name)
            == (&document.project_id, &document.zone, &document.cluster_name),
        "project.k declares cluster {} in {}/{}, and cluster.k names cluster {} in {}/{}",
        project.cluster_name,
        project.project_id,
        project.zone,
        document.cluster_name,
        document.project_id,
        document.zone
    );
    let mut pools: Vec<_> = project.node_pools.iter().collect();
    if direction == Direction::Wake {
        pools.sort_by_key(|pool| pool.label != POSTGRES_LABEL);
    }
    Ok(pools
        .into_iter()
        .map(|pool| {
            let size = match direction {
                Direction::Wake => pool.size,
                Direction::Sleep => 0,
            };
            let arguments = [
                "container",
                "clusters",
                "resize",
                &document.cluster_name,
                "--node-pool",
                &pool.name,
                "--num-nodes",
                &size.to_string(),
                "--zone",
                &document.zone,
                "--project",
                &document.project_id,
                "--quiet",
            ]
            .map(str::to_owned)
            .to_vec();
            (pool.name.clone(), size, arguments)
        })
        .collect())
}

/// The open file keeps the resize lock held until this guard is dropped.
#[derive(Debug)]
#[must_use = "keep the guard alive for the entire resize"]
pub struct ResizeLock {
    file: File,
}

impl Drop for ResizeLock {
    fn drop(&mut self) {
        // A concurrent process spawn can briefly inherit the open file before exec.
        // Release the lock even while that child still holds its descriptor.
        let _ = self.file.unlock();
    }
}

/// Return `$HOME/.cache/wamn-resize` without creating it.
///
/// # Errors
///
/// Returns an error if HOME is unset.
pub fn work_directory() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is unset")?;
    Ok(PathBuf::from(home).join(".cache/wamn-resize"))
}

/// Take the resize lock immediately, or refuse with the current holder.
///
/// # Errors
///
/// Returns an error if the lock is held or a filesystem operation fails.
pub fn take_resize_lock(directory: &Path, cluster: &str) -> anyhow::Result<ResizeLock> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create the resize directory {}", directory.display()))?;
    let path = directory.join(format!("resize-{cluster}.lock"));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open the resize lock {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut holder = String::new();
            file.read_to_string(&mut holder)
                .with_context(|| format!("read the resize lock holder {}", path.display()))?;
            bail!(
                "resize lock {} is held by {:?}",
                path.display(),
                holder.trim_end()
            );
        }
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("lock {}", path.display()));
        }
    }
    let holder = format!(
        "pid={} started={}\n",
        std::process::id(),
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    file.set_len(0)
        .with_context(|| format!("clear the resize lock holder {}", path.display()))?;
    file.write_all(holder.as_bytes())
        .with_context(|| format!("write the resize lock holder {}", path.display()))?;
    Ok(ResizeLock { file })
}

#[cfg(test)]
mod tests {
    use super::{Direction, resize_arguments, take_resize_lock};
    use crate::platform::document::{ClusterDocument, Provider};
    use crate::platform::project::ProjectDocument;
    use std::path::Path;

    fn committed() -> (ClusterDocument, ProjectDocument) {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/gcp");
        (
            ClusterDocument::compile(&directory.join("cluster.k")).expect("compile cluster.k"),
            ProjectDocument::compile(&directory.join("project.k")).expect("compile project.k"),
        )
    }

    fn resize(pool: &str, size: &str) -> Vec<String> {
        [
            "container",
            "clusters",
            "resize",
            "wamn",
            "--node-pool",
            pool,
            "--num-nodes",
            size,
            "--zone",
            "us-central1-a",
            "--project",
            "wamn-dev",
            "--quiet",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    #[test]
    fn wake_sets_each_declared_size_with_the_postgres_pool_first() {
        let (document, mut project) = committed();
        project.node_pools[0].size = 2;
        let plan = resize_arguments(&document, &project, Direction::Wake).unwrap();
        assert_eq!(
            plan,
            [
                ("pg".into(), 1, resize("pg", "1")),
                ("main".into(), 2, resize("main", "2")),
                ("bench".into(), 1, resize("bench", "1")),
            ]
        );
    }

    #[test]
    fn sleep_sets_every_pool_to_zero() {
        let (document, project) = committed();
        let plan = resize_arguments(&document, &project, Direction::Sleep).unwrap();
        assert_eq!(
            plan,
            [
                ("main".into(), 0, resize("main", "0")),
                ("bench".into(), 0, resize("bench", "0")),
                ("pg".into(), 0, resize("pg", "0")),
            ]
        );
    }

    #[test]
    fn a_kind_document_refuses_both_verbs() {
        let (mut document, project) = committed();
        document.provider = Provider::Kind;
        for direction in [Direction::Wake, Direction::Sleep] {
            assert_eq!(
                resize_arguments(&document, &project, direction)
                    .unwrap_err()
                    .to_string(),
                "platform wake and sleep resize GCP node pools, and cluster wamn is a kind cluster"
            );
        }
    }

    #[test]
    fn a_project_for_another_cluster_is_refused() {
        let (document, mut project) = committed();
        project.cluster_name = "other".into();
        assert_eq!(
            resize_arguments(&document, &project, Direction::Sleep)
                .unwrap_err()
                .to_string(),
            "project.k declares cluster other in wamn-dev/us-central1-a, and cluster.k names cluster wamn in wamn-dev/us-central1-a"
        );
    }
    #[test]
    fn resize_lock_serializes_one_cluster_and_releases_on_drop() {
        let directory = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let first = take_resize_lock(directory.path(), "one").unwrap();
        let path = directory.path().join("resize-one.lock");
        let holder = std::fs::read_to_string(&path).unwrap();
        let error = take_resize_lock(directory.path(), "one")
            .unwrap_err()
            .to_string();
        assert!(error.contains(holder.trim_end()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), holder);
        let other = take_resize_lock(directory.path(), "two").unwrap();
        let inherited = first.file.try_clone().unwrap();
        drop(first);
        let again = take_resize_lock(directory.path(), "one").unwrap();
        drop((other, again, inherited));
    }
}
