//! Persistence statements for the internal draft-to-suite application path.
//!
//! Draft document and validated-artifact semantics remain owned by
//! `wamn-catalog`; this module owns only the typed application-store boundary
//! beside the stored suites and reports it executes.

/// Insert the first revision of one mutable flow draft.
///
/// Params: tenant, draft id, flow id, JSON document text.
pub fn insert_flow_draft_sql() -> &'static str {
    "INSERT INTO catalog.flow_drafts \
       (tenant_id, draft_id, flow_id, revision, graph_json) \
     VALUES ($1, $2, $3, 1, $4::text::jsonb) \
     ON CONFLICT (tenant_id, draft_id) DO NOTHING \
     RETURNING revision, edited_at"
}

/// Replace one mutable draft only at its caller-observed revision.
///
/// Params: tenant, draft id, flow id, expected revision, JSON document text.
pub fn update_flow_draft_sql() -> &'static str {
    "UPDATE catalog.flow_drafts \
        SET revision = revision + 1, graph_json = $5::text::jsonb, \
            edited_at = GREATEST(clock_timestamp(), edited_at + interval '1 microsecond') \
      WHERE tenant_id = $1 AND draft_id = $2 AND flow_id = $3 AND revision = $4 \
      RETURNING revision, edited_at"
}

/// Read one exact draft revision for validation.
///
/// Params: tenant, draft id, revision.
pub fn select_flow_draft_sql() -> &'static str {
    "SELECT flow_id, graph_json::text, edited_at \
       FROM catalog.flow_drafts \
      WHERE tenant_id = $1 AND draft_id = $2 AND revision = $3"
}

/// Acquire the applied catalog-head row lock through the narrow run-plane
/// SECURITY DEFINER bridge. The host author role intentionally has no UPDATE
/// privilege on the catalog control table itself.
///
/// Params: tenant, catalog id, environment.
pub fn lock_draft_catalog_head_sql() -> &'static str {
    "SELECT lock_catalog_head($1, $2, $3) AS applied_catalog_version"
}

/// Resolve one exact applied catalog plus its source release member after the
/// caller has acquired [`lock_draft_catalog_head_sql`] in the same transaction.
///
/// The stored suite remains version-bound; that released member supplies its
/// existing connection bindings but never supplies the graph executed by the
/// draft run.
///
/// Params: tenant, catalog id, environment, expected catalog version, flow id,
/// suite flow version.
pub fn select_draft_catalog_source_member_sql() -> &'static str {
    "SELECT head.applied_catalog_version, artifact.artifact_hash \
       FROM catalog.catalog_heads AS head \
       JOIN catalog.release_flows AS member \
         ON member.tenant_id = head.tenant_id \
        AND member.catalog_id = head.catalog_id \
        AND member.catalog_version = head.applied_catalog_version \
       JOIN catalog.flow_artifacts AS artifact \
         ON artifact.tenant_id = member.tenant_id \
        AND artifact.flow_id = member.flow_id \
        AND artifact.flow_version = member.flow_version \
      WHERE head.tenant_id = $1 AND head.catalog_id = $2 \
        AND head.environment = $3 AND head.applied_catalog_version = $4 \
        AND member.flow_id = $5 AND member.flow_version = $6"
}

/// Persist one immutable validation of a content-addressed draft.
///
/// Params: tenant, draft id, revision, draft-content hash, catalog id/version,
/// environment, source-suite flow version, runtime flow version, graph JSON/hash, artifact hash,
/// interface bundle JSON/hash, component digests JSON, occurrence recovery
/// JSON/hash, execution-bundle bytes/hash, immutable binding-base artifact hash,
/// validated-draft identity hash.
pub fn insert_validated_flow_draft_sql() -> &'static str {
    "INSERT INTO catalog.validated_flow_drafts \
       (tenant_id, draft_id, draft_revision, draft_edited_at, draft_content_hash, catalog_id, \
        catalog_version, environment, suite_flow_version, flow_id, runtime_flow_version, graph_json, \
        graph_hash, draft_artifact_hash, interface_bundle_json, interface_bundle_hash, \
        component_digests, occurrence_recovery_json, occurrence_recovery_hash, \
        execution_bundle_bytes, execution_bundle_hash, binding_base_artifact_hash, \
        validated_draft_hash) \
     SELECT document.tenant_id, document.draft_id, document.revision, document.edited_at, \
            $4, $5, $6, $7, $8, \
            document.flow_id, $9, document.graph_json, $11, $12, $13, $14, \
            $15::text::jsonb, $16, $17, $18, $19, $20, $21 \
       FROM catalog.flow_drafts AS document \
      WHERE document.tenant_id = $1 AND document.draft_id = $2 \
        AND document.revision = $3 AND document.graph_json = $10::text::jsonb \
     ON CONFLICT (tenant_id, draft_id, draft_revision, draft_content_hash, \
                  catalog_id, catalog_version, \
                  environment, suite_flow_version, runtime_flow_version, draft_artifact_hash, \
                  execution_bundle_hash, binding_base_artifact_hash) DO NOTHING \
     RETURNING draft_edited_at"
}

/// Read one exact immutable draft pin before admission.
///
/// Params: tenant, draft id/revision, draft-content hash, catalog id/version,
/// environment, source-suite version,
/// runtime version, ordinary artifact hash, bundle hash, binding-base artifact hash,
/// validated-draft identity hash.
pub fn select_validated_flow_draft_sql() -> &'static str {
    "SELECT draft_id, draft_revision, draft_edited_at, environment, flow_id, runtime_flow_version, \
            graph_json::text, graph_hash, draft_artifact_hash, interface_bundle_json::text, \
            interface_bundle_hash, component_digests::text, \
            occurrence_recovery_json::text, occurrence_recovery_hash, \
            execution_bundle_bytes, binding_base_artifact_hash, validated_at \
       FROM catalog.validated_flow_drafts \
      WHERE tenant_id = $1 AND draft_id = $2 AND draft_revision = $3 \
        AND draft_content_hash = $4 AND catalog_id = $5 AND catalog_version = $6 \
        AND environment = $7 AND suite_flow_version = $8 \
        AND runtime_flow_version = $9 AND draft_artifact_hash = $10 \
        AND execution_bundle_hash = $11 AND binding_base_artifact_hash = $12 \
        AND validated_draft_hash = $13"
}

/// Install or restore draft-safe authority on one exact immutable generation.
///
/// This is called only by the internal development-administrator adapter.
/// Params: tenant, environment, instance id, generation, reason.
pub fn grant_draft_safe_generation_sql() -> &'static str {
    "INSERT INTO catalog.draft_safe_connection_grants \
       (tenant_id, environment, instance_id, generation, reason) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (tenant_id, environment, instance_id, generation) DO UPDATE \
       SET revoked_at = NULL, reason = EXCLUDED.reason, \
           granted_at = GREATEST( \
               clock_timestamp(), \
               draft_safe_connection_grants.granted_at + interval '1 microsecond', \
               COALESCE(draft_safe_connection_grants.revoked_at + interval '1 microsecond', \
                        '-infinity'::timestamptz))"
}

/// Revoke draft-safe authority without mutating the connection generation.
///
/// Params: tenant, environment, instance id, generation.
pub fn revoke_draft_safe_generation_sql() -> &'static str {
    "UPDATE catalog.draft_safe_connection_grants SET revoked_at = clock_timestamp() \
      WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3 \
        AND generation = $4 AND revoked_at IS NULL"
}

/// Reserve one deterministic report identity before the first case admission.
///
/// Params: tenant, report id, execution id, flow id, suite flow version,
/// suite id, command JSON/hash, lineage JSON/hash.
pub fn insert_authoring_report_reservation_sql() -> &'static str {
    "INSERT INTO authoring_report_reservations \
       (tenant_id, report_id, execution_id, flow_id, suite_flow_version, suite_id, \
        command_json, command_hash, lineage_json, lineage_hash) \
     VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb, $8, $9::text::jsonb, $10) \
     ON CONFLICT DO NOTHING \
     RETURNING state"
}

/// Read the exact reservation for idempotence and pending lookup.
pub fn select_authoring_report_reservation_sql() -> &'static str {
    "SELECT execution_id, flow_id, suite_flow_version, suite_id, command_json::text, \
            command_hash, lineage_json::text, lineage_hash, state, created_at, finalized_at \
       FROM authoring_report_reservations \
      WHERE tenant_id = $1 AND report_id = $2"
}

/// Serialize finalizers for one deterministic reservation.
pub fn lock_authoring_report_reservation_state_sql() -> &'static str {
    "SELECT state FROM authoring_report_reservations \
      WHERE tenant_id = $1 AND report_id = $2 FOR UPDATE"
}

/// Insert one immutable observed case fact while its reservation is pending.
///
/// Params: tenant, report id, canonical ordinal, case id, run id, passed,
/// status, fail kind, fail node, outcome JSON.
pub fn insert_authoring_suite_case_fact_sql() -> &'static str {
    "INSERT INTO authoring_suite_case_facts \
       (tenant_id, report_id, ordinal, case_id, run_id, passed, status, fail_kind, \
        fail_node, outcome) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::text::jsonb) \
     ON CONFLICT DO NOTHING \
     RETURNING ordinal"
}

/// Read immutable case facts in canonical command order.
pub fn select_authoring_suite_case_facts_sql() -> &'static str {
    "SELECT ordinal, case_id, run_id, passed, status, fail_kind, fail_node, outcome::text \
       FROM authoring_suite_case_facts \
      WHERE tenant_id = $1 AND report_id = $2 \
      ORDER BY ordinal"
}

/// Insert the immutable suite-level report summary from complete facts or an
/// explicitly refused contiguous prefix.
///
/// Params: tenant, report id, execution id, flow id, suite flow version,
/// suite id, passed, lineage JSON/hash, edit-to-run milliseconds, refusal JSON.
pub fn insert_authoring_suite_report_sql() -> &'static str {
    "INSERT INTO authoring_suite_reports \
       (tenant_id, report_id, execution_id, flow_id, suite_flow_version, suite_id, \
        passed, lineage_json, lineage_hash, edit_to_run_ms, refusal) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::jsonb, $9, $10, $11::text::jsonb)"
}

/// Finalize exactly one pending reservation after its immutable summary exists.
pub fn finalize_authoring_report_reservation_sql() -> &'static str {
    "UPDATE authoring_report_reservations \
        SET state = 'finalized', finalized_at = clock_timestamp() \
      WHERE tenant_id = $1 AND report_id = $2 AND state = 'pending'"
}

/// Read one immutable report summary by tenant and report identity.
pub fn select_authoring_suite_report_sql() -> &'static str {
    "SELECT execution_id, flow_id, suite_flow_version, suite_id, passed, lineage_json::text, \
            edit_to_run_ms, refusal::text \
       FROM authoring_suite_reports \
      WHERE tenant_id = $1 AND report_id = $2"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimistic_draft_writes_never_overwrite_an_unseen_revision() {
        let update = update_flow_draft_sql();
        assert!(update.contains("revision = revision + 1"));
        assert!(update.contains("revision = $4"));
        assert!(update.contains("RETURNING revision, edited_at"));
        assert!(insert_flow_draft_sql().contains("ON CONFLICT (tenant_id, draft_id) DO NOTHING"));
    }

    #[test]
    fn validated_draft_identity_carries_catalog_and_bundle_pins() {
        let insert = insert_validated_flow_draft_sql();
        for field in [
            "draft_id",
            "draft_revision",
            "draft_edited_at",
            "draft_content_hash",
            "draft_artifact_hash",
            "catalog_id",
            "catalog_version",
            "environment",
            "suite_flow_version",
            "runtime_flow_version",
            "execution_bundle_bytes",
            "execution_bundle_hash",
            "binding_base_artifact_hash",
            "validated_draft_hash",
        ] {
            assert!(insert.contains(field), "validated insert omits {field}");
        }
        assert!(!insert.contains("release_manifests"));
        assert!(!insert.contains("INSERT INTO catalog.flow_artifacts"));
        assert!(insert.contains("document.graph_json = $10::text::jsonb"));
        assert!(insert.contains("document.flow_id, $9, document.graph_json"));
        assert!(
            insert.contains("ON CONFLICT (tenant_id, draft_id, draft_revision, draft_content_hash")
        );

        let select = select_validated_flow_draft_sql();
        for predicate in [
            "draft_id = $2",
            "draft_revision = $3",
            "draft_content_hash = $4",
            "environment = $7",
            "suite_flow_version = $8",
            "runtime_flow_version = $9",
            "draft_artifact_hash = $10",
            "execution_bundle_hash = $11",
            "binding_base_artifact_hash = $12",
            "validated_draft_hash = $13",
        ] {
            assert!(
                select.contains(predicate),
                "validated lookup omits {predicate}"
            );
        }
    }

    #[test]
    fn canonical_validated_artifact_json_survives_postgres_byte_exact() {
        let ddl = include_str!("../../../../deploy/sql/catalog-schema.sql");
        assert!(ddl.contains("interface_bundle_json     text NOT NULL"));
        assert!(ddl.contains("jsonb_typeof(interface_bundle_json::jsonb) = 'array'"));
        assert!(ddl.contains("occurrence_recovery_json  text,"));
        assert!(ddl.contains("jsonb_typeof(occurrence_recovery_json::jsonb) = 'array'"));

        let insert = insert_validated_flow_draft_sql();
        assert!(insert.contains("$11, $12, $13, $14"));
        assert!(insert.contains("$15::text::jsonb, $16, $17"));
        assert!(!insert.contains("$13::text::jsonb"));
        assert!(!insert.contains("$16::text::jsonb"));
    }

    #[test]
    fn immutable_validation_preserves_the_exact_edit_origin_after_workspace_mutation() {
        let insert = insert_validated_flow_draft_sql();
        assert!(insert.contains("document.revision, document.edited_at"));
        assert!(insert.contains("RETURNING draft_edited_at"));

        let reload = select_validated_flow_draft_sql();
        assert!(reload.contains("draft_id, draft_revision, draft_edited_at"));
        assert!(!reload.contains("JOIN catalog.flow_drafts"));
        assert!(!reload.contains("FROM catalog.flow_drafts"));
    }

    #[test]
    fn rapid_regrant_advances_authority_time_past_grant_and_revocation() {
        let sql = grant_draft_safe_generation_sql();
        assert!(sql.contains("granted_at = GREATEST("));
        assert!(sql.contains("granted_at + interval '1 microsecond'"));
        assert!(sql.contains("revoked_at + interval '1 microsecond'"));
        assert!(sql.contains("SET revoked_at = NULL"));
    }

    #[test]
    fn report_query_is_read_only_and_tenant_scoped() {
        for sql in [
            select_authoring_suite_report_sql(),
            select_authoring_suite_case_facts_sql(),
            select_authoring_report_reservation_sql(),
        ] {
            assert!(sql.starts_with("SELECT"));
            assert!(sql.contains("tenant_id = $1"));
            assert!(sql.contains("report_id = $2"));
            assert!(!sql.contains("UPDATE"));
            assert!(!sql.contains("DELETE"));
        }
    }

    #[test]
    fn report_finalization_serializes_on_the_pending_reservation() {
        let lock = lock_authoring_report_reservation_state_sql();
        assert!(lock.contains("tenant_id = $1"));
        assert!(lock.contains("report_id = $2"));
        assert!(lock.ends_with("FOR UPDATE"));

        let finalization = finalize_authoring_report_reservation_sql();
        assert!(finalization.contains("state = 'pending'"));
        assert!(finalization.contains("state = 'finalized'"));
        assert!(insert_authoring_report_reservation_sql().contains("ON CONFLICT DO NOTHING"));
        assert!(insert_authoring_suite_case_fact_sql().contains("ON CONFLICT DO NOTHING"));
    }
}
