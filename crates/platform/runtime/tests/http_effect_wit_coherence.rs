//! Static coherence guard for the trusted HTTP effect capability.

const FLOWRUNNER_PACKAGE: &str =
    include_str!("../../../../components/execution/flowrunner/wit/deps/wamn-runner/package.wit");
const RUNTIME_PACKAGE: &str = include_str!("../wit/deps/wamn-runner/package.wit");
const FLOWRUNNER_LOGGING_PACKAGE: &str =
    include_str!("../../../../components/execution/flowrunner/wit/deps/wasi-logging/package.wit");
const RUNTIME_LOGGING_PACKAGE: &str = include_str!("../wit/deps/wasi-logging/package.wit");

fn interface_body<'a>(package: &'a str, header: &str) -> &'a str {
    let (_, interface) = package
        .split_once(header)
        .unwrap_or_else(|| panic!("wamn:runner package contains {header:?}"));
    interface
        .split_once("\n}\n")
        .map(|(interface, _)| interface)
        .unwrap_or_else(|| panic!("{header:?} closes"))
}

fn http_effect_interface(package: &str) -> &str {
    interface_body(package, "interface http-effect {")
}

fn invocation_context(interface: &str) -> &str {
    let (_, context) = interface
        .split_once("record invocation-context {")
        .expect("http-effect declares invocation-context");
    context
        .split_once("\n  }")
        .map(|(context, _)| context.trim())
        .expect("invocation-context record closes")
}

#[test]
fn runner_packages_keep_the_frozen_package_identity() {
    for package in [FLOWRUNNER_PACKAGE, RUNTIME_PACKAGE] {
        assert!(package.starts_with("package wamn:runner@0.1.0;\n"));
        assert!(!package.contains("run-frames"));
        assert!(!package.contains("run_frames"));
    }
}

#[test]
fn plan_supply_interface_stays_identical_in_both_runner_copies() {
    const HEADER: &str = "interface plan-supply {";
    assert_eq!(
        interface_body(FLOWRUNNER_PACKAGE, HEADER),
        interface_body(RUNTIME_PACKAGE, HEADER),
        "the two wamn:runner copies drifted in plan-supply"
    );
}

#[test]
fn causation_interface_stays_identical_in_both_runner_copies() {
    const HEADER: &str = "interface causation {";
    assert_eq!(
        interface_body(FLOWRUNNER_PACKAGE, HEADER),
        interface_body(RUNTIME_PACKAGE, HEADER),
        "the two wamn:runner copies drifted in causation"
    );
}

#[test]
fn wasi_logging_package_stays_byte_identical_in_both_copies() {
    assert_eq!(
        FLOWRUNNER_LOGGING_PACKAGE, RUNTIME_LOGGING_PACKAGE,
        "the runtime and flowrunner wasi:logging packages drifted"
    );
}

#[test]
fn invocation_context_is_the_attempt_principal_in_both_copies() {
    const CONTEXT: &str = r#"version: string,
    run-id: string,
    root-plan-hash: string,
    current-plan-hash: string,
    frame-id: u64,
    local-node-id: string,
    occurrence: u32,
    source-artifact-hash: string,
    requirement-name: string,"#;

    for package in [FLOWRUNNER_PACKAGE, RUNTIME_PACKAGE] {
        let interface = http_effect_interface(package);
        assert_eq!(invocation_context(interface), CONTEXT);
        assert!(interface.contains(
            "send: func(\n    context: invocation-context,\n    request: relative-request,\n  )"
        ));
        assert_eq!(interface.matches("requirement-name: string,").count(), 1);
    }
}
