//! Conformance test for the pinned runtime's lookup and socket-policy APIs.
//!
//! The lookup and raw-socket decisions are driven through public wash-runtime
//! APIs. This deliberately shows behavior rather than the presence of a WAMN
//! patch or a private runtime call site.

use std::net::SocketAddr;
use std::sync::Arc;
use url::Host;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::allowed_ip_name::{AllowedIpName, check_allowed_ip_name};
use wash_runtime::sockets::policy::{EgressMode, SocketPolicy};
use wash_runtime::sockets::resolved_names::ResolvedNames;
use wash_runtime::sockets::{AddrDecision, DenyReason, SocketAddrUse};
use wash_runtime::types::LocalResources;

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
    match policy.decide_for_store(operation, target, &ResolvedNames::default()) {
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
