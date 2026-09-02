//! Drift guard for every surviving `wamn:postgres@0.1.0` package copy.
//!
//! `wit-bindgen` resolves each guest and host from its own WIT tree, so every
//! copy must be registered here and remain byte-identical to the host copy.

use std::fs;
use std::path::{Path, PathBuf};

const AUTHORITY_COPY: &str = "crates/platform/runtime/wit/deps/wamn-postgres/package.wit";

const EXPECTED_COPIES: [&str; 5] = [
    "components/data/postgres-statements/wit/deps/wamn-postgres/package.wit",
    "components/data/postgres-sqlx/wit/deps/wamn-postgres/package.wit",
    "components/data/receiving-data/wit/deps/wamn-postgres/package.wit",
    "components/execution/materializer/wit/deps/wamn-postgres/package.wit",
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
        let parent = path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str());
        let grandparent = path
            .parent()
            .and_then(Path::parent)
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str());
        if parent == Some("wamn-postgres") && grandparent == Some("deps") {
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
fn all_vendored_copies_are_registered() {
    let root = repo_root();
    let discovered = discover_copies(&root);
    let mut expected: Vec<String> = EXPECTED_COPIES
        .iter()
        .map(|path| path.to_string())
        .collect();
    expected.sort();

    assert_eq!(
        discovered, expected,
        "the wamn:postgres package inventory changed; register every surviving copy in \
         crates/platform/runtime/tests/postgres_wit_coherence.rs"
    );
}

#[test]
fn every_copy_is_byte_identical_to_the_authority() {
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
