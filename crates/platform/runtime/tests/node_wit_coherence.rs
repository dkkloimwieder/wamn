//! Drift guard for every `wamn:node@0.1.0` package copy.
//!
//! The router owns the contract. Each guest vendors its own copy for
//! `wit-bindgen`, so every discovered copy must match the complete contract.

use std::fs;
use std::path::{Path, PathBuf};

const PACKAGE_PREFIX: &str = "package wamn:node@";
const AUTHORITY_COPY: &str = "crates/execution/router/wit/package.wit";

fn repo_root() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .expect("canonicalize repo root")
}

fn collect_copies(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some("target") {
                continue;
            }
            collect_copies(&path, root, out);
            continue;
        }
        if path.extension().and_then(|name| name.to_str()) != Some("wit") {
            continue;
        }

        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        if source
            .lines()
            .any(|line| line.trim().starts_with(PACKAGE_PREFIX))
        {
            out.push(
                path.strip_prefix(root)
                    .expect("copy is under repo root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

fn discover_copies(root: &Path) -> Vec<String> {
    let mut copies = Vec::new();
    for top in [
        "apps",
        "components",
        "crates",
        "services",
        "test-support",
        "tests",
    ] {
        collect_copies(&root.join(top), root, &mut copies);
    }
    copies.sort();
    copies
}

#[test]
fn every_copy_is_byte_identical_to_the_router_authority() {
    let root = repo_root();
    let authority = fs::read(root.join(AUTHORITY_COPY))
        .unwrap_or_else(|error| panic!("{AUTHORITY_COPY} reads: {error}"));

    for copy in discover_copies(&root) {
        let bytes =
            fs::read(root.join(&copy)).unwrap_or_else(|error| panic!("{copy} reads: {error}"));
        assert_eq!(
            bytes, authority,
            "{copy} drifted from {AUTHORITY_COPY}; re-vendor the complete package"
        );
    }
}
