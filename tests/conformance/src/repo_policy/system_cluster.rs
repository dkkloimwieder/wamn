//! Invariant 1: the system cluster is absent from every request path.
//!
//! Only a definition of the cluster that holds the system database may name
//! the system cluster or its database. No data-plane workload manifest under
//! `deploy/` may.

use std::path::Path;

use super::Problems;

/// The manifests, below `deploy/`, that define the cluster of the system
/// database: the Google Cloud cluster that holds the system database
/// and every environment database
/// (`docs/history/gcp-deployment.md`).
const ALLOWLIST: &[&str] = &["infra/cnpg-cluster.yaml"];

/// The fewest manifests a real `deploy/` tree holds. A lower count means the
/// walk went vacuous (the pre-tiering flat `read_dir` bug class).
const MINIMUM_MANIFESTS: usize = 10;

pub(super) fn check(root: &Path, problems: &mut Problems) {
    let mut scanned = 0usize;
    let deploy = root.join("deploy");
    let mut stack = vec![deploy.clone()];
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                problems.push(format!("read {}: {error}", directory.display()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("yaml") {
                continue;
            }
            scanned += 1;
            let name = path
                .strip_prefix(&deploy)
                .ok()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if ALLOWLIST.contains(&name) {
                continue;
            }
            let body = match std::fs::read_to_string(&path) {
                Ok(body) => body,
                Err(error) => {
                    problems.push(format!("read {}: {error}", path.display()));
                    continue;
                }
            };
            // Comments do not declare database access.
            let rendered = body
                .lines()
                .map(|line| line.split_once('#').map_or(line, |(rendered, _)| rendered))
                .collect::<Vec<_>>()
                .join("\n");
            problems.require(
                !rendered.contains("wamn-sysdb") && !rendered.contains("wamn_system"),
                || {
                    format!(
                        "{} references the T1 system cluster or database (request-path-free \
                         invariant 1); only control-plane tooling may join the allowlist",
                        path.display()
                    )
                },
            );
        }
    }
    problems.require(scanned >= MINIMUM_MANIFESTS, || {
        format!("the deploy/ manifest walk saw only {scanned} yaml files")
    });
}
