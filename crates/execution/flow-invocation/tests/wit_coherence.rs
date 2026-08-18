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

// The node ABI is LIVE again as `wamn:node@0.1.0` (wamn-0h0g.16.2), so the pair
// of tests below no longer says "this package is deleted" — it says the ingress
// contract and the node contract are two contracts and must stay two. Ingress
// speaks to a caller about a whole run (`begin`/`wait`); the node ABI speaks to
// one pooled component instance about one graph hop. Aliasing the node ABI here
// would put node-execution vocabulary on the caller-facing wire, where a
// component-internal change would then break external callers.
//
// The ban is only worth as much as the package it names: if `wamn:node` moved or
// vanished, "does not contain it" would pass forever while meaning nothing.
// `positive_live_node_abi_is_registered_where_the_ban_names_it` is what keeps it
// honest, and the two must be read together.

/// Registered home of the live node ABI, repo-root-relative.
///
/// STAGING: the package's intended owner is `crates/execution/router`, which
/// does not exist yet; it is bound by nothing meanwhile. When the router lands
/// and takes ownership at `crates/execution/router/wit/package.wit`, this
/// constant moves with it and the guard keeps biting at the new path.
const LIVE_NODE_ABI: &str = "crates/execution/router/wit/package.wit";

/// The one signature the router invokes. Pinned here so a silent change to the
/// node ABI's shape cannot leave this file asserting a contract that is gone.
const NODE_RUN_SIGNATURE: &str =
    "run: func(ctx: node-context, input: json) -> result<emission, node-error>;";

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/execution/flow-invocation; the root is three up.
    fs::canonicalize(crate_root().join("../../.."))
        .unwrap_or_else(|error| panic!("canonicalize repo root: {error}"))
}

#[test]
fn negative_ingress_contract_does_not_alias_the_live_node_abi() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).expect("Cargo.toml reads");
    let wit = fs::read_to_string(crate_root().join("wit/package.wit")).expect("WIT reads");
    let rust = fs::read_to_string(crate_root().join("src/lib.rs")).expect("src/lib.rs reads");
    let executable_sources = format!("{manifest}\n{wit}\n{rust}").to_ascii_lowercase();

    // Both executable forms a reference can take: `wamn:node/types` names an
    // interface of the package, `wamn:node@0.1.0` names the package itself
    // (a `use`, an `include`, or the package header of a pasted-in copy).
    for forbidden in ["wamn:node/", "wamn:node@"] {
        assert!(
            !executable_sources.contains(forbidden),
            "the flow invocation contract must not alias the node ABI, but it \
             references {forbidden:?}; node execution vocabulary does not belong \
             on the caller-facing wire"
        );
    }
}

#[test]
fn positive_live_node_abi_is_registered_where_the_ban_names_it() {
    let path = repo_root().join(LIVE_NODE_ABI);
    let wit = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "the live node ABI must exist at {LIVE_NODE_ABI} for the ingress ban \
             above to mean anything, but it did not read: {error}. If the package \
             moved (the router taking ownership is the expected reason), update \
             LIVE_NODE_ABI here rather than deleting this test"
        )
    });

    assert!(
        wit.contains("package wamn:node@0.1.0;"),
        "{LIVE_NODE_ABI} is no longer the wamn:node@0.1.0 package the ban names"
    );
    assert!(
        wit.lines().any(|line| line.trim() == NODE_RUN_SIGNATURE),
        "{LIVE_NODE_ABI} must still export {NODE_RUN_SIGNATURE:?} — the single \
         operation the router invokes per graph node"
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
