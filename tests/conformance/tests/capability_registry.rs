//! The capability registry's WASI families admit the pinned WASI adapter.
//!
//! Project admission sees virtualized component imports. The HTTP shell's
//! vendored WIT describes a separate platform workload and cannot establish
//! those versions. This acceptance test generates adapter bytes through the pinned
//! WASI-Virt public API and inspects their interfaces on the production engine.
//! The WASI-Virt revision and adapter digest stay exact
//! (docs/plan/alignment.md W2). No guest build or external artifact is
//! required.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use wamn_component_policy::{
    CAPABILITY_REGISTRY, WASI_FAMILY, capability_row, import_pkg, import_version,
};
use wash_runtime::wasmtime::component::Component;
use wasi_virt::WasiVirt;

/// The pins of docs/architecture/native-alignment.md.
const WASI_VIRT_SOURCE_REV: &str = "448f6df8f688cee5d6995e96b1ffc31f9bf00742";
const WASI_VIRT_ADAPTER_DIGEST: &str =
    "sha256:28eff8a2255812b440fbad2784a5a87660321e667331c17fb9a95f29caa85632";

fn repository_root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("canonicalize repository root")
}

/// Every `package wasi:…@version;` declaration vendored under the code tiers,
/// as `package -> {versions}`.
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

fn assert_registry_family(package: &str, interfaces: &[String]) {
    let row = CAPABILITY_REGISTRY
        .iter()
        .find(|row| row.package == package)
        .unwrap_or_else(|| panic!("{package} must carry a registry row"));
    assert_eq!(
        row.version, WASI_FAMILY,
        "{package} row must name the family"
    );
    let mut found = false;
    for name in interfaces.iter().filter(|name| import_pkg(name) == package) {
        found = true;
        import_version(name)
            .unwrap_or_else(|| panic!("pinned adapter interface {name} has no version"));
        assert!(
            capability_row(name).is_some_and(|admitted| admitted.package == package),
            "the {package} row does not admit the pinned adapter interface {name}"
        );
    }
    assert!(
        found,
        "the pinned adapter imports no {package}: {interfaces:?}"
    );
}

/// The resolved WASI-Virt source revision and the digest of the adapter it
/// embeds.
fn resolved_wasi_virt() -> (String, String) {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .current_dir(repository_root())
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata");
    let package = metadata["packages"]
        .as_array()
        .expect("metadata packages")
        .iter()
        .find(|package| package["name"] == "wasi-virt")
        .expect("the workspace resolves wasi-virt");
    let source = package["source"].as_str().expect("a wasi-virt source");
    let revision = source.rsplit_once('#').map_or_else(
        || panic!("wasi-virt source {source} names no revision"),
        |(_, revision)| revision.to_owned(),
    );
    let manifest = Path::new(package["manifest_path"].as_str().expect("a manifest path"));
    let adapter = manifest
        .parent()
        .expect("a manifest directory")
        .join("lib/virtual_adapter-wasip2.wasm");
    let bytes = std::fs::read(&adapter)
        .unwrap_or_else(|error| panic!("read {}: {error}", adapter.display()));
    (revision, wamn_digest::bytes_sha256(&bytes))
}

#[test]
fn capability_registry_wasi_rows_match_the_pinned_adapter() {
    let (revision, adapter_digest) = resolved_wasi_virt();
    assert_eq!(revision, WASI_VIRT_SOURCE_REV, "WASI-Virt revision moved");
    assert_eq!(
        adapter_digest, WASI_VIRT_ADAPTER_DIGEST,
        "WASI-Virt adapter digest moved"
    );

    let mut virtualizer = WasiVirt::new();
    // The production tool's fixed profile, before component-specific filtering.
    virtualizer.clocks(true);
    virtualizer.env().deny_all();
    virtualizer.exit(false);
    virtualizer.stdio().deny();
    virtualizer.wasm_opt(false);

    let adapter = virtualizer
        .finish()
        .expect("generate the pinned WASI adapter");
    let engine = wamn_engine::build_engine(&[]).expect("build the production engine");
    let component =
        Component::new(engine.inner(), &adapter.adapter).expect("compile the pinned WASI adapter");
    let imports: Vec<String> = component
        .component_type()
        .imports(component.engine())
        .map(|(name, _)| name.to_owned())
        .collect();
    println!(
        "production-profile-adapter={} imports={imports:?}",
        wamn_digest::bytes_sha256(&adapter.adapter)
    );
    for package in ["wasi:io", "wasi:clocks"] {
        assert_registry_family(package, &imports);
    }
    assert!(
        imports.iter().all(|name| import_pkg(name) != "wasi:random"),
        "the production adapter profile unexpectedly imports random: {imports:?}"
    );

    // The production profile leaves random unset, so the adapter neither
    // imports nor exports it: a guest's random import passes through at the
    // version Rust std links. The registry row's family admits it, and the
    // `jsonata` node's admission test measures it on a real guest.
    let exports: Vec<String> = component
        .component_type()
        .exports(component.engine())
        .map(|(name, _)| name.to_owned())
        .filter(|name| import_pkg(name) == "wasi:random")
        .collect();
    assert!(
        exports.is_empty(),
        "the production adapter must pass random through, not serve it: {exports:?}"
    );
}

/// Every WASI package the tree vendors and admission can reach must be either
/// registered or deliberately absent. This is the other direction: adding a
/// WASI dependency without a registry row must not pass unnoticed.
#[test]
fn vendored_wasi_packages_are_registered_or_deliberately_absent() {
    // Deliberately unregistered: reachable in the tree's WIT, refused by
    // admission. `wasi:sockets` is the denied egress package; `wasi:http` is
    // imported only by `http-route`, a `wash push` workload on the
    // non-project path the registry does not govern. The platform materializer
    // exports wasi:cli/run as a service workload, not a project import.
    const DELIBERATELY_ABSENT: [&str; 3] = ["wasi:sockets", "wasi:http", "wasi:cli"];

    let vendored = vendored_wasi_versions(&repository_root());
    assert!(
        !vendored.is_empty(),
        "no vendored wasi WIT found; the walk is broken, so this suite checks no WIT"
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
