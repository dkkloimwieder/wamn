//! Invariant 1: the system cluster is absent from every request path.
//!
//! Only the T1 cluster definition itself (`wamn-sysdb.yaml`) may name the
//! system cluster or its database. No data-plane workload manifest under
//! `deploy/` may.

use std::path::Path;

use super::Problems;

/// The one manifest that defines the system cluster.
const ALLOWLIST: &[&str] = &["wamn-sysdb.yaml"];

/// The fewest manifests a real `deploy/` tree holds. A lower count means the
/// walk went vacuous (the pre-tiering flat `read_dir` bug class).
const MINIMUM_MANIFESTS: usize = 10;

pub(super) fn check(root: &Path, problems: &mut Problems) {
    let mut scanned = 0usize;
    let mut stack = vec![root.join("deploy")];
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
                .file_name()
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
            // Comments are stripped first, because the invariant is about the
            // REQUEST PATH and a comment is not one. Naming `wamn-sysdb.yaml`
            // as the R8b precedent while documenting a Secret reference is
            // exactly the prose a credential-hygiene change should carry, and
            // tripping on it pushes the next author toward the ALLOWLIST --
            // which is the one edit that genuinely weakens this lint.
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

#[cfg(test)]
mod tests {
    use super::{MINIMUM_MANIFESTS, Problems, check};

    /// A clean manifest tree passes, and one manifest that names the system
    /// database fails with that file named.
    #[test]
    fn the_lint_fails_on_one_system_cluster_reference_and_passes_a_clean_tree() {
        let root = std::env::temp_dir().join(format!("wamn-repo-policy-{}", std::process::id()));
        let deploy = root.join("deploy/workloads");
        std::fs::create_dir_all(&deploy).expect("create the fixture tree");
        for index in 0..MINIMUM_MANIFESTS {
            std::fs::write(
                deploy.join(format!("clean-{index}.yaml")),
                "kind: Deployment\n# wamn-sysdb is named only in this comment\n",
            )
            .expect("write a clean manifest");
        }
        std::fs::write(root.join("deploy/wamn-sysdb.yaml"), "name: wamn-sysdb\n")
            .expect("write the allowed cluster definition");

        let mut clean = Problems::default();
        check(&root, &mut clean);

        std::fs::write(deploy.join("host.yaml"), "env:\n  - value: wamn_system\n")
            .expect("write the offending manifest");
        let mut offending = Problems::default();
        check(&root, &mut offending);
        std::fs::remove_dir_all(&root).expect("remove the fixture tree");

        assert!(clean.0.is_empty(), "a clean tree must pass: {:?}", clean.0);
        assert_eq!(offending.0.len(), 1, "{:?}", offending.0);
        assert!(offending.0[0].contains("host.yaml"), "{:?}", offending.0);
    }
}
