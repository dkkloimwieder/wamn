//! Guards the shelved one-attempt, single-dispatch effect contract.

use std::fs;
use std::path::{Path, PathBuf};

const EXECUTION_MODEL: &str = "docs/exe-model.md";

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
fn premium_shelf_keeps_one_attempt_and_single_dispatch() {
    let model = read_document(EXECUTION_MODEL);
    let shelf = normalize(section(
        &model,
        "### Premium durable shelf contract",
        "## Data, identity and generated APIs",
    ));

    for required in [
        "A pure occurrence writes no effect-ledger row.",
        "one immutable write-ahead attempt and at most one immutable dispatch fact",
        "exact retries are no-ops and different facts refuse",
        "The first successful dispatch insert is the sole wire-I/O permit.",
        "A sent attempt without a recorded outcome is `effect-uncertain`; it never sends again.",
        "Admission idempotency selects the existing run and never licenses effect redispatch.",
        "There is no success assertion, continuation, bulk selection, successor attempt or silent re-execution.",
    ] {
        assert!(
            shelf.contains(required),
            "premium durable shelf lost required text: {required:?}"
        );
    }
}

#[test]
fn retired_effect_retry_taxonomy_stays_deleted() {
    let model = read_document(EXECUTION_MODEL).to_ascii_lowercase();

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
            !model.contains(retired),
            "retired effect-retry vocabulary returned: {retired:?}"
        );
    }
}
