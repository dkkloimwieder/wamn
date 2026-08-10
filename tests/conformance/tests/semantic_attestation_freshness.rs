//! Guards the PLAN-2B semantic-attestation freshness decision.

use std::fs;
use std::path::{Path, PathBuf};

const ACTIVE_PLAN: &str = "docs/archive/PLAN/PLAN.md";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
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
fn semantic_attestation_freshness_decision_is_folded_back_fail_closed() {
    let plan_path = repository_root().join(ACTIVE_PLAN);
    let plan = fs::read_to_string(&plan_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", plan_path.display()));
    let connections = normalize(section(
        &plan,
        "### 2B · Connections — the env boundary for anything external",
        "### 2C · Node authoring as a product",
    ));

    for required in [
        "Decision (wamn-ko5r.4): every strengthening semantic-attestation type owns a fail-closed freshness contract; external claims are never indefinite operator responsibility.",
        "maximum validity window, revalidation procedure, semantic scope, and complete set of material invalidation inputs",
        "HTTP `0.1` / `stable-key-dedup-v1`",
        "Time-bounded, periodically revalidated end-to-end evidence",
        "configured proxy and every admitted primary/failover route",
        "remote upgrades and dedup-retention changes",
        "proxy route or header policy, TLS-authenticated service identity, credential principal or tenant scope, named idempotency domain",
        "DNS answers are neither semantic proof nor an automatic invalidation",
        "resolution outside the attested TLS service identity and idempotency scope is an explicit refusal",
        "absence of a policy means only the conservative recovery default is available",
        "fails explicitly",
        "never silently downgrades to `never-replay`",
    ] {
        assert!(
            connections.contains(required),
            "PLAN-2B semantic-attestation decision lost required text: {required:?}"
        );
    }

    let open_decisions = normalize(section(&plan, "## Open decisions", "## Known gaps"));
    assert!(
        open_decisions.contains(
            "~~What evidence, freshness, and invalidation rules apply per semantic-attestation type~~"
        ) && open_decisions.contains(
            "**Settled (wamn-ko5r.4):** each strengthening connection claim declares bounded evidence"
        ) && open_decisions.contains(
            "No expiry, revocation, or invalidation may silently downgrade to `never-replay`. | — |"
        ),
        "the semantic-attestation question must remain visibly settled and unblocked"
    );
}
