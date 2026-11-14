//! Standard WASI imports must belong to the admitted capability families.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use wamn_component_policy::CAPABILITY_REGISTRY;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn vendored_wasi_versions(root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut stack = vec![
        root.join("apps"),
        root.join("crates"),
        root.join("services"),
    ];
    while let Some(directory) = stack.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
        for entry in entries {
            let entry = entry.expect("WIT directory entry must be readable");
            let path = entry.path();
            let file_type = entry
                .file_type()
                .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()));
            if file_type.is_dir() {
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "wit") {
                let source = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
                for line in source.lines() {
                    let line = line.trim();
                    let Some(rest) = line.strip_prefix("package wasi:") else {
                        continue;
                    };
                    let Some(declaration) = rest.split(';').next() else {
                        continue;
                    };
                    if let Some((package, version)) = declaration.split_once('@') {
                        found
                            .entry(format!("wasi:{package}"))
                            .or_default()
                            .insert(version.to_owned());
                    }
                }
            }
        }
    }
    found
}

/// Every WASI package the tree vendors and admission can reach must be either
/// registered or deliberately absent. This is the other direction: adding a
/// WASI dependency without a registry row must not pass unnoticed.
#[test]
fn vendored_wasi_packages_are_registered_or_deliberately_absent() {
    // Deliberately unregistered: reachable in the tree's WIT, refused by
    // admission. `wasi:sockets` is the denied egress package; `wasi:http` is
    // imported only by `http-route`, a `wash push` workload on the
    // non-project path the registry does not govern. Publication supplies route
    // data through wasi:config only to that platform workload (P12). The platform
    // materializer exports wasi:cli/run as a service workload.
    const DELIBERATELY_ABSENT: [&str; 3] = ["wasi:sockets", "wasi:http", "wasi:config"];

    let vendored = vendored_wasi_versions(&repository_root());
    assert!(
        !vendored.is_empty(),
        "no vendored wasi WIT found; the walk is broken, so this suite tests no WIT"
    );
    let registered: BTreeSet<&str> = CAPABILITY_REGISTRY.iter().map(|row| row.package).collect();

    for package in vendored.keys() {
        assert!(
            registered.contains(package.as_str())
                || DELIBERATELY_ABSENT.contains(&package.as_str()),
            "{package} is vendored in the tree but neither registered nor listed as \
             deliberately absent; a new WASI dependency needs a ruling, not silence"
        );
    }
}
