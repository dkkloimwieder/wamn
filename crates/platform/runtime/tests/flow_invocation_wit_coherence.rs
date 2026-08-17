//! Static conformance guard for the production flow-invocation provider.

const CONTRACT: &str = include_str!("../../../execution/flow-invocation/wit/package.wit");
const HOST_COPY: &str = include_str!("../wit/deps/wamn-flow-invocation/package.wit");
const HTTP_COPY: &str = include_str!(
    "../../../../components/ingress/flow-http/wit/deps/wamn-flow-invocation/package.wit"
);
const WORLD: &str = include_str!("../wit/world.wit");
const SERVICE: &str = include_str!("../src/flow_invocation.rs");
const PLUGIN: &str = include_str!("../src/plugins/wamn_flow_invocation.rs");
const HOST: &str = include_str!("../../../../services/host/src/host.rs");
const HOST_MANIFEST: &str = include_str!("../../../../services/host/Cargo.toml");

#[test]
fn host_copy_preserves_the_frozen_interface_surface() {
    for anchor in [
        "package wamn:flow-invocation@0.1.0;",
        // Both operations carry the wamn-0h0g.15.40 error channel in every copy,
        // so a host failure answers rather than trapping the ingress instance.
        "begin: func(req: invoke-request) -> result<begin-result, invocation-error>;",
        "wait: func(run-id: string, timeout-ms: u32) -> result<option<invoke-result>, invocation-error>;",
        "variant invocation-error {",
        "store-unavailable,",
        "store-corrupt,",
        "unknown-run,",
        "invalid-request,",
        "expected-catalog-version: u64,",
        "expected-definition-hash: string,",
        "client-request-fingerprint: string,",
    ] {
        assert!(CONTRACT.contains(anchor), "contract missing {anchor}");
        assert!(HOST_COPY.contains(anchor), "host copy missing {anchor}");
        assert!(HTTP_COPY.contains(anchor), "HTTP copy missing {anchor}");
    }
}

#[test]
fn every_vendored_contract_refuses_deleted_vocabulary() {
    for (name, source) in [
        ("canonical", CONTRACT),
        ("host", HOST_COPY),
        ("HTTP", HTTP_COPY),
    ] {
        let code = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");
        for deleted in [
            "cancel:",
            "cancelled(",
            "outcome-expired",
            "accepted(",
            "pending(",
        ] {
            assert!(
                !code.contains(deleted),
                "{name} contract restored deleted vocabulary {deleted:?}"
            );
        }
    }
}

#[test]
fn runtime_world_and_plugin_register_the_exact_import() {
    assert!(WORLD.contains("world flow-invocation-plugin"));
    assert!(WORLD.contains("import wamn:flow-invocation/invocation@0.1.0;"));
    assert!(PLUGIN.contains("\"wamn:flow-invocation/invocation@0.1.0\""));
    assert!(PLUGIN.contains("invocation::add_to_linker"));
}

// wamn-0h0g.15.40: before the error channel existed the plugin converted every
// host-side failure with `Error::msg(error.to_string())`, which is a wasm trap —
// a transient store outage poisoned the flow-http instance and leaked the host's
// error text to the guest. Both halves must stay gone: the trap conversion, and
// any second place that decides an arm.
#[test]
fn host_failures_are_translated_once_and_never_trap_the_guest() {
    assert!(
        !PLUGIN.contains("Error::msg(error.to_string())"),
        "a host-side invocation failure must not be converted into a trap"
    );
    assert!(PLUGIN.contains("fn map_invocation_error(failure: InvocationFailure)"));
    // One definition plus exactly two uses: `begin` and `wait`, nowhere else.
    assert_eq!(PLUGIN.matches("map_invocation_error").count(), 3);
    for arm in [
        "invocation::InvocationError::StoreUnavailable",
        "invocation::InvocationError::StoreCorrupt",
        "invocation::InvocationError::UnknownRun",
        "invocation::InvocationError::InvalidRequest",
    ] {
        assert!(PLUGIN.contains(arm), "plugin never answers {arm}");
    }
    assert!(
        SERVICE.contains("pub fn kind(&self) -> InvocationError"),
        "the service must classify its own failures, not the plugin"
    );
}

#[test]
fn production_host_constructs_the_provider_without_inline_execution() {
    assert!(HOST.contains("WamnFlowInvocation::from_env()"));
    assert!(HOST.contains(".with_plugin(Arc::new("));
    assert!(HOST.contains("wamn:flow-invocation plugin init"));
    for deleted in [
        "InlineExecutionDriver",
        "inline_driver",
        "flowrunner",
        "credentials_file",
        "allowed_hosts",
        "inline_lease_ttl_ms",
    ] {
        assert!(
            !HOST.contains(deleted),
            "production host restored deleted inline execution surface {deleted:?}"
        );
    }
    assert!(!HOST_MANIFEST.contains("wamn-execution-host"));

    for deleted in [
        "InlineRunDriver",
        "InlineRunClaim",
        "pub project:",
        "pub schema:",
        "pub executor_id:",
        "pub lease_ttl:",
    ] {
        assert!(
            !SERVICE.contains(deleted),
            "flow-invocation service restored deleted inline execution surface {deleted:?}"
        );
    }
    for deleted in [
        "InlineRunDriver",
        "inline_executor_id",
        "PROJECT_CONFIG_KEY",
        "SCHEMA_CONFIG_KEY",
    ] {
        assert!(
            !PLUGIN.contains(deleted),
            "flow-invocation plugin restored deleted inline execution surface {deleted:?}"
        );
    }
}
