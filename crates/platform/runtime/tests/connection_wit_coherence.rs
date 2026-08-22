//! Drift guard for the frozen `wamn:connection/http@0.1.0` capability.

use std::fs;
use std::path::{Path, PathBuf};

mod host_bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "connection-http-plugin",
        path: "wit",
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

const EXPECTED_COPIES: [&str; 3] = [
    "components/fixtures/connection-http-standard/wit/deps/wamn-connection/package.wit",
    "components/library/http-request/wit/deps/wamn-connection/package.wit",
    "crates/platform/runtime/wit/deps/wamn-connection/package.wit",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root canonicalizes")
}

fn collect_copies(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("{} reads: {error}", dir.display()));
    for entry in entries {
        let entry = entry.expect("directory entry reads");
        let path = entry.path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                continue;
            }
            collect_copies(&path, root, found);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("package.wit") {
            let source = fs::read_to_string(&path).expect("candidate WIT reads");
            if source.contains("package wamn:connection@") {
                found.push(
                    path.strip_prefix(root)
                        .expect("vendored WIT is within repository")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
}

#[test]
fn every_vendored_http_connection_contract_is_registered_and_byte_identical() {
    let root = repo_root();
    let authority = fs::read_to_string(root.join("docs/archive/contracts/wamn-connection.wit"))
        .expect("authoritative connection WIT reads");
    let mut found = Vec::new();
    for top in ["components", "crates", "services"] {
        collect_copies(&root.join(top), &root, &mut found);
    }
    found.sort();

    assert_eq!(found, EXPECTED_COPIES);
    for path in EXPECTED_COPIES {
        let copy = fs::read_to_string(root.join(path))
            .unwrap_or_else(|error| panic!("{path} reads: {error}"));
        assert_eq!(
            copy, authority,
            "{path} drifted from docs/archive/contracts/wamn-connection.wit"
        );
    }
}

#[test]
fn frozen_http_surface_is_relative_typed_and_extension_free() {
    let authority =
        fs::read_to_string(repo_root().join("docs/archive/contracts/wamn-connection.wit"))
            .expect("authoritative connection WIT reads");
    for required in [
        "package wamn:connection@0.1.0;",
        "requirement: string,",
        "path-and-query: string,",
        "idempotency-key: option<string>,",
        "send: func(request: request) -> result<response, connection-error>;",
        "authority-denied,",
        "attestation-invalid,",
        "credential-unavailable,",
    ] {
        assert!(
            authority.contains(required),
            "missing frozen WIT line {required:?}"
        );
    }
    for forbidden in [
        "absolute-uri",
        "authority:",
        "scheme:",
        "host:",
        "port:",
        "proxy",
        "socket",
        "json",
        "extensions",
    ] {
        assert!(
            !authority.to_ascii_lowercase().contains(forbidden),
            "forbidden connection surface {forbidden:?} entered the frozen WIT"
        );
    }
}

#[test]
fn host_adapter_and_trusted_runner_worlds_pin_the_authority_split() {
    let root = repo_root();
    let host = fs::read_to_string(root.join("crates/platform/runtime/wit/world.wit"))
        .expect("host world reads");
    let portable_import = "import wamn:connection/http@0.1.0;";
    let trusted_import = "import wamn:runner/http-effect@0.1.0;";

    assert_eq!(host.matches(portable_import).count(), 1);
    assert_eq!(host.matches(trusted_import).count(), 1);
}
