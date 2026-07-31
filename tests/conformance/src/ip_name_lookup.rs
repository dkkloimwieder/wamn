//! Conformance proof for the pinned runtime's `allowedIpNameLookups` primitive.
//!
//! Matching behavior is exercised through wash-runtime's public policy API.
//! Socket dominance is a source-backed guard because the runtime's socket
//! address callback and P2/P3 host implementations are intentionally private.

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use url::Host;
use wash_runtime::host::allowed_ip_name::{AllowedIpName, check_allowed_ip_name};
use wash_runtime::types::LocalResources;

const EXPECTED_VERSION: &str = "2.6.1";
const EXPECTED_REVISION: &str = "09b1132f2bab36e6e71f4637bd0e4755e359dd43";

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

struct RuntimePackage {
    root: PathBuf,
    version: String,
    source: String,
}

struct RuntimeSources {
    linked_call: String,
    lookup_p2: String,
    lookup_p3: String,
    tcp_p2: String,
    tcp_p3: String,
    udp_p2: String,
    udp_p3: String,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn runtime_package() -> RuntimePackage {
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
            .expect("wash-runtime must resolve from the fork"),
    }
}

fn read_source(root: &Path, relative: &str) -> String {
    let path = root.join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn runtime_sources(root: &Path) -> RuntimeSources {
    RuntimeSources {
        linked_call: read_source(root, "src/engine/linked_call.rs"),
        lookup_p2: read_source(root, "src/sockets/host_ip_name_lookup.rs"),
        lookup_p3: read_source(root, "src/sockets/host_ip_name_lookup_p3.rs"),
        tcp_p2: read_source(root, "src/sockets/host_tcp.rs"),
        tcp_p3: read_source(root, "src/sockets/host_tcp_p3.rs"),
        udp_p2: read_source(root, "src/sockets/host_udp.rs"),
        udp_p3: read_source(root, "src/sockets/host_udp_p3.rs"),
    }
}

fn compact(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn require(source: &str, marker: &str, seam: &str) -> Result<(), String> {
    if source.contains(marker) {
        Ok(())
    } else {
        Err(format!("{seam} no longer contains `{marker}`"))
    }
}

fn validate_socket_dominance(sources: &RuntimeSources) -> Result<(), String> {
    let linked_call = compact(&sources.linked_call);
    require(
        &linked_call,
        "allowed_ip_name_lookups:Arc::clone(&template.local_resources.allowed_ip_name_lookups)",
        "lookup policy wiring",
    )?;
    require(
        &linked_call,
        "letpermitted=socket_addr_permitted(reason,addr.ip().is_loopback(),addr.ip().is_unspecified(),is_service,allow_raw_sockets,);",
        "fork socket decision",
    )?;
    for operation in [
        "SocketAddrUse::TcpConnect=>allow_raw_sockets",
        "SocketAddrUse::UdpConnect=>allow_raw_sockets",
        "SocketAddrUse::UdpOutgoingDatagram=>allow_raw_sockets",
    ] {
        require(&linked_call, operation, "fork raw-egress policy")?;
    }

    let lookup_p2 = compact(&sources.lookup_p2);
    require(
        &lookup_p2,
        "check_allowed_ip_name(&network.allowed_ip_name_lookups,&host,)",
        "P2 lookup policy",
    )?;
    require(
        &lookup_p2,
        "ErrorCode::PermanentResolverFailure",
        "P2 lookup denial",
    )?;

    let lookup_p3 = compact(&sources.lookup_p3);
    require(
        &lookup_p3,
        "check_allowed_ip_name(&view.get().ctx.allowed_ip_name_lookups,&host,)",
        "P3 lookup policy",
    )?;
    require(
        &lookup_p3,
        "ErrorCode::PermanentResolverFailure",
        "P3 lookup denial",
    )?;

    let tcp_p2 = compact(&sources.tcp_p2);
    require(
        &tcp_p2,
        ".check_socket_addr(remote_address,SocketAddrUse::TcpConnect).await",
        "P2 TCP connect",
    )?;

    let tcp_p3 = compact(&sources.tcp_p3);
    require(
        &tcp_p3,
        "if!check(remote_address,SocketAddrUse::TcpConnect).await{returnErr(types::ErrorCode::AccessDenied.into());}",
        "P3 TCP connect",
    )?;

    let udp_p2 = compact(&sources.udp_p2);
    require(
        &udp_p2,
        ".check(connect_addr,SocketAddrUse::UdpConnect).await",
        "P2 UDP connect",
    )?;
    require(
        &udp_p2,
        ".check(addr,SocketAddrUse::UdpOutgoingDatagram).await",
        "P2 UDP outgoing datagram",
    )?;

    let udp_p3 = compact(&sources.udp_p3);
    require(
        &udp_p3,
        "if!(self.ctx.socket_addr_check)(remote_address,SocketAddrUse::UdpConnect).await{returnErr(ErrorCode::AccessDenied.into());}",
        "P3 UDP connect",
    )?;
    require(
        &udp_p3,
        "if!check(remote_address,SocketAddrUse::UdpOutgoingDatagram).await{returnErr(ErrorCode::AccessDenied.into());}",
        "P3 UDP outgoing datagram",
    )?;
    Ok(())
}

fn host(name: &str) -> Host<String> {
    Host::parse(name).expect("test host must parse")
}

fn policy(entry: &str) -> AllowedIpName {
    entry.parse().expect("test policy entry must parse")
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
fn pinned_runtime_keeps_lookup_separate_from_p2_and_p3_tcp_udp_authority() {
    let package = runtime_package();
    assert_eq!(package.version, EXPECTED_VERSION);
    assert!(
        package.source.contains(&format!("rev={EXPECTED_REVISION}"))
            && package.source.ends_with(&format!("#{EXPECTED_REVISION}")),
        "wash-runtime must resolve to fork revision {EXPECTED_REVISION}, got {}",
        package.source
    );
    let sources = runtime_sources(&package.root);
    validate_socket_dominance(&sources).unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn approved_lookup_must_not_authorize_fork_denied_tcp_or_udp() {
    let package = runtime_package();
    let mut sources = runtime_sources(&package.root);
    let original = "let permitted = socket_addr_permitted(";
    let fault = "let permitted = !template.local_resources.allowed_ip_name_lookups.is_empty() || socket_addr_permitted(";
    assert!(
        sources.linked_call.contains(original),
        "fault injection target must remain present"
    );
    sources.linked_call = sources.linked_call.replacen(original, fault, 1);

    let error = validate_socket_dominance(&sources)
        .expect_err("approved lookup must not bypass the fork's raw TCP/UDP decision");
    assert!(
        error.contains("fork socket decision"),
        "fault must fail at the independent socket decision, got: {error}"
    );
}
