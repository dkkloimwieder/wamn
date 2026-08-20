//! Guards retirement of recurring REPLICA IDENTITY superuser authority.
//!
//! Commit c37cbc5e scheduled the existing one-shot repair command. The Job later
//! became suspended and unstartable after its fixture disappeared, while its
//! mutable CronJob spec still distributed a standing superuser credential.
//! wamn-0h0g.12.70 retires that scheduler and keeps the existing migration and
//! explicit operator callers.

use std::fs;
use std::path::{Path, PathBuf};

const RETIRED_CRONJOB: &str = "deploy/platform/replica-identity-reconcile.example.yaml";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn without_yaml_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once('#').map_or(line, |(contents, _)| contents))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn recurring_replica_identity_superuser_authority_is_absent() {
    let root = repository_root();
    let retired = root.join(RETIRED_CRONJOB);
    assert!(
        !retired.exists(),
        "retired recurring authority returned at {}",
        retired.display()
    );

    let platform = root.join("deploy/platform");
    for entry in fs::read_dir(&platform)
        .unwrap_or_else(|error| panic!("read {}: {error}", platform.display()))
    {
        let path = entry.expect("read platform directory entry").path();
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "yaml" | "yml"))
        {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let source = without_yaml_comments(&source);
        for document in source.split("\n---\n") {
            if !document.lines().any(|line| line.trim() == "kind: CronJob") {
                continue;
            }
            assert!(
                !document.contains("reconcile-replica-identity"),
                "{} schedules the one-shot replica-identity repair command",
                path.display()
            );
            assert!(
                !document.contains("postgres-fixture-superuser"),
                "{} distributes the fixture superuser through a recurring spec",
                path.display()
            );
        }
    }
}
