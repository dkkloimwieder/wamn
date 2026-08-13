use wamn_run_state::admission::{AdmissionTransition, admission_transaction};

fn draft_admission_sql() -> String {
    admission_transaction(AdmissionTransition::PinnedScenarioDraft)
        .admit()
        .to_string()
}

#[test]
fn draft_admission_uses_one_exact_nonrelease_executable_pin() {
    let sql = draft_admission_sql();

    for predicate in [
        "d.draft_id = $10",
        "d.draft_revision = $11",
        "d.draft_content_hash = $12",
        "d.draft_artifact_hash = $13",
        "d.execution_bundle_hash = $14",
        "d.binding_base_artifact_hash = $15",
        "d.suite_flow_version = $16",
        "d.validated_draft_hash = $17",
        "$19 <> ''",
    ] {
        assert!(sql.contains(predicate), "draft admission omits {predicate}");
    }
    assert!(sql.contains("'scenario-draft'"));
    assert!(!sql.contains("trigger_source, capture_mode"));
    assert!(sql.contains("'producer', 'draft-scenario'"));
    assert!(sql.contains("'artifact-digest', d.draft_artifact_hash"));
    assert!(!sql.contains("release_flows"));
    assert!(!sql.contains("FROM flows"));
    assert!(sql.contains("'draft-content-hash', d.draft_content_hash"));
    assert!(sql.contains("'catalog-version', d.catalog_version"));
    assert!(sql.contains("'report-id', $19::text"));
}

#[test]
fn draft_admission_persists_validated_bundle_only_in_run_column() {
    let sql = draft_admission_sql();

    assert!(sql.contains("JOIN catalog.execution_bundles AS bundle"));
    assert!(sql.contains("bundle.execution_bundle_hash = d.execution_bundle_hash"));
    assert!(sql.contains("environment, execution_bundle_hash, status"));
    assert!(sql.contains("d.environment, d.execution_bundle_hash"));
    assert!(sql.contains("THEN 'missing-root-plan'"));
    assert!(sql.contains("THEN 'conflicting-run-identity'"));
    let retired_json_pin = ["execution", "bundle", "hash"].join("-");
    assert!(!sql.contains(&format!("'{retired_json_pin}'")));
    assert!(sql.contains("existing.execution_bundle_hash"));
}

#[test]
fn draft_duplicate_identity_covers_authoritative_pins_and_override_identity() {
    let sql = draft_admission_sql();

    for predicate in [
        "existing.flow_id IS DISTINCT FROM d.flow_id",
        "existing.flow_version IS DISTINCT FROM d.runtime_flow_version",
        "existing.catalog_id IS DISTINCT FROM d.catalog_id",
        "existing.catalog_version IS DISTINCT FROM d.catalog_version",
        "existing.environment IS DISTINCT FROM d.environment",
        "existing.capture_mode IS DISTINCT FROM 'off'",
        "existing.execution_bundle_hash \
                           IS DISTINCT FROM d.execution_bundle_hash",
        "existing.invocation_context #>> '{principal,artifact-digest}' \
                           IS DISTINCT FROM d.draft_artifact_hash",
        "existing.invocation_context #>> '{principal,draft-id}' \
                           IS DISTINCT FROM d.draft_id",
        "existing.invocation_context #>> '{principal,draft-revision}' \
                           IS DISTINCT FROM d.draft_revision::text",
        "existing.invocation_context #>> '{principal,draft-content-hash}' \
                           IS DISTINCT FROM d.draft_content_hash",
        "existing.invocation_context #>> '{principal,validated-draft-hash}' \
                           IS DISTINCT FROM d.validated_draft_hash",
        "existing.invocation_context #>> '{principal,binding-base-artifact-hash}' \
                           IS DISTINCT FROM d.binding_base_artifact_hash",
        "existing.invocation_context #>> '{principal,suite-flow-version}' \
                           IS DISTINCT FROM d.suite_flow_version::text",
        "existing.invocation_context #>> '{source,suite-id}' \
                           IS DISTINCT FROM $8::text",
        "existing.invocation_context #>> '{source,case-id}' \
                           IS DISTINCT FROM $9::text",
        "existing.invocation_context #>> '{source,report-id}' \
                           IS DISTINCT FROM $19::text",
    ] {
        assert!(sql.contains(predicate), "draft duplicate omits {predicate}");
    }
}

#[test]
fn draft_connection_authority_is_exact_generation_and_default_deny() {
    let sql = draft_admission_sql();

    assert!(sql.contains("base_requirement.requirement_json = requirement.value -> 'requirement'"));
    assert!(!sql.contains("base_requirement.requirement_json -> 'requirement'"));
    assert!(sql.contains("binding.environment = d.environment"));
    assert!(sql.contains("instance.requirement_type"));
    assert!(sql.contains("instance.contract"));
    assert!(sql.contains("generation.generation = instance.active_generation"));
    assert!(sql.contains("grant_row.generation = generation.generation"));
    assert!(sql.contains("grant_row.revoked_at IS NULL"));
    assert!(sql.contains("WHERE NOT decision.authorized"));
    assert!(sql.contains("THEN 'draft-connections-denied'"));
    assert!(!sql.contains("COALESCE(authorized, true)"));
}

#[test]
fn queue_write_depends_on_the_fully_pinned_run_insert() {
    let sql = draft_admission_sql();
    let run = sql.find("inserted_run AS").unwrap();
    let queue = sql.find("inserted_queue AS").unwrap();
    let outcome = sql.find("END AS result_code").unwrap();

    assert!(run < queue && queue < outcome);
    assert!(sql.contains("SELECT tenant_id, run_id, NULL, 0, now() FROM inserted_run"));
    assert!(sql.contains("WHEN NOT EXISTS (SELECT 1 FROM draft) THEN 'draft-drift'"));
}
