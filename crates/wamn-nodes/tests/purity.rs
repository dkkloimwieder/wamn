//! THE mechanical purity lint (docs/platform-plan.md 5.3): standard node
//! crates depend on the SDK crate ONLY — never the runner crate — so no node
//! can circumvent the `wamn:node` interface and silently break the
//! frozen-flow composition path (5.13). Enforced over `cargo metadata`: the
//! test walks `wamn-nodes`' resolved NORMAL dependency edges and fails the
//! build the moment a forbidden crate enters the closure or an undeclared
//! direct dependency appears.

use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Command;

use serde_json::Value;

/// Platform crates that must NEVER appear in a node crate's dependency
/// closure: the engine itself, and everything host/store-side of it.
const FORBIDDEN: &[&str] = &[
    "wamn-runner",
    "wamn-run-store",
    "wamn-run-queue",
    "wamn-host",
    "wamn-flow",
    "wamn-f1",
];

/// The EXACT direct (normal) dependencies wamn-nodes may have. Growing this
/// list is a conscious, test-updating act.
const ALLOWED_DIRECT: &[&str] = &["wamn-node-sdk", "wamn-api", "serde_json", "jmespath"];

fn metadata() -> Value {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1"])
        .output()
        .expect("cargo metadata runs");
    assert!(out.status.success(), "cargo metadata failed");
    serde_json::from_slice(&out.stdout).expect("cargo metadata is JSON")
}

/// The resolved package ids reachable from `root` over NORMAL dependency
/// edges only (dev/build edges excluded — the lint governs what ships).
fn normal_closure(meta: &Value, root: &str) -> HashSet<String> {
    let nodes: HashMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes")
        .iter()
        .map(|n| (n["id"].as_str().unwrap(), n))
        .collect();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([root.to_string()]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(node) = nodes.get(id.as_str()) else {
            continue;
        };
        for dep in node["deps"].as_array().expect("deps") {
            let normal = dep["dep_kinds"]
                .as_array()
                .expect("dep_kinds")
                .iter()
                .any(|k| k["kind"].is_null());
            if normal {
                queue.push_back(dep["pkg"].as_str().unwrap().to_string());
            }
        }
    }
    seen
}

fn package<'a>(meta: &'a Value, name: &str) -> &'a Value {
    meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("package {name} in metadata"))
}

#[test]
fn node_crates_depend_on_the_sdk_only_never_the_runner() {
    let meta = metadata();

    for crate_name in ["wamn-nodes", "wamn-node-sdk"] {
        let root = package(&meta, crate_name)["id"].as_str().unwrap();
        let closure = normal_closure(&meta, root);
        let names: HashSet<&str> = meta["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| closure.contains(p["id"].as_str().unwrap()))
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        for forbidden in FORBIDDEN {
            assert!(
                !names.contains(forbidden),
                "{crate_name} must never depend on {forbidden} (the 5.3/5.13 purity rule); \
                 its resolved closure contains it"
            );
        }
    }
}

#[test]
fn direct_dependencies_are_exactly_the_declared_allowlist() {
    let meta = metadata();

    let direct: HashSet<String> = package(&meta, "wamn-nodes")["dependencies"]
        .as_array()
        .expect("dependencies")
        .iter()
        .filter(|d| d["kind"].is_null())
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    let allowed: HashSet<String> = ALLOWED_DIRECT.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        direct, allowed,
        "wamn-nodes' direct dependencies changed — adding one is a conscious, \
         test-updating act (and never the runner)"
    );

    // The SDK itself stays minimal: serde_json and nothing else.
    let sdk_direct: HashSet<String> = package(&meta, "wamn-node-sdk")["dependencies"]
        .as_array()
        .expect("dependencies")
        .iter()
        .filter(|d| d["kind"].is_null())
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        sdk_direct,
        HashSet::from(["serde_json".to_string()]),
        "wamn-node-sdk must stay a minimal leaf"
    );
}
