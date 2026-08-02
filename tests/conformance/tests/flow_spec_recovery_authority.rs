//! Pins FLOW-SPEC's current recovery authority to the shipped artifact/runtime model.

use std::path::Path;

const FLOW_SPEC: &str = include_str!("../../../docs/execution/FLOW-SPEC.md");
const DOCS_INDEX: &str = include_str!("../../../docs/README.md");
const MANIFEST_SOURCE: &str = include_str!("../../../crates/node/manifest/src/lib.rs");
const CATALOG_SOURCE: &str = include_str!("../../../crates/catalog/model/src/lib.rs");
const FLOWRUNNER_SOURCE: &str = include_str!("../../../components/execution/flowrunner/src/lib.rs");
const CATALOG_DDL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const RUN_STATE_DDL: &str = include_str!("../../../deploy/sql/run-state.sql");

fn normative_spec() -> &'static str {
    FLOW_SPEC
        .split_once("## 20. Revision history")
        .expect("FLOW-SPEC must retain an explicit revision-history boundary")
        .0
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn flow_spec_names_the_shipped_three_layer_recovery_authority() {
    let current = normalized(normative_spec());
    for required in [
        "Recovery authority has three immutable-to-admission layers",
        "ResolvedNodeContract.executable_recovery",
        "flow_artifacts.occurrence_recovery_json",
        "flow_artifacts.occurrence_recovery_hash",
        "load_pinned_artifact",
        "PinnedArtifact::from_storage",
        "admit_occurrence_recovery",
        "selected_recovery_class",
        "effective `recovery_class`",
        "generation_fact_kind",
        "connection_generation",
        "credential_generation",
        "standard and custom nodes use this same executable-contract model",
        "Environment attestations may satisfy a pinned portable requirement or cause a refusal",
        "capture mode are not recovery authorities",
    ] {
        assert!(
            current.contains(required),
            "FLOW-SPEC lost shipped recovery authority statement {required:?}"
        );
    }
}

#[test]
fn flow_spec_normative_policy_rejects_legacy_recovery_classifiers() {
    let current = normalized(normative_spec());
    for forbidden in [
        "Purity comes from the pinned interface",
        "GET/HEAD `replay` policy-gated",
        "PUT keyed-by-content",
        "DELETE keyed-by-identity `idempotent-with-key`",
        "The purity override rule",
        "reconstructed on recovery by re-executing pure nodes",
    ] {
        assert!(
            !current.contains(forbidden),
            "legacy recovery classifier returned to normative FLOW-SPEC: {forbidden:?}"
        );
    }

    for invariant in [
        "GET and HEAD do not authorize replay",
        "PUT or DELETE do not authorize idempotent replay",
        "Mutable configuration cannot strengthen a pinned selection",
        "Capture is independently optional and has no role in classification or admission",
        "Environment facts never strengthen, weaken, or retarget the selected class",
    ] {
        assert!(
            current.contains(invariant),
            "FLOW-SPEC lost fail-closed recovery invariant {invariant:?}"
        );
    }
}

#[test]
fn shipped_sources_match_the_specified_fields_readers_and_ledger() {
    for required in [
        "pub struct ResolvedNodeContract",
        "pub executable_recovery: ExecutableRecoveryContract",
    ] {
        assert!(
            MANIFEST_SOURCE.contains(required),
            "resolved-contract source lost {required:?}"
        );
    }
    for required in [
        "pub struct PinnedArtifact",
        "pub fn from_storage(",
        "occurrence_recovery_json: Option<&str>",
        "occurrence_recovery_hash: Option<&str>",
    ] {
        assert!(
            CATALOG_SOURCE.contains(required),
            "pinned-artifact reader lost {required:?}"
        );
    }
    for required in [
        "fn load_pinned_artifact(",
        "wamn_catalog::PinnedArtifact::from_storage(",
        "fn admit_occurrence_recovery(",
    ] {
        assert!(
            FLOWRUNNER_SOURCE.contains(required),
            "flowrunner recovery path lost {required:?}"
        );
    }
    for required in [
        "occurrence_recovery_json text",
        "occurrence_recovery_hash text",
    ] {
        assert!(
            CATALOG_DDL.contains(required),
            "catalog artifact persistence lost {required:?}"
        );
    }
    for required in [
        "selected_recovery_class text",
        "recovery_class text",
        "generation_fact_kind text",
        "connection_generation text",
        "credential_generation text",
    ] {
        assert!(
            RUN_STATE_DDL.contains(required),
            "attempt ledger lost {required:?}"
        );
    }
}

#[test]
fn flow_spec_revision_index_links_and_section_references_resolve() {
    assert!(FLOW_SPEC.contains("NORMATIVE DRAFT — REVISION 19 (2026-08-02)"));
    assert!(FLOW_SPEC.contains("**Rev 19: recovery authority reconciled"));
    assert!(DOCS_INDEX.contains("[FLOW-SPEC.md](execution/FLOW-SPEC.md)** (rev 19, normative)"));
    assert!(DOCS_INDEX.contains("superseded by callable-flow rev19"));

    let docs_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    for target in markdown_link_targets(DOCS_INDEX) {
        if target.contains("://") {
            continue;
        }
        let path = target.split('#').next().expect("link target has a path");
        assert!(
            docs_dir.join(path).exists(),
            "docs index link does not resolve: {target}"
        );
    }

    for section in section_references(normative_spec()) {
        let heading = format!("## {section}");
        let subheading = format!("### {section}");
        let inline = format!("**{section}");
        assert!(
            FLOW_SPEC.contains(&heading)
                || FLOW_SPEC.contains(&subheading)
                || FLOW_SPEC.contains(&inline),
            "FLOW-SPEC cross-reference §{section} does not resolve"
        );
    }
}

fn markdown_link_targets(markdown: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut remaining = markdown;
    while let Some(start) = remaining.find("](") {
        remaining = &remaining[start + 2..];
        let Some(end) = remaining.find(')') else {
            break;
        };
        targets.push(&remaining[..end]);
        remaining = &remaining[end + 1..];
    }
    targets
}

fn section_references(markdown: &str) -> Vec<String> {
    let mut sections = Vec::new();
    for tail in markdown.split('§').skip(1) {
        let section = tail
            .chars()
            .take_while(|character| character.is_ascii_digit() || *character == '.')
            .collect::<String>()
            .trim_end_matches('.')
            .to_string();
        if !section.is_empty() && !sections.contains(&section) {
            sections.push(section);
        }
    }
    sections
}
