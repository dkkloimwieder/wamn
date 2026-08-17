use std::fs;
use std::path::{Path, PathBuf};

// Both operations carry the wamn-0h0g.15.40 error channel, so a host failure
// answers the caller instead of trapping the ingress instance. Pinned once here
// and reused, so a signature edit cannot pass by updating only one assertion.
const BEGIN_SIGNATURE: &str =
    "begin: func(req: invoke-request) -> result<begin-result, invocation-error>;";
const WAIT_SIGNATURE: &str =
    "wait: func(run-id: string, timeout-ms: u32) -> result<option<invoke-result>, invocation-error>;";

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn wit_lines() -> Vec<String> {
    fs::read_to_string(crate_root().join("wit/package.wit"))
        .expect("wit/package.wit reads")
        .lines()
        .filter_map(|line| {
            let code = line.split_once("//").map_or(line, |(code, _)| code).trim();
            (!code.is_empty()).then(|| code.to_string())
        })
        .collect()
}

fn rust_lines() -> Vec<String> {
    fs::read_to_string(crate_root().join("src/lib.rs"))
        .expect("src/lib.rs reads")
        .lines()
        .filter_map(|line| {
            let code = line.trim();
            (!code.is_empty() && !code.starts_with("//")).then(|| code.to_string())
        })
        .collect()
}

fn definition(lines: &[String], header: &str) -> Vec<String> {
    let start = lines
        .iter()
        .position(|line| line == header)
        .unwrap_or_else(|| panic!("missing contract definition {header:?}"));
    let relative_end = lines[start..]
        .iter()
        .position(|line| line == "}")
        .unwrap_or_else(|| panic!("unterminated contract definition {header:?}"));
    lines[start..=start + relative_end].to_vec()
}

fn functions(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.starts_with("begin:") || line.starts_with("wait:"))
        .cloned()
        .collect()
}

#[test]
fn positive_wit_package_and_operations_are_versioned_and_complete() {
    let lines = wit_lines();
    assert_eq!(lines[0], "package wamn:flow-invocation@0.1.0;");
    assert_eq!(functions(&lines), [BEGIN_SIGNATURE, WAIT_SIGNATURE]);
}

#[test]
fn negative_wait_has_one_mandatory_bounded_timeout_and_no_unbounded_form() {
    let functions = functions(&wit_lines());
    let waits: Vec<&String> = functions
        .iter()
        .filter(|function| function.starts_with("wait:"))
        .collect();

    assert_eq!(waits, [&WAIT_SIGNATURE.to_string()]);
}

#[test]
fn negative_result_and_rejection_variants_are_not_ambiguous() {
    let lines = wit_lines();
    assert_eq!(
        definition(&lines, "variant begin-result {"),
        [
            "variant begin-result {",
            "admitted(admitted),",
            "rejected(rejection),",
            "}",
        ]
    );
    assert_eq!(
        definition(&lines, "variant invoke-result {"),
        [
            "variant invoke-result {",
            "responded(response),",
            "failed(failure),",
            "}",
        ]
    );
}

// A host failure and a pre-run rejection are different answers: the rejection
// arm is a decided business outcome carrying `status`/`code`, the error channel
// is the host declining to decide (wamn-0h0g.15.40). Collapsing either into the
// other would report a transient store outage as a caller's fault, or a real
// refusal as an outage the caller should retry.
#[test]
fn negative_host_failure_channel_never_impersonates_a_pre_run_rejection() {
    let arms = definition(&wit_lines(), "variant invocation-error {");
    for arm in &arms[1..arms.len() - 1] {
        assert!(
            !arm.contains('('),
            "an invocation-error arm must carry no detail: {arm:?}"
        );
    }
    let rejection = definition(&wit_lines(), "record rejection {");
    assert!(rejection.contains(&"status: u16,".to_string()));
    assert!(rejection.contains(&"code: string,".to_string()));
    assert!(
        definition(&wit_lines(), "variant begin-result {")
            .contains(&"rejected(rejection),".to_string()),
        "the pre-run rejection must stay a begin outcome, not a host failure"
    );
}

#[test]
fn fault_invocation_error_wit_rust_coherence_detects_drift() {
    assert_eq!(
        definition(&wit_lines(), "variant invocation-error {"),
        [
            "variant invocation-error {",
            "store-unavailable,",
            "store-corrupt,",
            "unknown-run,",
            "invalid-request,",
            "}",
        ]
    );
    assert_eq!(
        definition(&rust_lines(), "pub enum InvocationError {"),
        [
            "pub enum InvocationError {",
            "StoreUnavailable,",
            "StoreCorrupt,",
            "UnknownRun,",
            "InvalidRequest,",
            "}",
        ]
    );
}

#[test]
fn negative_deleted_invocation_vocabulary_cannot_reenter_the_contract() {
    let code = wit_lines().join("\n");
    for deleted in [
        "cancel:",
        "cancelled(",
        "outcome-expired",
        "accepted(",
        "pending(",
    ] {
        assert!(
            !code.contains(deleted),
            "deleted invocation vocabulary returned through {deleted:?}"
        );
    }
}

#[test]
fn negative_every_post_admission_value_has_run_identity() {
    let wit = wit_lines();
    let rust = rust_lines();
    assert_eq!(
        definition(&wit, "record admitted {"),
        ["record admitted {", "run-id: string,", "}"]
    );
    assert_eq!(
        definition(&rust, "pub struct Admitted {"),
        ["pub struct Admitted {", "pub run_id: String,", "}"]
    );
    assert!(definition(&wit, "record response {").contains(&"run-id: string,".to_string()));
    assert!(
        definition(&rust, "pub struct Response {").contains(&"pub run_id: String,".to_string())
    );
    assert!(definition(&wit, "record flow-error {").contains(&"run-id: string,".to_string()));
    assert!(
        definition(&rust, "pub struct FlowError {").contains(&"pub run_id: String,".to_string())
    );
    assert!(
        !definition(&wit, "record rejection {")
            .iter()
            .any(|line| line.starts_with("run-id:")),
        "a pre-run rejection must not invent a run ID"
    );
    assert!(
        !definition(&rust, "pub struct Rejection {")
            .iter()
            .any(|line| line.starts_with("pub run_id:")),
        "the Rust pre-run rejection must not invent a run ID"
    );
}

#[test]
fn negative_contract_does_not_alias_the_deleted_node_abi() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("Cargo.toml reads");
    let wit = fs::read_to_string(crate_root().join("wit/package.wit")).expect("WIT reads");
    let rust = fs::read_to_string(crate_root().join("src/lib.rs")).expect("src/lib.rs reads");
    let executable_sources = format!("{manifest}\n{wit}\n{rust}").to_ascii_lowercase();

    let forbidden = "wamn:node/";
    assert!(
        !executable_sources.contains(forbidden),
        "flow invocation must not alias deleted node ABI marker {forbidden:?}"
    );
}

#[test]
fn fault_rejection_wit_rust_coherence_detects_drift() {
    assert_eq!(
        definition(&wit_lines(), "record rejection {"),
        ["record rejection {", "status: u16,", "code: string,", "}",]
    );
    assert_eq!(
        definition(&rust_lines(), "pub struct Rejection {"),
        [
            "pub struct Rejection {",
            "pub status: u16,",
            "pub code: String,",
            "}",
        ]
    );
}

#[test]
fn fault_result_wit_rust_coherence_detects_drift() {
    let wit = wit_lines();
    let rust = rust_lines();
    assert_eq!(
        definition(&wit, "record response {"),
        [
            "record response {",
            "run-id: string,",
            "body: json,",
            "status-hint: option<u16>,",
            "}",
        ]
    );
    assert_eq!(
        definition(&rust, "pub struct Response {"),
        [
            "pub struct Response {",
            "pub run_id: String,",
            "pub body: String,",
            "pub status_hint: Option<u16>,",
            "}",
        ]
    );
    assert_eq!(
        definition(&wit, "record flow-error {"),
        [
            "record flow-error {",
            "code: string,",
            "message: option<string>,",
            "run-id: string,",
            "flow-id: string,",
            "flow-version: u32,",
            "}",
        ]
    );
    assert_eq!(
        definition(&rust, "pub struct FlowError {"),
        [
            "pub struct FlowError {",
            "pub code: String,",
            "pub message: Option<String>,",
            "pub run_id: String,",
            "pub flow_id: String,",
            "pub flow_version: u32,",
            "}",
        ]
    );
    assert_eq!(
        definition(&wit, "variant invoke-result {"),
        [
            "variant invoke-result {",
            "responded(response),",
            "failed(failure),",
            "}",
        ]
    );
    assert_eq!(
        definition(&rust, "pub enum InvokeResult {"),
        [
            "pub enum InvokeResult {",
            "Responded(Response),",
            "Failed(Failure),",
            "}",
        ]
    );
}

#[test]
fn fault_stored_status_wit_rust_coherence_detects_drift() {
    let wit = wit_lines();
    let rust = rust_lines();
    assert_eq!(
        definition(&wit, "record failure {"),
        [
            "record failure {",
            "status: u16,",
            "error: flow-error,",
            "}",
        ]
    );
    assert!(
        definition(&wit, "record response {").contains(&"status-hint: option<u16>,".to_string())
    );
    assert!(definition(&wit, "record rejection {").contains(&"status: u16,".to_string()));
    assert_eq!(
        definition(&rust, "pub struct Failure {"),
        [
            "pub struct Failure {",
            "pub status: u16,",
            "pub error: FlowError,",
            "}",
        ]
    );
    assert!(
        definition(&rust, "pub struct Response {")
            .contains(&"pub status_hint: Option<u16>,".to_string())
    );
    assert!(definition(&rust, "pub struct Rejection {").contains(&"pub status: u16,".to_string()));
}

#[test]
fn fault_timeout_wit_rust_coherence_detects_drift() {
    let wit = wit_lines();
    let rust = rust_lines();
    assert!(
        definition(&wit, "record invoke-request {")
            .contains(&"deadline-override: option<u64>,".to_string())
    );
    assert!(functions(&wit).contains(&WAIT_SIGNATURE.to_string()));
    assert!(
        definition(&rust, "pub struct InvokeRequest {")
            .contains(&"pub deadline_override: Option<u64>,".to_string())
    );
    assert_eq!(
        definition(&rust, "pub trait FlowInvocation {"),
        [
            "pub trait FlowInvocation {",
            "fn begin(&mut self, request: InvokeRequest) -> Result<BeginResult, InvocationError>;",
            "fn wait(",
            "&mut self,",
            "run_id: String,",
            "timeout_ms: u32,",
            ") -> Result<Option<InvokeResult>, InvocationError>;",
            "}",
        ]
    );
}
