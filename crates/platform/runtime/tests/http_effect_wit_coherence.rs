//! Static coherence guard for the trusted HTTP effect capability.

const FLOWRUNNER_PACKAGE: &str =
    include_str!("../../../../components/execution/flowrunner/wit/deps/wamn-runner/package.wit");
const RUNTIME_PACKAGE: &str = include_str!("../wit/deps/wamn-runner/package.wit");

fn http_effect_interface(package: &str) -> &str {
    let (_, interface) = package
        .split_once("interface http-effect {")
        .expect("wamn:runner package contains http-effect");
    interface
        .split_once("\n}\n")
        .map(|(interface, _)| interface)
        .expect("http-effect interface closes")
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
