//! Guards the component-policy module documentation's layered v2.6.1 socket posture.

const COMPONENT_POLICY_SOURCE: &str =
    include_str!("../../../crates/platform/component-policy/src/lib.rs");

fn module_documentation(source: &str) -> String {
    source
        .lines()
        .take_while(|line| line.starts_with("//!"))
        .map(|line| line.trim_start_matches("//!").trim_start())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn component_policy_module_docs_match_v2_6_1_socket_enforcement() {
    let documentation = module_documentation(COMPONENT_POLICY_SOURCE);

    for stale_claim in [
        "TcpConnect` policy is allow-all",
        "runtime half",
        "until the runtime",
        "has not landed",
        "AllowIPNameLookup",
        "allowIpNameLookup",
    ] {
        assert!(
            !documentation.contains(stale_claim),
            "stale socket-policy claim returned: {stale_claim:?}"
        );
    }

    for required in [
        "Structurally refuses publication",
        "every P2 or P3 `wasi:sockets` interface",
        "before\npublication",
        "independent of the pinned wasmCloud v2.6.1 runtime",
        "`TcpConnect`, `UdpConnect`, and `UdpOutgoingDatagram` deny by default",
        "explicit raw-socket opt-in",
        "`UdpBind` remains\nservice-loopback-only",
        "`AllowedIPNameLookups` and the `allowed_hosts` allowlist are independent",
        "`allowed_hosts` governs `wasi:http` only",
        "`docs/security-db-path.md` for the layered boundary",
        "`docs/wash-runtime-fork.md` for the authoritative branch, revision, and\ncarried-policy details",
    ] {
        assert!(
            documentation.contains(required),
            "component-policy module docs lost required v2.6.1 posture {required:?}"
        );
    }
}
