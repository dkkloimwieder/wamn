//! Drift guard for every `wamn:node@0.1.0` package copy.
//!
//! The router owns the contract. Each guest vendors its own copy for
//! `wit-bindgen`, so inventory and complete package bytes are both guarded.

use std::fs;
use std::path::{Path, PathBuf};

const PACKAGE_DECLARATION: &str = "package wamn:node@0.1.0;";
const AUTHORITY_COPY: &str = "crates/execution/router/wit/package.wit";

const EXPECTED_COPIES: [&str; 7] = [
    "components/data/receiving-data/wit/deps/wamn-node/package.wit",
    "components/data/wms-data/wit/deps/wamn-node/package.wit",
    "components/execution/blob-put/wit/deps/wamn-node/package.wit",
    "components/no-std/http-request/wit/deps/wamn-node/package.wit",
    "components/no-std/label-render/wit/deps/wamn-node/package.wit",
    "components/no-std/transform/wit/deps/wamn-node/package.wit",
    AUTHORITY_COPY,
];

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
        if path.file_name().and_then(|name| name.to_str()) != Some("package.wit") {
            continue;
        }

        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        if source.lines().any(|line| line == PACKAGE_DECLARATION) {
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
    for top in ["components", "crates", "services"] {
        collect_copies(&root.join(top), root, &mut copies);
    }
    copies.sort();
    copies
}

#[test]
fn all_package_copies_are_registered() {
    let root = repo_root();
    let discovered = discover_copies(&root);
    let mut expected: Vec<String> = EXPECTED_COPIES
        .iter()
        .map(|path| path.to_string())
        .collect();
    expected.sort();

    assert_eq!(
        discovered, expected,
        "the wamn:node package inventory changed; register every copy in \
         crates/platform/runtime/tests/node_wit_coherence.rs"
    );
}

#[test]
fn every_copy_is_byte_identical_to_the_router_authority() {
    let root = repo_root();
    let authority = fs::read(root.join(AUTHORITY_COPY))
        .unwrap_or_else(|error| panic!("{AUTHORITY_COPY} reads: {error}"));

    for copy in EXPECTED_COPIES {
        let bytes =
            fs::read(root.join(copy)).unwrap_or_else(|error| panic!("{copy} reads: {error}"));
        assert_eq!(
            bytes, authority,
            "{copy} drifted from {AUTHORITY_COPY}; re-vendor the complete package"
        );
    }
}
