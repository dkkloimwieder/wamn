//! Static coherence guard for the trusted custom-node invocation capability.

const FLOWRUNNER_PACKAGE: &str =
    include_str!("../../../../components/execution/flowrunner/wit/deps/wamn-runner/package.wit");
const RUNTIME_PACKAGE: &str = include_str!("../wit/deps/wamn-runner/package.wit");
const FLOWRUNNER_WORLD: &str =
    include_str!("../../../../components/execution/flowrunner/wit/world.wit");
const RUNTIME_WORLD: &str = include_str!("../wit/world.wit");
const RUNTIME_PLUGIN: &str = include_str!("../src/plugins/node_invocation.rs");

fn node_invocation_interface(package: &str) -> &str {
    package
        .split_once("interface node-invocation {")
        .map(|(_, interface)| interface)
        .expect("wamn:runner package contains node-invocation")
}

fn invocation_context(interface: &str) -> &str {
    let (_, context) = interface
        .split_once("record invocation-context {")
        .expect("node-invocation declares invocation-context");
    context
        .split_once("\n  }")
        .map(|(context, _)| context)
        .expect("invocation-context record closes")
}

#[test]
fn full_runner_packages_are_byte_identical() {
    assert_eq!(
        FLOWRUNNER_PACKAGE, RUNTIME_PACKAGE,
        "the flowrunner and runtime wamn:runner WIT copies must remain byte-identical"
    );
}

#[test]
fn guest_and_host_worlds_register_the_exact_capability() {
    const IMPORT: &str = "import wamn:runner/node-invocation@0.1.0;";

    assert_eq!(FLOWRUNNER_WORLD.matches(IMPORT).count(), 1);
    assert!(RUNTIME_WORLD.contains("world runner-node-invocation-plugin {"));
    assert_eq!(RUNTIME_WORLD.matches(IMPORT).count(), 1);
    assert!(RUNTIME_PLUGIN.contains("\"wamn:runner/node-invocation@0.1.0\""));
    assert!(RUNTIME_PLUGIN.contains("node_invocation::add_to_linker"));
}

#[test]
fn invocation_context_names_only_admitted_identity_not_placement() {
    let interface = node_invocation_interface(FLOWRUNNER_PACKAGE);
    assert!(invocation_context(interface).contains("implementation-digest: string,"));

    let fields = interface.lines().filter_map(|line| {
        let line = line.trim_start();
        if line.starts_with("//") {
            return None;
        }
        let (name, _) = line.split_once(':')?;
        let name = name.trim();
        name.chars()
            .all(|character| character.is_ascii_lowercase() || character == '-')
            .then_some(name)
    });
    for field in fields {
        assert!(
            !["endpoint", "url", "authority"]
                .iter()
                .any(|forbidden| field.contains(forbidden)),
            "platform-owned routing field {field:?} entered node-invocation WIT"
        );
    }
}

#[test]
fn node_error_remains_inside_opaque_response_bytes() {
    let interface = node_invocation_interface(FLOWRUNNER_PACKAGE);
    assert!(interface.contains("request: list<u8>,"));
    assert!(interface.contains("result<list<u8>, effect-error>"));
    assert!(interface.contains("`node-error` stays inside `response`; it is not an effect error."));
    for node_error_variant in [
        "retryable(",
        "rate-limited(",
        "terminal(",
        "invalid-input(",
        "cancelled,",
    ] {
        assert!(
            !interface.contains(node_error_variant),
            "node error variant {node_error_variant:?} escaped into the host-effect taxonomy"
        );
    }
}
