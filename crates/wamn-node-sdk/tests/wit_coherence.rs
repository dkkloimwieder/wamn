//! Drift-guards tying the FROZEN `wamn:node` 0.1 contract file
//! (`docs/wamn-node.wit`) to (a) every vendored copy of it and (b) the exact
//! WIT lines this SDK mirrors natively. The wamn-catalog/wamn-flow
//! committed-contract pattern: editing the contract without updating the
//! mirrors (or vice versa) fails a named test instead of shipping skew.

use std::fs;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn docs_wit() -> String {
    fs::read_to_string(root().join("../../docs/wamn-node.wit")).expect("docs/wamn-node.wit reads")
}

/// Comment- and blank-stripped, whitespace-trimmed code lines.
fn code_lines(wit: &str) -> Vec<&str> {
    wit.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect()
}

/// Every vendored copy's code lines must appear IN ORDER in the contract file
/// (a trimmed copy omits interfaces/worlds and doc comments, never edits a
/// kept line), and the four trimmed guest copies must be byte-identical to
/// each other.
#[test]
fn vendored_wit_copies_match_the_frozen_contract() {
    let docs = docs_wit();
    let docs_lines = code_lines(&docs);

    let trimmed_paths = [
        "../../components/samples/node-rs/wit/deps/wamn-node/package.wit",
        "../../components/samples/node-ts/wit/deps/wamn-node/package.wit",
        "../../components/flow-driver/wit/deps/wamn-node/package.wit",
        "../wamn-node-guest/wit/deps/wamn-node/package.wit",
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
        "../wamn-node-guest/wit-caps/deps/wamn-node/package.wit",
        "../../components/flowrunner/wit/deps/wamn-node/package.wit",
        // cjv.3: the direct-import threat fixture imports the SAME trimmed
        // credentials interface a custom node would.
        "../../components/fixtures/cred-probe/wit/deps/wamn-node/package.wit",
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
        "crates/wamn-host/wit/deps/wamn-node/package.wit",
        fs::read_to_string(root().join("../wamn-host/wit/deps/wamn-node/package.wit"))
            .expect("host copy reads"),
    )];
    copies.push((trimmed_paths[0], first));
    copies.push((cred_paths[0], cred_first));

    for (name, copy) in &copies {
        let mut docs_iter = docs_lines.iter();
        for line in code_lines(copy) {
            assert!(
                docs_iter.any(|d| *d == line),
                "{name}: line {line:?} does not appear (in order) in docs/wamn-node.wit — \
                 a vendored copy drifted from the frozen contract"
            );
        }
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
