//! Guards the documented DB-path socket-policy posture against stale claims.

use std::fs;
use std::path::{Path, PathBuf};

const SECURITY_DB_PATH_DOC: &str = "docs/security-db-path.md";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn section<'a>(document: &'a str, start: &str, end: &str) -> &'a str {
    let (_, remainder) = document
        .split_once(start)
        .unwrap_or_else(|| panic!("{SECURITY_DB_PATH_DOC} is missing section {start:?}"));
    remainder
        .split_once(end)
        .map_or(remainder, |(contents, _)| contents)
}

#[test]
fn security_db_path_v2_6_1_rejects_allow_all_and_pins_p2_p3_tcp_udp() {
    let path = repository_root().join(SECURITY_DB_PATH_DOC);
    let document = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));

    assert!(
        document.contains("wasmCloud v2.6.1"),
        "DB-path posture must name the v2.6.1 runtime baseline"
    );
    assert!(
        document.contains("`docs/wash-runtime-fork.md` is authoritative"),
        "branch, revision, and carried-policy details must stay in the fork ledger"
    );

    for stale_claim in [
        "allow-all",
        "allow all",
        "AllowIPNameLookup",
        "allowIpNameLookup",
        "Outbound TCP connect is permitted to any address",
        "the host provides no defense-in-depth",
    ] {
        assert!(
            !document.contains(stale_claim),
            "stale socket-policy claim returned: {stale_claim:?}"
        );
    }

    let composition = section(
        &document,
        "### 1. WIT-world composition",
        "### 2. Publish-time socket refusal",
    );
    assert!(
        document.contains("### 1. WIT-world composition — reachability, not permission")
            && composition.contains("does not add that")
            && composition.contains("import to a component")
            && composition.contains("runtime each enforce a separate policy"),
        "WIT composition must be distinguished from publish-time and runtime policy"
    );

    let publication = section(
        &document,
        "### 2. Publish-time socket refusal",
        "### 3. Runtime raw TCP/UDP policy",
    );
    for required in ["P2", "P3", "refused", "standard"] {
        assert!(
            publication.contains(required),
            "publish-time policy section lost required marker {required:?}"
        );
    }

    let runtime = section(
        &document,
        "### 3. Runtime raw TCP/UDP policy",
        "### 4. `allowed_hosts`",
    );
    for required in [
        "P2 and P3 `TcpConnect`",
        "P2 and P3 `UdpConnect`",
        "P2 and P3 `UdpOutgoingDatagram`",
        "P2 and P3 `UdpBind`",
        "deny when the raw-socket",
        "wamn.allow-raw-sockets",
        "`AllowedIPNameLookups`",
        "`allowedIpNameLookups`",
        "service-loopback-only",
    ] {
        assert!(
            runtime.contains(required),
            "runtime policy section lost required P2/P3 TCP/UDP marker {required:?}"
        );
    }

    let kubernetes = section(
        &document,
        "## Kubernetes defense in depth",
        "## Standing proof set",
    );
    for required in [
        "approved Postgres endpoints and ports",
        "not a substitute",
        "co-located in the host process",
        "production CNI",
    ] {
        assert!(
            kubernetes.contains(required),
            "Kubernetes defense-in-depth boundary lost marker {required:?}"
        );
    }
}
