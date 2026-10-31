//! Generate `WASI_CEILING`, the highest `wasi:*@0.2` patch the host links
//! (docs/plan/alignment.md W3).
//!
//! The patch comes from the WIT that the resolved `wasmtime-wasi` ships, so a
//! Wasmtime bump moves the ceiling with no source edit.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn main() {
    // platform_build::inputs::declared_reads needs literal directives. The
    // lock file names the resolved wasmtime-wasi, and a registry source never
    // changes under one version.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../../Cargo.lock");

    let wit = linked_wit();
    let ceiling = highest_patch(&wit);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    fs::write(
        out.join("wasi_ceiling.rs"),
        format!("pub const WASI_CEILING: u32 = {ceiling};\n"),
    )
    .expect("write wasi_ceiling.rs");
    println!("cargo:rustc-env=WAMN_LINKED_WASI_WIT={}", wit.display());
}

/// The WASI 0.2 WIT directory of the one `wasmtime-wasi` in the lock file.
fn linked_wit() -> PathBuf {
    let cargo = std::env::var_os("CARGO").expect("CARGO");
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("parse cargo metadata");
    let manifests: Vec<&str> = metadata["packages"]
        .as_array()
        .expect("metadata packages")
        .iter()
        .filter(|package| package["name"] == "wasmtime-wasi")
        .filter_map(|package| package["manifest_path"].as_str())
        .collect();
    let [manifest] = manifests[..] else {
        panic!("the lock file must resolve exactly one wasmtime-wasi, found {manifests:?}");
    };
    Path::new(manifest)
        .parent()
        .expect("a manifest directory")
        .join("src/p2/wit/deps")
}

/// The highest `N` in a `package wasi:<name>@0.2.N;` line of the WIT files.
fn highest_patch(wit: &Path) -> u32 {
    let mut patches = Vec::new();
    for entry in fs::read_dir(wit).unwrap_or_else(|error| panic!("read {}: {error}", wit.display())) {
        let path = entry.expect("a WIT entry").path();
        if path.extension().is_none_or(|extension| extension != "wit") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read a WIT file");
        patches.extend(text.lines().filter_map(|line| {
            let version = line.trim().strip_prefix("package wasi:")?.split_once('@')?.1;
            version.strip_suffix(';')?.strip_prefix("0.2.")?.parse::<u32>().ok()
        }));
    }
    patches
        .into_iter()
        .max()
        .unwrap_or_else(|| panic!("{} declares no wasi 0.2 package", wit.display()))
}
