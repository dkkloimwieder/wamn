//! Conformance proof for the pinned runtime's lookup and socket-policy APIs.
//!
//! The lookup and raw-socket decisions are driven through public wash-runtime
//! APIs. This deliberately proves behavior rather than the presence of a WAMN
//! patch or a private runtime call site.

use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use url::Host;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::allowed_ip_name::{AllowedIpName, check_allowed_ip_name};
use wash_runtime::sockets::policy::{EgressMode, SocketPolicy};
use wash_runtime::sockets::{AddrDecision, DenyReason, SocketAddrUse};
use wash_runtime::types::LocalResources;

pub(super) const EXPECTED_VERSION: &str = "2.8.0";
pub(super) const EXPECTED_REVISION: &str = "735b57982545358409a7d965a22549b08487ca09";

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: String,
}

pub(super) struct RuntimePackage {
    pub(super) root: PathBuf,
    pub(super) version: String,
    pub(super) source: String,
}

pub(super) fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

pub(super) fn runtime_package() -> RuntimePackage {
    let root = repository_root();
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .output()
        .expect("run cargo metadata for wash-runtime source");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must be valid JSON");
    let package = metadata
        .packages
        .into_iter()
        .find(|package| package.name == "wash-runtime")
        .expect("resolved graph must contain wash-runtime");
    let manifest = PathBuf::from(package.manifest_path);
    RuntimePackage {
        root: manifest
            .parent()
            .expect("wash-runtime manifest must have a parent")
            .to_path_buf(),
        version: package.version,
        source: package
            .source
            .expect("wash-runtime must resolve from the pinned repository"),
    }
}

pub(super) fn compact(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn host(name: &str) -> Host<String> {
    Host::parse(name).expect("test host must parse")
}

fn policy(entry: &str) -> AllowedIpName {
    entry.parse().expect("test policy entry must parse")
}

fn address(value: &str) -> SocketAddr {
    value.parse().expect("test socket address must parse")
}

fn enforcing_policy(allowed_hosts: Arc<[AllowedHost]>) -> SocketPolicy {
    SocketPolicy {
        allowed_hosts,
        egress_mode: EgressMode::Enforce,
        ..SocketPolicy::default()
    }
}

fn denial(
    policy: &SocketPolicy,
    operation: SocketAddrUse,
    target: SocketAddr,
) -> Option<DenyReason> {
    match policy.decide(operation, target) {
        AddrDecision::Deny(reason) => Some(reason),
        AddrDecision::Allow(_) => None,
    }
}

#[test]
fn exact_lookup_matches_only_the_approved_name() {
    let entry = policy("Example.COM");
    assert!(check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("example.com")
    ));
    assert!(check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("EXAMPLE.COM")
    ));
    assert!(!check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("api.example.com")
    ));
    assert!(!check_allowed_ip_name(&[entry], &host("example.org")));
}

#[test]
fn wildcard_lookup_requires_a_real_subdomain_and_never_matches_an_ip() {
    let entry = policy("*.example.com");
    assert!(check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("api.example.com")
    ));
    assert!(check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("deep.api.example.com")
    ));
    assert!(!check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("example.com")
    ));
    assert!(!check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("notexample.com")
    ));
    assert!(!check_allowed_ip_name(&[entry], &host("127.0.0.1")));
}

#[test]
fn literal_ip_lookup_matches_only_the_approved_address() {
    let entry = policy("127.0.0.1");
    assert!(check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("127.0.0.1")
    ));
    assert!(!check_allowed_ip_name(
        std::slice::from_ref(&entry),
        &host("127.0.0.2")
    ));
    assert!(!check_allowed_ip_name(&[entry], &host("localhost")));
}

#[test]
fn local_resources_default_is_an_empty_deny_all_lookup_policy() {
    let resources = LocalResources::default();
    assert!(
        resources.allowed_ip_name_lookups.is_empty(),
        "LocalResources must default allowedIpNameLookups to []"
    );
    assert!(!check_allowed_ip_name(
        &resources.allowed_ip_name_lookups,
        &host("localhost")
    ));
    assert!(!check_allowed_ip_name(
        &resources.allowed_ip_name_lookups,
        &host("127.0.0.1")
    ));
}

#[test]
fn pinned_runtime_is_vanilla_v2_8_0() {
    let package = runtime_package();
    assert_eq!(package.version, EXPECTED_VERSION);
    assert!(
        package.source.contains(&format!("rev={EXPECTED_REVISION}"))
            && package.source.ends_with(&format!("#{EXPECTED_REVISION}")),
        "wash-runtime must resolve to vanilla v2.8.0 revision {EXPECTED_REVISION}, got {}",
        package.source
    );
}

#[test]
fn enforced_empty_allowlist_denies_raw_tcp_and_udp() {
    let target = address("93.184.216.34:443");
    let policy = enforcing_policy(Arc::from([]));

    for operation in [
        SocketAddrUse::TcpConnect,
        SocketAddrUse::UdpConnect,
        SocketAddrUse::UdpOutgoingDatagram,
    ] {
        assert_eq!(
            denial(&policy, operation, target),
            Some(DenyReason::NotPermitted),
            "an undeclared destination must be refused for {operation:?}"
        );
    }
}

#[test]
fn vanilla_address_defaults_deny_special_ranges_and_allow_private_ranges() {
    let policy = enforcing_policy(Arc::from([AllowedHost::Any]));
    for blocked in [
        "169.254.169.254:80",
        "169.254.1.1:80",
        "192.0.2.10:80",
        "[fe80::1]:80",
    ] {
        assert_eq!(
            denial(&policy, SocketAddrUse::TcpConnect, address(blocked)),
            Some(DenyReason::BlockedRange),
            "the vanilla range policy must block {blocked}"
        );
    }

    for permitted in ["10.0.0.5:443", "192.168.1.5:443", "93.184.216.34:443"] {
        assert_eq!(
            denial(&policy, SocketAddrUse::TcpConnect, address(permitted)),
            None,
            "the vanilla default range policy must permit {permitted} after allowedHosts"
        );
    }
}

#[test]
fn approved_lookup_does_not_grant_raw_socket_authority() {
    let lookup = policy("example.com");
    assert!(check_allowed_ip_name(&[lookup], &host("example.com")));

    let sockets = enforcing_policy(Arc::from([]));
    let target = address("93.184.216.34:443");
    for operation in [SocketAddrUse::TcpConnect, SocketAddrUse::UdpConnect] {
        assert_eq!(
            denial(&sockets, operation, target),
            Some(DenyReason::NotPermitted),
            "name lookup approval must not grant {operation:?}"
        );
    }
}
