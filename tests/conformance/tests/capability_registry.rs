//! The capability registry's inherited versions match the pinned WASI adapter.
//!
//! Tenant admission sees virtualized component imports. The HTTP shell's
//! vendored WIT describes a separate platform workload and cannot establish
//! those versions. This gate generates adapter bytes through the pinned
//! WASI-Virt public API and inspects their interfaces on the production engine.
//! No guest build or external artifact is required.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use wamn_component_policy::{CAPABILITY_REGISTRY, Posture, import_pkg, import_version};
use wash_runtime::wasmtime::component::Component;
use wasi_virt::WasiVirt;

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

fn assert_registry_version(package: &str, interfaces: &[String]) {
    let row = CAPABILITY_REGISTRY
        .iter()
        .find(|row| row.package == package)
        .unwrap_or_else(|| panic!("{package} must carry a registry row"));
    let versions: BTreeSet<&str> = interfaces
        .iter()
        .filter(|name| import_pkg(name) == package)
        .map(|name| {
            import_version(name)
                .unwrap_or_else(|| panic!("pinned adapter interface {name} has no version"))
        })
        .collect();
    assert_eq!(
        versions,
        BTreeSet::from([row.version]),
        "{package} registry version differs from the pinned adapter interfaces {interfaces:?}"
    );
}

#[test]
fn capability_registry_wasi_rows_match_the_pinned_adapter() {
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
    let engine = wamn_runtime::build_engine(&[]).expect("build the production engine");
    let component =
        Component::new(engine.inner(), &adapter.adapter).expect("compile the pinned WASI adapter");
    let imports: Vec<String> = component
        .component_type()
        .imports(component.engine())
        .map(|(name, _)| name.to_owned())
        .collect();
    println!(
        "production-profile-adapter={} imports={imports:?}",
        wamn_runtime::component_admission::component_digest(&adapter.adapter)
    );
    for package in ["wasi:io", "wasi:clocks"] {
        assert_registry_version(package, &imports);
    }
    assert!(
        imports.iter().all(|name| import_pkg(name) != "wasi:random"),
        "the production adapter profile unexpectedly imports random: {imports:?}"
    );

    // Random is offered by the host but absent from current tenant imports.
    // Inspect the adapter's real refusal exports to bind that row's vocabulary;
    // this test-only option does not change the production virtualization policy.
    virtualizer.random(false);
    let adapter = virtualizer
        .finish()
        .expect("generate the pinned adapter with random refusal exports");
    let component = Component::new(engine.inner(), &adapter.adapter)
        .expect("compile the pinned adapter with random refusal exports");
    let exports: Vec<String> = component
        .component_type()
        .exports(component.engine())
        .map(|(name, _)| name.to_owned())
        .filter(|name| import_pkg(name) == "wasi:random")
        .collect();
    println!(
        "random-refusal-adapter={} exports={exports:?}",
        wamn_runtime::component_admission::component_digest(&adapter.adapter)
    );
    assert_eq!(
        exports.len(),
        3,
        "the adapter must expose all random interfaces"
    );
    assert_registry_version("wasi:random", &exports);
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
    assert!(
        !vendored.is_empty(),
        "no vendored wasi WIT found; the walk is broken, so this suite proves nothing"
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
