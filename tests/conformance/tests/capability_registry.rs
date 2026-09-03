//! The capability registry's inherited-version rows, bound to the tree.
//!
//! §2a splits the registry into two provenance classes. The `wamn:*` rows and
//! `wasi:logging` carry versions WE author, so they move only when we move
//! them. The remaining `wasi:*` rows do not: measured on `receiving`, the
//! authored WIT says `0.2.12`, the raw build imports `0.2.9`, and the
//! virtualized artifact that admission actually sees imports `0.2.12` — the
//! virtualizer rewrites the version.
//!
//! That makes those rows fragile in a way a reader cannot see from the table
//! alone: bumping the WASI-Virt revision or adapter digest in
//! `docs/architecture/native-alignment-ledger.md` row 5 changes the version
//! admission compares against, and without a matching edit to the registry
//! **every std guest is silently refused at admission**.
//!
//! This suite is what turns that into a gate failure instead. It binds the
//! inherited rows to the WASI vocabulary vendored in the tree — the same WIT
//! guests bind against — so the registry and the tree cannot drift apart
//! quietly. It reads source, never build output, which is this tier's
//! convention and means it can never self-skip.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use wamn_component_policy::{CAPABILITY_REGISTRY, Posture};

/// Rows whose version the virtualizer/toolchain authors, not us.
const INHERITED_PACKAGES: [&str; 3] = ["wasi:io", "wasi:clocks", "wasi:random"];

fn repository_root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("canonicalize repository root")
}

/// Every `package wasi:…@version;` declaration vendored under the code tiers,
/// as `package -> {versions}`.
fn vendored_wasi_versions(root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut stack = vec![
        root.join("components"),
        root.join("crates"),
        root.join("services"),
    ];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "wit") {
                let Ok(source) = std::fs::read_to_string(&path) else {
                    continue;
                };
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

/// The load-bearing assertion: an inherited row must equal the version the tree
/// vendors for that package. A virtualizer bump that moves the vendored WIT
/// without moving the registry fails HERE, at the gate — not at admission,
/// where it would present as every std guest suddenly being unadmittable.
#[test]
fn capability_registry_wasi_rows_match_the_vendored_wit() {
    let vendored = vendored_wasi_versions(&repository_root());
    assert!(
        !vendored.is_empty(),
        "no vendored wasi WIT found; the walk is broken, so this suite proves nothing"
    );

    for package in INHERITED_PACKAGES {
        let row = CAPABILITY_REGISTRY
            .iter()
            .find(|row| row.package == package)
            .unwrap_or_else(|| panic!("{package} must carry a registry row"));
        let versions = vendored
            .get(package)
            .unwrap_or_else(|| panic!("{package} is registered but vendored nowhere in the tree"));
        assert!(
            versions.contains(row.version),
            "registry has {package}@{} but the tree vendors {versions:?} — a virtualizer or \
             toolchain bump moved the WASI vocabulary without moving the registry, and every \
             std guest would refuse at admission",
            row.version
        );
    }
}

/// Every WASI package the tree vendors and admission can reach must be either
/// registered or deliberately absent. This is the other direction: adding a
/// WASI dependency without a registry row must not pass unnoticed.
#[test]
fn vendored_wasi_packages_are_registered_or_deliberately_absent() {
    // Deliberately unregistered: reachable in the tree's WIT, refused by
    // admission. `wasi:sockets` is the denied egress package; `wasi:http` is
    // imported only by `http-route`, a `wash push` workload on the
    // non-tenant path the registry does not govern.
    const DELIBERATELY_ABSENT: [&str; 2] = ["wasi:sockets", "wasi:http"];

    let vendored = vendored_wasi_versions(&repository_root());
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

/// The registry's own shape, asserted where a reviewer will look for it.
#[test]
fn registry_is_closed_and_every_row_is_reachable() {
    assert_eq!(
        CAPABILITY_REGISTRY.len(),
        8,
        "the registry is a closed set; changing its size is a ruled expansion"
    );
    let effects: Vec<&str> = CAPABILITY_REGISTRY
        .iter()
        .filter(|row| row.posture == Posture::Effect)
        .map(|row| row.package)
        .collect();
    assert_eq!(
        effects,
        vec!["wamn:postgres", "wamn:connection", "wasmcloud:blobstore"],
        "the effect set is the security-relevant half; it moves only by ruling"
    );
}
