//! Guards workspace-owned dependency identities against package-local duplication.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_dependency_names(workspace_manifest: &Path) -> HashSet<String> {
    let source = std::fs::read_to_string(workspace_manifest).expect("read workspace Cargo.toml");
    let mut names = HashSet::new();
    let mut workspace_dependencies = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            workspace_dependencies = trimmed == "[workspace.dependencies]";
            continue;
        }
        if !workspace_dependencies || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((raw_name, _)) = trimmed.split_once('=') {
            names.insert(raw_name.trim().trim_matches('"').to_string());
        }
    }

    names
}

fn workspace_member_manifests(workspace_manifest: &Path) -> Vec<PathBuf> {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(workspace_manifest)
        .output()
        .expect("run cargo metadata for dependency-identity guard");
    assert!(
        output.status.success(),
        "cargo metadata failed for {}:\n{}",
        workspace_manifest.display(),
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata");
    let members: HashSet<&str> = metadata["workspace_members"]
        .as_array()
        .expect("workspace_members array")
        .iter()
        .map(|member| member.as_str().expect("workspace member id"))
        .collect();

    metadata["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .filter(|package| {
            members.contains(
                package["id"]
                    .as_str()
                    .expect("package id in cargo metadata"),
            )
        })
        .map(|package| {
            PathBuf::from(
                package["manifest_path"]
                    .as_str()
                    .expect("package manifest_path in cargo metadata"),
            )
        })
        .collect()
}

fn assert_workspace_inheritance(workspace_manifest: &Path, workspace_name: &str) {
    let governed_names = workspace_dependency_names(workspace_manifest);
    let mut violations = Vec::new();
    for manifest in workspace_member_manifests(workspace_manifest) {
        let source = std::fs::read_to_string(&manifest).expect("read member Cargo.toml");
        let mut dependency_table = false;

        for (line_index, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                let table = &trimmed[1..trimmed.len() - 1];
                dependency_table = matches!(
                    table.rsplit('.').next(),
                    Some("dependencies" | "dev-dependencies" | "build-dependencies")
                );
                continue;
            }
            if !dependency_table || trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let Some((raw_name, declaration)) = trimmed.split_once('=') else {
                continue;
            };
            let name = raw_name.trim().trim_matches('"');
            let compact: String = declaration.split_whitespace().collect();
            let governed = name.starts_with("wamn-")
                || governed_names.contains(name)
                || compact.contains("package=\"wamn-");
            if !governed {
                continue;
            }

            if !compact.contains("workspace=true") {
                violations.push(format!(
                    "{}:{}: `{name}` must inherit its {workspace_name} workspace identity",
                    manifest.display(),
                    line_index + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "duplicated governed dependency identities:\n{}",
        violations.join("\n")
    );
}

#[test]
fn governed_dependency_identities_are_workspace_owned() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tests/conformance lives two levels below the repository root");

    assert_workspace_inheritance(&repository.join("Cargo.toml"), "native");
    assert_workspace_inheritance(&repository.join("components/Cargo.toml"), "component");
}
