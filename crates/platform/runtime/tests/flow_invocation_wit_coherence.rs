//! Static conformance guard for the production flow-invocation provider.

const CONTRACT: &str = include_str!("../../../execution/flow-invocation/wit/package.wit");
const HOST_COPY: &str = include_str!("../wit/deps/wamn-flow-invocation/package.wit");
const WORLD: &str = include_str!("../wit/world.wit");
const PLUGIN: &str = include_str!("../src/plugins/wamn_flow_invocation.rs");
const HOST: &str = include_str!("../../../../services/host/src/host.rs");

#[test]
fn host_copy_preserves_the_frozen_interface_surface() {
    for anchor in [
        "package wamn:flow-invocation@0.1.0;",
        "begin: func(req: invoke-request) -> begin-result;",
        "wait: func(run-id: string, timeout-ms: u32) -> option<invoke-result>;",
        "expected-catalog-version: u64,",
        "expected-definition-hash: string,",
        "client-request-fingerprint: string,",
    ] {
        assert!(CONTRACT.contains(anchor), "contract missing {anchor}");
        assert!(HOST_COPY.contains(anchor), "host copy missing {anchor}");
    }
}

#[test]
fn runtime_world_and_plugin_register_the_exact_import() {
    assert!(WORLD.contains("world flow-invocation-plugin"));
    assert!(WORLD.contains("import wamn:flow-invocation/invocation@0.1.0;"));
    assert!(PLUGIN.contains("\"wamn:flow-invocation/invocation@0.1.0\""));
    assert!(PLUGIN.contains("invocation::add_to_linker"));
}

#[test]
fn production_host_constructs_the_provider() {
    assert!(HOST.contains("WamnFlowInvocation::from_env(inline_driver)"));
    assert!(HOST.contains(".with_plugin(Arc::new("));
    assert!(HOST.contains("wamn:flow-invocation plugin init"));
}
