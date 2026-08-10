//! Drift-guards tying the FROZEN `wamn:node` 0.1 contract file
//! (`docs/archive/contracts/wamn-node.wit`) to (a) every vendored copy of it and (b) the exact
//! WIT lines this SDK mirrors natively. The wamn-schema-model/wamn-flow
//! committed-contract pattern: editing the contract without updating the
//! mirrors (or vice versa) fails a named test instead of shipping skew.

use std::fs;
use std::path::{Path, PathBuf};

const VENDORED_WIT_PATHS: [&str; 10] = [
    "components/execution/flowrunner/wit/deps/wamn-node/package.wit",
    "components/fixtures/cred-probe/wit/deps/wamn-node/package.wit",
    "components/fixtures/flow-driver/wit/deps/wamn-node/package.wit",
    "components/samples/node-cred/wit/deps/wamn-node/package.wit",
    "components/samples/node-rs/wit/deps/wamn-node/package.wit",
    "components/samples/node-ts/wit/deps/wamn-node/package.wit",
    "crates/node/guest/wit-caps/deps/wamn-node/package.wit",
    "crates/node/guest/wit-payload/deps/wamn-node/package.wit",
    "crates/node/guest/wit/deps/wamn-node/package.wit",
    "crates/platform/runtime/wit/deps/wamn-node/package.wit",
];

const FULL_TYPE_WIT_PATHS: [&str; 6] = [
    "components/fixtures/flow-driver/wit/deps/wamn-node/package.wit",
    "components/samples/node-cred/wit/deps/wamn-node/package.wit",
    "components/samples/node-rs/wit/deps/wamn-node/package.wit",
    "components/samples/node-ts/wit/deps/wamn-node/package.wit",
    "crates/node/guest/wit/deps/wamn-node/package.wit",
    "crates/platform/runtime/wit/deps/wamn-node/package.wit",
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn docs_wit() -> String {
    fs::read_to_string(root().join("../../../docs/archive/contracts/wamn-node.wit"))
        .expect("docs/archive/contracts/wamn-node.wit reads")
}

fn workspace_root() -> PathBuf {
    root()
        .join("../../..")
        .canonicalize()
        .expect("workspace root")
}

fn workspace_wit(path: &str) -> String {
    fs::read_to_string(workspace_root().join(path)).unwrap_or_else(|e| panic!("{path} reads: {e}"))
}

/// Comment- and blank-stripped, whitespace-trimmed code lines.
fn code_lines(wit: &str) -> Vec<&str> {
    wit.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect()
}

fn block_lines<'a>(wit: &'a str, declaration: &str) -> Vec<&'a str> {
    code_lines(wit)
        .into_iter()
        .skip_while(|line| *line != declaration)
        .take_while(|line| *line != "}")
        .collect()
}

fn assert_code_subsequence(name: &str, candidate: &str, authority: &str) {
    let authority_lines = code_lines(authority);
    let mut authority_iter = authority_lines.iter();
    for line in code_lines(candidate) {
        assert!(
            authority_iter.any(|authority_line| *authority_line == line),
            "{name}: line {line:?} is absent or out of order in the authority"
        );
    }
}

fn discover_wamn_node_packages(dir: &Path, found: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("{} reads: {e}", dir.display())) {
        let entry = entry.expect("directory entry reads");
        let path = entry.path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "target")
            ) {
                continue;
            }
            discover_wamn_node_packages(&path, found);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("package.wit") {
            let wit = fs::read_to_string(&path).expect("candidate package.wit reads");
            if wit.contains("package wamn:node@") {
                found.push(
                    path.strip_prefix(workspace_root())
                        .expect("vendored WIT is within workspace")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
}

#[test]
fn vendored_wit_inventory_is_complete() {
    let mut found = Vec::new();
    discover_wamn_node_packages(&workspace_root(), &mut found);
    found.sort();
    assert_eq!(found, VENDORED_WIT_PATHS);
}

/// Every vendored copy's code lines must appear IN ORDER in the contract file
/// (a trimmed copy omits interfaces/worlds and doc comments, never edits a
/// kept line), and the four trimmed guest copies must be byte-identical to
/// each other.
#[test]
fn vendored_wit_copies_match_the_frozen_contract() {
    let docs = docs_wit();

    let trimmed_paths = [
        "../../../components/samples/node-rs/wit/deps/wamn-node/package.wit",
        "../../../components/samples/node-ts/wit/deps/wamn-node/package.wit",
        "../../../components/fixtures/flow-driver/wit/deps/wamn-node/package.wit",
        "../guest/wit/deps/wamn-node/package.wit",
    ];
    let first = fs::read_to_string(root().join(trimmed_paths[0])).expect("trimmed copy reads");
    for p in &trimmed_paths[1..] {
        let other = fs::read_to_string(root().join(p)).expect("trimmed copy reads");
        assert_eq!(
            first, other,
            "trimmed guest copies diverged: {p} != {}",
            trimmed_paths[0]
        );
    }

    // The 5.9 credentials copies (the caps-node bindings world + the
    // flowrunner component): a SECOND trim — just the vault interface — that
    // must stay byte-identical to each other and in-order within the contract.
    let cred_paths = [
        "../guest/wit-caps/deps/wamn-node/package.wit",
        "../../../components/execution/flowrunner/wit/deps/wamn-node/package.wit",
        // cjv.3: the direct-import threat fixture imports the SAME trimmed
        // credentials interface a custom node would.
        "../../../components/fixtures/cred-probe/wit/deps/wamn-node/package.wit",
    ];
    let cred_first = fs::read_to_string(root().join(cred_paths[0])).expect("cred copy reads");
    for p in &cred_paths[1..] {
        let other = fs::read_to_string(root().join(p)).expect("cred copy reads");
        assert_eq!(
            cred_first, other,
            "credentials copies diverged: {p} != {}",
            cred_paths[0]
        );
    }

    let mut copies: Vec<(&str, String)> = vec![(
        "crates/platform/runtime/wit/deps/wamn-node/package.wit",
        fs::read_to_string(root().join("../../platform/runtime/wit/deps/wamn-node/package.wit"))
            .expect("host copy reads"),
    )];
    copies.push((trimmed_paths[0], first));
    copies.push((cred_paths[0], cred_first));
    copies.push((
        "../guest/wit-payload/deps/wamn-node/package.wit",
        fs::read_to_string(root().join("../guest/wit-payload/deps/wamn-node/package.wit"))
            .expect("payload copy reads"),
    ));
    copies.push((
        "../../../components/samples/node-cred/wit/deps/wamn-node/package.wit",
        fs::read_to_string(
            root().join("../../../components/samples/node-cred/wit/deps/wamn-node/package.wit"),
        )
        .expect("credential-bearing node copy reads"),
    ));

    for (name, copy) in &copies {
        assert_code_subsequence(name, copy, &docs);
    }
}

#[test]
fn payload_streams_are_the_pinned_host_p2_interface() {
    const PIN: &str = "wasi:io/streams@0.2.12";
    const IMPORT: &str = "use wasi:io/streams@0.2.12.{input-stream, output-stream};";

    let docs = docs_wit();
    let host = workspace_wit("crates/platform/runtime/wit/deps/wamn-node/package.wit");
    for (name, wit) in [
        ("docs/archive/contracts/wamn-node.wit", &docs),
        (
            "crates/platform/runtime/wit/deps/wamn-node/package.wit",
            &host,
        ),
    ] {
        assert_eq!(
            code_lines(wit)
                .into_iter()
                .filter(|line| line.contains("wasi:io/streams@"))
                .collect::<Vec<_>>(),
            vec![IMPORT],
            "{name}: payloads must use exactly {PIN}"
        );
        assert!(
            !wit.contains("stream<"),
            "{name}: the frozen 0.1 payload ABI must remain P2 resource-based"
        );
    }

    assert_eq!(
        block_lines(&docs, "interface payloads {"),
        block_lines(&host, "interface payloads {"),
        "the host binding copy must retain the authoritative payload interface"
    );
}

#[test]
fn host_wasi_io_dependency_matches_the_pinned_generated_copy() {
    let generated = workspace_wit("components/execution/flowrunner/wit/deps/wasi-io/package.wit");
    assert!(generated.starts_with("package wasi:io@0.2.12;"));

    let mut host_lines = vec!["package wasi:io@0.2.12;".to_string()];
    for file in ["error.wit", "poll.wit", "streams.wit", "world.wit"] {
        let path = format!("crates/platform/runtime/wit/deps/wasi-io/{file}");
        let copy = workspace_wit(&path);
        assert!(
            copy.starts_with("package wasi:io@0.2.12;"),
            "{path}: wrong wasi:io package version"
        );
        let without_repeated_package = copy
            .strip_prefix("package wasi:io@0.2.12;")
            .expect("version checked above");
        host_lines.extend(
            code_lines(without_repeated_package)
                .into_iter()
                .map(str::to_string),
        );
    }

    assert_eq!(
        host_lines,
        code_lines(&generated),
        "the host dependency must match the exact generated wasi:io@0.2.12 code"
    );
}

#[test]
fn payload_guest_import_is_bounded_and_pinned_to_the_frozen_p2_contract() {
    let world = workspace_wit("crates/node/guest/wit-payload/world.wit");
    let node = workspace_wit("crates/node/guest/wit-payload/deps/wamn-node/package.wit");
    let wasi = workspace_wit("crates/node/guest/wit-payload/deps/wasi-io/package.wit");
    let generated_wasi =
        workspace_wit("components/execution/flowrunner/wit/deps/wasi-io/package.wit");

    assert_eq!(
        code_lines(&world),
        vec![
            "package wamn:node-payload-guest@0.1.0;",
            "world payload-api {",
            "import wamn:node/payloads@0.1.0;",
            "}",
        ],
        "payload guest world must import only the frozen payload interface"
    );
    assert_code_subsequence("payload guest node copy", &node, &docs_wit());
    assert_code_subsequence("payload guest wasi:io copy", &wasi, &generated_wasi);
    assert!(wasi.starts_with("package wasi:io@0.2.12;"));
    assert!(node.contains("use wasi:io/streams@0.2.12.{input-stream, output-stream};"));
    assert!(
        !node.contains("blocking"),
        "node ABI exposes stream resources, not buffering helpers"
    );
    for forbidden in ["read-to-end", "collect-payload", "stream<"] {
        assert!(
            !world.contains(forbidden) && !node.contains(forbidden) && !wasi.contains(forbidden),
            "payload guest bindings expose forbidden whole-object/P3 form: {forbidden}"
        );
    }
}

#[test]
fn frozen_node_packages_reject_a_p3_or_0_2_drift() {
    let mut copies = vec![("docs/archive/contracts/wamn-node.wit", docs_wit())];
    copies.extend(
        VENDORED_WIT_PATHS
            .into_iter()
            .map(|path| (path, workspace_wit(path))),
    );

    for (path, wit) in copies {
        assert!(
            wit.contains("package wamn:node@0.1.0;"),
            "{path}: frozen node package identity changed"
        );
        assert!(
            !wit.contains("package wamn:node@0.2") && !wit.contains("stream<"),
            "{path}: accidental node 0.2/P3 ABI"
        );
    }
}

#[test]
fn every_invocation_wit_requires_input_context() {
    let docs = docs_wit();
    assert!(
        block_lines(&docs, "record run-context {").contains(&"context: json,"),
        "canonical run-context must carry input context"
    );
    for path in FULL_TYPE_WIT_PATHS {
        let wit = workspace_wit(path);
        assert!(
            block_lines(&wit, "record run-context {").contains(&"context: json,"),
            "{path}: run-context must carry input context"
        );
    }
}

#[test]
fn replacement_context_exists_only_on_success_emissions() {
    let mut copies = vec![("docs/archive/contracts/wamn-node.wit", docs_wit())];
    copies.extend(
        FULL_TYPE_WIT_PATHS
            .into_iter()
            .map(|path| (path, workspace_wit(path))),
    );

    for (path, wit) in copies {
        let emission = block_lines(&wit, "record emission {");
        assert!(
            emission.contains(&"ctx: option<json>,"),
            "{path}: success emission must allow replacement context"
        );
        for declaration in [
            "record error-detail {",
            "record rate-limit-detail {",
            "variant node-error {",
        ] {
            assert!(
                !block_lines(&wit, declaration)
                    .iter()
                    .any(|line| line.starts_with("ctx:")),
                "{path}: {declaration} must remain context-free"
            );
        }
        assert_eq!(
            code_lines(&wit)
                .into_iter()
                .filter(|line| *line == "ctx: option<json>,")
                .count(),
            1,
            "{path}: replacement ctx belongs only to the success emission"
        );
    }
}

/// The exact WIT spellings the SDK's native types mirror. Changing any of
/// these lines in the contract is a breaking 0.2 change AND requires the
/// mirror (ctx.rs / error.rs / Emission) to move in lockstep.
#[test]
fn sdk_mirrors_the_frozen_wit() {
    let docs = docs_wit();
    let lines = code_lines(&docs);
    let has = |l: &str| lines.contains(&l);

    assert!(
        docs.contains("STATUS: FROZEN 0.1.0"),
        "freeze header present"
    );

    // node-error: five variants, variant for variant (error.rs).
    for v in [
        "retryable(error-detail),",
        "rate-limited(rate-limit-detail),",
        "terminal(error-detail),",
        "invalid-input(error-detail),",
        "cancelled,",
    ] {
        assert!(has(v), "node-error variant line missing: {v:?}");
    }

    // rate-limit-detail (error.rs RateLimitDetail incl the throttle key).
    for l in [
        "retry-after-ms: option<u64>,",
        "target-host: option<string>,",
    ] {
        assert!(has(l), "rate-limit-detail line missing: {l:?}");
    }

    // emission (lib.rs Emission; port == MAIN_PORT travels absent).
    assert!(has("record emission {"), "emission record missing");
    assert!(has("port: option<string>,"), "emission port line missing");
    assert!(
        has("ctx: option<json>,"),
        "emission replacement context line missing"
    );
    assert!(
        has("run: func(ctx: run-context, input: payload) -> result<emission, node-error>;"),
        "run signature missing"
    );

    // run-context (ctx.rs RunContext), field for field.
    for l in [
        "run-id: string,",
        "flow-id: string,",
        "flow-version: u32,",
        "node-id: string,",
        "attempt: u32,",
        "idempotency-key: string,",
        "traceparent: option<string>,",
        "tracestate: option<string>,",
        "deadline-ms: option<u64>,",
        "config: json,",
        "context: json,",
    ] {
        assert!(has(l), "run-context field line missing: {l:?}");
    }

    // error-detail (error.rs ErrorDetail).
    for l in [
        "message: string,",
        "code: option<string>,",
        "data: option<json>,",
    ] {
        assert!(has(l), "error-detail field line missing: {l:?}");
    }

    // credentials (ctx.rs CredentialCapError + NodeCtx::credential — the 5.9
    // vault; the SDK facade is deliberately no-arg over the DECLARED name,
    // while the WIT `get` carries the handle across the component boundary).
    for l in [
        "interface credentials {",
        "variant credential-error {",
        "not-granted,",
        "not-found,",
        "get: func(handle: string) -> result<string, credential-error>;",
    ] {
        assert!(has(l), "credentials line missing: {l:?}");
    }
}
