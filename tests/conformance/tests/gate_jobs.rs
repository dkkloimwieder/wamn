//! Check the execution fields in the actual gate Job manifests.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const GATE_DIRECTORY: &str = "deploy/gates";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn discover_manifests(root: &Path) -> BTreeSet<String> {
    let directory = root.join(GATE_DIRECTORY);
    fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .map(|entry| {
            let entry = entry.expect("gate directory entry must be readable");
            let name = entry.file_name().to_string_lossy().into_owned();
            (entry.path(), name)
        })
        .filter(|(path, name)| path.is_file() && name.ends_with("-job.yaml"))
        .map(|(_, name)| format!("{GATE_DIRECTORY}/{name}"))
        .collect()
}

fn validate_job(path: &Path) -> Result<(), String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let job_document = source
        .split("\n---")
        .find(|document| document.lines().any(|line| line.trim() == "kind: Job"))
        .ok_or_else(|| format!("{} has no Job document", path.display()))?;
    if !job_document
        .lines()
        .any(|line| line.trim() == "apiVersion: batch/v1")
    {
        return Err(format!("{} Job is not batch/v1", path.display()));
    }
    if !job_document.lines().any(|line| line.trim() == "metadata:") {
        return Err(format!("{} Job has no metadata", path.display()));
    }

    let images: BTreeSet<_> = job_document
        .lines()
        .filter_map(|line| line.trim().strip_prefix("image:"))
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if images.is_empty() {
        return Err(format!("{} Job has no container image", path.display()));
    }

    let invocations: Vec<_> = job_document
        .lines()
        .filter(|line| {
            let line = line.trim();
            line.starts_with("command:") || line.starts_with("args:")
        })
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect();
    if invocations.is_empty() {
        return Err(format!("{} Job has no command or args", path.display()));
    }

    Ok(())
}

#[test]
fn every_gate_manifest_declares_a_runnable_job() {
    let root = repository_root();
    for manifest in discover_manifests(&root) {
        validate_job(&root.join(manifest)).unwrap_or_else(|error| panic!("{error}"));
    }
}
