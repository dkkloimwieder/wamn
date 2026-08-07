//! Pins FLOW-SPEC's current recovery authority to the shipped artifact/runtime model.

use std::path::Path;

const FLOW_SPEC: &str = include_str!("../../../docs/execution/FLOW-SPEC.md");
const DOCS_INDEX: &str = include_str!("../../../docs/README.md");
const MANIFEST_SOURCE: &str = include_str!("../../../crates/node/manifest/src/lib.rs");
const CATALOG_SOURCE: &str = include_str!("../../../crates/catalog/model/src/lib.rs");
const FLOWRUNNER_SOURCE: &str = include_str!("../../../components/execution/flowrunner/src/lib.rs");
const DISPOSITION_SOURCE: &str =
    include_str!("../../../crates/execution/run-state/src/disposition.rs");
const CTL_DISPOSITION_SOURCE: &str =
    include_str!("../../../services/ctl/src/effect_disposition.rs");
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
fn claimed_runner_consumes_resolution_before_every_dispatch_boundary() {
    let claimed = FLOWRUNNER_SOURCE
        .split_once("fn execute_claimed(")
        .expect("claimed executor")
        .1
        .split_once("fn run_next(")
        .expect("claimed executor boundary")
        .0;
    let dispatch_step = claimed
        .split_once("Step::Dispatch(d) => {")
        .expect("claimed dispatch step")
        .1;
    let resolution = dispatch_step
        .find("load_current_resolution(")
        .expect("resolution read");
    let attempt = dispatch_step
        .find("begin_attempt(")
        .expect("attempt intent");
    let node_dispatch = dispatch_step.find("dispatch_node(").expect("node dispatch");

    assert!(
        resolution < attempt,
        "resolution moved after attempt admission"
    );
    assert!(
        resolution < node_dispatch,
        "resolution moved after node/network dispatch"
    );
}

#[test]
fn uncertain_never_replay_parks_and_release_does_not_grant_dispatch() {
    let claimed = FLOWRUNNER_SOURCE
        .split_once("fn execute_claimed(")
        .expect("claimed executor")
        .1
        .split_once("fn run_next(")
        .expect("claimed executor boundary")
        .0;
    let uncertain = claimed
        .split_once("AttemptStartResult::EffectUncertain =>")
        .expect("effect-uncertain arm")
        .1
        .split_once("AttemptStartResult::MissingAttemptKey")
        .expect("end of effect-uncertain arm")
        .0;
    assert!(uncertain.contains("park_effect_uncertain("));
    assert!(uncertain.contains("outcome: 1"));
    assert!(uncertain.contains("already_settled: true"));
    assert!(!uncertain.contains("terminalize"));
    assert!(!uncertain.contains("mark_attempt_dispatched"));
    assert!(!uncertain.contains("dispatch_node"));

    assert!(DISPOSITION_SOURCE.contains("d.action = 'resolve'"));
}

#[test]
fn disposition_append_is_not_ordinary_application_dml() {
    for required in [
        "CREATE FUNCTION wamn_run.guard_effect_disposition_append()",
        "effect-disposition-append-requires-trusted-adapter",
        "REVOKE INSERT ON wamn_run.effect_disposition_requests FROM wamn_app",
        "REVOKE INSERT ON wamn_run.effect_dispositions FROM wamn_app",
        "effect_disposition_requests_insert_guard",
        "effect_dispositions_insert_guard",
        "CREATE FUNCTION wamn_run.park_effect_uncertain(",
        "'executor-auth-required'",
        "SET search_path = pg_catalog, wamn_run, pg_temp",
        "SET search_path = pg_catalog, pg_temp",
        "pg_catalog.pg_roles",
        "append_ordinal bigint GENERATED ALWAYS AS IDENTITY",
        "effect_dispositions_outcome_check CHECK ((",
        ") IS TRUE)",
    ] {
        assert!(
            RUN_STATE_DDL.contains(required),
            "disposition DML boundary lost {required:?}"
        );
    }
    let automatic_park = RUN_STATE_DDL
        .split_once("CREATE FUNCTION wamn_run.park_effect_uncertain(")
        .expect("automatic park function")
        .1
        .split_once("REVOKE ALL ON FUNCTION wamn_run.park_effect_uncertain(")
        .expect("automatic park function boundary")
        .0;
    assert!(automatic_park.contains("SECURITY DEFINER"));
    assert!(automatic_park.contains("q.lease_expires_at <= now()"));
}

#[test]
fn platform_break_glass_derives_a_privileged_session_actor() {
    for required in [
        "SESSION_USER::text AS principal",
        "'platform-admin-break-glass'::text AS effective_role",
        "'platform-privilege-required'",
        "'break-glass-reason-required'",
    ] {
        assert!(
            DISPOSITION_SOURCE.contains(required),
            "platform disposition authority lost {required:?}"
        );
    }
    assert!(
        DISPOSITION_SOURCE.contains("$3::text AS ignored_principal"),
        "the legacy-shaped parameter must remain explicitly ignored"
    );
    assert!(DISPOSITION_SOURCE.contains("FROM pg_catalog.pg_roles"));
    assert!(!DISPOSITION_SOURCE.contains("wamn_platform_admin"));
    assert!(CTL_DISPOSITION_SOURCE.contains("IsolationLevel::Serializable"));
    assert!(CTL_DISPOSITION_SOURCE.contains("SqlState::T_R_SERIALIZATION_FAILURE"));
    assert!(CTL_DISPOSITION_SOURCE.contains("SERIALIZATION_ATTEMPTS"));
}

#[test]
fn bulk_disposition_is_bounded_ordered_and_all_or_none() {
    for required in [
        "bounded_attempts AS MATERIALIZED",
        "candidates AS MATERIALIZED",
        "row_number() OVER (ORDER BY n.attempt_started_at, n.attempt_id)",
        "authorized a WHERE a.result_code='ready'",
        "connection-generation-required",
        "bounded-window-required",
        "ORDER BY c.selection_ordinal",
    ] {
        assert!(
            DISPOSITION_SOURCE.contains(required),
            "bounded disposition invariant lost {required:?}"
        );
    }
    let run_lock = DISPOSITION_SOURCE
        .find("locked_runs AS MATERIALIZED")
        .expect("run lock");
    let queue_lock = DISPOSITION_SOURCE
        .find("locked_queues AS MATERIALIZED")
        .expect("queue lock");
    let node_lock = DISPOSITION_SOURCE
        .find("locked_projections AS MATERIALIZED")
        .expect("node lock");
    assert!(run_lock < queue_lock && queue_lock < node_lock);
    assert!(DISPOSITION_SOURCE[node_lock..].contains("JOIN locked_queues q"));
    assert!(DISPOSITION_SOURCE.contains("current_setting('transaction_isolation') <> 'serializable'"));
    assert!(DISPOSITION_SOURCE.contains("ORDER BY d.append_ordinal DESC"));
    assert!(DISPOSITION_SOURCE.contains("THEN 'run-terminal'"));
    assert!(DISPOSITION_SOURCE.contains("pg_input_is_valid($9::text, 'jsonb')"));
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
