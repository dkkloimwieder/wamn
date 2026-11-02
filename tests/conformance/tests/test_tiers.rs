//! Every executable Cargo target has exactly one execution tier.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

type Target = (String, String, String);

fn key(row: &Value) -> Target {
    (
        row["workspace"].as_str().unwrap().into(),
        row["package"].as_str().unwrap().into(),
        row["target"].as_str().unwrap().into(),
    )
}

fn assignments(policy: &Value, targets: &BTreeSet<Target>) -> Result<(), String> {
    let mut rows = BTreeMap::new();
    for row in policy["targets"].as_array().unwrap() {
        let target = key(row);
        let tier = row["tier"].as_str().unwrap();
        if !["unit", "contract", "live-postgres", "live-nats", "kind"].contains(&tier) {
            return Err(format!("unknown tier {tier}: {target:?}"));
        }
        if !targets.contains(&target) {
            return Err(format!("row has no executable target: {target:?}"));
        }
        if rows.insert(target.clone(), tier).is_some() {
            return Err(format!("duplicate target: {target:?}"));
        }
    }
    for target in targets {
        if !rows.contains_key(target) {
            return Err(format!("target has no row: {target:?}"));
        }
    }
    for suite in policy["suites"].as_array().unwrap() {
        for target in suite["target"].as_array().unwrap() {
            let target = (
                suite["workspace"].as_str().unwrap().into(),
                suite["package"].as_str().unwrap().into(),
                target.as_str().unwrap().into(),
            );
            if rows.get(&target).copied() != suite["tier"].as_str() {
                return Err(format!("suite {} disagrees with {target:?}", suite["id"]));
            }
        }
    }
    Ok(())
}

fn policy(root: &Path) -> Value {
    let output = Command::new("python3")
        .args([
            "-c",
            "import json,sys,tomllib; print(json.dumps(tomllib.load(open(sys.argv[1], 'rb'))))",
        ])
        .arg(root.join("tests/selection.toml"))
        .output()
        .expect("read the selection file");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn real_metadata_has_exactly_one_tier_per_executable_target() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut targets = BTreeSet::new();
    for (workspace, manifest) in [
        ("root", "Cargo.toml"),
        ("apps", "apps/Cargo.toml"),
        ("no-std", "apps/platform/no-std/Cargo.toml"),
    ] {
        let output = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "metadata",
                "--locked",
                "--offline",
                "--no-deps",
                "--format-version",
                "1",
                "--manifest-path",
                manifest,
            ])
            .output()
            .expect("read offline Cargo metadata");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
        for package in metadata["packages"].as_array().unwrap() {
            if !metadata["workspace_members"]
                .as_array()
                .unwrap()
                .contains(&package["id"])
            {
                continue;
            }
            for target in package["targets"].as_array().unwrap() {
                if target["test"] == true {
                    targets.insert((
                        workspace.into(),
                        package["name"].as_str().unwrap().into(),
                        format!(
                            "{}:{}",
                            target["kind"][0].as_str().unwrap(),
                            target["name"].as_str().unwrap()
                        ),
                    ));
                }
            }
        }
    }
    assignments(&policy(&root), &targets).unwrap();
}

#[test]
fn missing_duplicate_stale_and_unknown_assignments_fail() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let policy = policy(&root);
    let targets = policy["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(key)
        .collect();
    assignments(&policy, &targets).unwrap();
    for defect in ["missing", "duplicate", "stale", "unknown", "suite"] {
        let mut broken = policy.clone();
        let rows = broken["targets"].as_array_mut().unwrap();
        match defect {
            "missing" => {
                rows.remove(0);
            }
            "duplicate" => rows.push(rows[0].clone()),
            "stale" => rows[0]["target"] = "test:absent".into(),
            "unknown" => rows[0]["tier"] = "absent".into(),
            "suite" => {
                broken["suites"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|suite| !suite["target"].as_array().unwrap().is_empty())
                    .unwrap()["tier"] = "unit".into();
            }
            _ => unreachable!(),
        }
        assert!(assignments(&broken, &targets).is_err(), "accepted {defect}");
    }
}
