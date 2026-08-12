//! Guards the one-attempt, single-dispatch effect contract.

use std::fs;
use std::path::{Path, PathBuf};

const ACTIVE_PLAN: &str = "docs/archive/PLAN/PLAN.md";
const FLOW_SPEC: &str = "docs/archive/execution/FLOW-SPEC.md";
const MVP_CHARTER: &str = "docs/scope-reduction-mvp.md";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read_document(relative_path: &str) -> String {
    let path = repository_root().join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn normalize(document: &str) -> String {
    document.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn section<'a>(document: &'a str, start: &str, end: &str) -> &'a str {
    let (_, remainder) = document
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start {start:?}"));
    remainder
        .split_once(end)
        .map_or(remainder, |(contents, _)| contents)
}

#[test]
fn one_attempt_and_single_dispatch_are_folded_back() {
    let plan = read_document(ACTIVE_PLAN);
    let effect_posture = normalize(section(
        &plan,
        "### v1 effect posture — one attempt, one dispatch",
        "**Checkpoint recovery must not foreclose fan-out.**",
    ));

    for required in [
        "a pure occurrence writes no effect-ledger row",
        "one immutable write-ahead attempt, at most one immutable dispatch fact",
        "different facts refuse",
        "first successful insert the sole wire-I/O permit",
        "`wamn-0h0g.4.9` lands the inaccessible ledger primitive, private run-state API, and database proofs without a production caller.",
        "A sent attempt without a recorded outcome is `effect-uncertain`; it never sends again.",
        "There is no success assertion, continuation, bulk selection, successor attempt, or silent re-execution.",
    ] {
        assert!(
            effect_posture.contains(required),
            "effect posture lost required text: {required:?}"
        );
    }

    let charter = read_document(MVP_CHARTER);
    let execution_cut = normalize(section(
        &charter,
        "### 4 · Execution: crash floor, single path, flow calls",
        "### 5 · Publish gate",
    ));

    for required in [
        "pure → no effect attempt",
        "effectful → one immutable write-ahead attempt identity · at most one dispatch record",
        "`wamn-0h0g.4.9` lands the inaccessible run-state primitive and its database proofs; it claims no production dispatch activation.",
        "A sent attempt without a recorded outcome is `effect-uncertain`; neither reclaim nor an admission retry sends it again.",
        "Inbound admission idempotency selects the existing run; it never licenses effect redispatch.",
    ] {
        assert!(
            execution_cut.contains(required),
            "MVP execution cut lost required text: {required:?}"
        );
    }

    let flow_spec = read_document(FLOW_SPEC);
    let attempt_protocol = normalize(section(
        &flow_spec,
        "### 10.3 Node-attempt protocol",
        "### 10.4 Inline lease ownership",
    ));

    for required in [
        "Pure occurrences skip all four protocol operations.",
        "the first successful dispatch insert for that exact occurrence",
        "Attempt and outcome retries are exact-idempotent",
        "any difference refuses",
        "A sent attempt without a recorded outcome is **`effect-uncertain`** and never sends again.",
    ] {
        assert!(
            attempt_protocol.contains(required),
            "FLOW-SPEC attempt protocol lost required text: {required:?}"
        );
    }
}

#[test]
fn retired_effect_retry_taxonomy_stays_deleted() {
    let documents = format!(
        "{}\n{}\n{}",
        read_document(ACTIVE_PLAN),
        read_document(FLOW_SPEC),
        read_document(MVP_CHARTER),
    )
    .to_ascii_lowercase();

    for retired in [
        "stable-key-dedup-v1",
        "stable key",
        "stable_key",
        "idempotent-with-key",
        "idempotent with key",
        "idempotent_with_key",
        "never-replay",
        "never replay",
        "never_replay",
        "operation fingerprint",
        "operation-fingerprint",
        "operation_fingerprint",
        "semantic attestation",
        "semantic-attestation",
        "semantic_attestation",
        "multi-dispatch",
        "multi dispatch",
        "multi_dispatch",
        "attempt_key",
        "attempt-key",
    ] {
        assert!(
            !documents.contains(retired),
            "retired effect-retry vocabulary returned: {retired:?}"
        );
    }
}
