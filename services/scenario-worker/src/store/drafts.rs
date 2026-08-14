//! Persistence statements for the internal flow-draft application path.
//!
//! Draft document and validated-artifact semantics remain owned by
//! `wamn-catalog`; this module owns only the typed application-store boundary.

/// Insert the first revision of one mutable flow draft.
///
/// Params: tenant, draft id, flow id, exact definition text.
///
/// The definition is stored as `text`, byte for byte. Save deliberately does
/// not parse it, so a half-finished edit is preserved rather than refused;
/// `validate` parses at its own stage and owns the typed refusal.
pub fn insert_flow_draft_sql() -> &'static str {
    "INSERT INTO catalog.flow_drafts \
       (tenant_id, draft_id, flow_id, revision, definition) \
     VALUES ($1, $2, $3, 1, $4) \
     ON CONFLICT (tenant_id, draft_id) DO NOTHING \
     RETURNING revision, edited_at"
}

/// Replace one mutable draft only at its caller-observed revision.
///
/// Params: tenant, draft id, flow id, expected revision, exact definition text.
pub fn update_flow_draft_sql() -> &'static str {
    "UPDATE catalog.flow_drafts \
        SET revision = revision + 1, definition = $5, \
            edited_at = GREATEST(clock_timestamp(), edited_at + interval '1 microsecond') \
      WHERE tenant_id = $1 AND draft_id = $2 AND flow_id = $3 AND revision = $4 \
      RETURNING revision, edited_at"
}

/// Read one exact draft revision for validation.
///
/// Returns the definition exactly as it was submitted; the caller parses.
///
/// The `graph_json` fallback serves rows saved before wamn-ftfc.2, whose exact
/// bytes were destroyed by the old `jsonb` cast and are unrecoverable. Such a
/// row reads back as its normalized document until its next save; every row
/// written since is byte-exact.
///
/// Params: tenant, draft id, revision.
pub fn select_flow_draft_sql() -> &'static str {
    "SELECT flow_id, COALESCE(definition, graph_json::text), edited_at \
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
/// That released member supplies its existing connection bindings but never
/// supplies the graph executed by the draft run.
///
/// Params: tenant, catalog id, environment, expected catalog version, flow id.
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
        AND member.flow_id = $5"
}

/// Resolve one callee name in the exact catalog version pinned by draft validation.
///
/// Params: tenant, catalog id, catalog version, callee flow id.
pub fn select_call_flow_callee_plan_sql() -> &'static str {
    "SELECT member.flow_version, member.execution_bundle_hash, bundle.exact_bytes \
       FROM catalog.release_flows AS member \
       JOIN catalog.execution_bundles AS bundle \
         ON bundle.tenant_id = member.tenant_id \
        AND bundle.execution_bundle_hash = member.execution_bundle_hash \
      WHERE member.tenant_id = $1 AND member.catalog_id = $2 \
        AND member.catalog_version = $3 AND member.flow_id = $4"
}

/// Persist one immutable validation of a content-addressed draft.
///
/// `graph_json` here is the canonical re-serialization of the parsed flow, which
/// is what an immutable validated artifact stores. The drift guard is separate
/// and byte-exact: `$15` is the definition text the validator actually read, so
/// a concurrent edit between read and insert cannot be validated away.
///
/// Params: tenant, draft id, revision, draft-content hash, catalog id/version,
/// environment, runtime flow version, graph JSON/hash, artifact hash,
/// execution-bundle hash, immutable binding-base artifact hash,
/// validated-draft identity hash, exact source definition text.
pub fn insert_validated_flow_draft_sql() -> &'static str {
    "INSERT INTO catalog.validated_flow_drafts \
       (tenant_id, draft_id, draft_revision, draft_edited_at, draft_content_hash, catalog_id, \
        catalog_version, environment, flow_id, runtime_flow_version, graph_json, \
        graph_hash, draft_artifact_hash, execution_bundle_hash, binding_base_artifact_hash, \
        validated_draft_hash) \
     SELECT document.tenant_id, document.draft_id, document.revision, document.edited_at, \
            $4, $5, $6, $7, \
            document.flow_id, $8, $9::text::jsonb, $10, $11, $12, $13, $14 \
       FROM catalog.flow_drafts AS document \
      WHERE document.tenant_id = $1 AND document.draft_id = $2 \
        AND document.revision = $3 \
        AND COALESCE(document.definition, document.graph_json::text) = $15 \
     ON CONFLICT (tenant_id, draft_id, draft_revision, draft_content_hash, \
                  catalog_id, catalog_version, \
                  environment, runtime_flow_version, draft_artifact_hash, \
                  execution_bundle_hash, binding_base_artifact_hash) DO NOTHING \
     RETURNING draft_edited_at"
}

/// Read one exact immutable draft pin before admission.
///
/// Params: tenant, draft id/revision, draft-content hash, catalog id/version,
/// environment, runtime version, ordinary artifact hash, bundle hash, binding-base artifact hash,
/// validated-draft identity hash.
pub fn select_validated_flow_draft_sql() -> &'static str {
    "SELECT draft.draft_id, draft.draft_revision, draft.draft_edited_at, draft.environment, \
            draft.flow_id, draft.runtime_flow_version, draft.graph_json::text, draft.graph_hash, \
            draft.draft_artifact_hash, bundle.exact_bytes, draft.binding_base_artifact_hash, \
            draft.validated_at \
       FROM catalog.validated_flow_drafts AS draft \
       JOIN catalog.execution_bundles AS bundle \
         ON bundle.tenant_id = draft.tenant_id \
        AND bundle.execution_bundle_hash = draft.execution_bundle_hash \
      WHERE draft.tenant_id = $1 AND draft.draft_id = $2 AND draft.draft_revision = $3 \
        AND draft.draft_content_hash = $4 AND draft.catalog_id = $5 \
        AND draft.catalog_version = $6 AND draft.environment = $7 \
        AND draft.runtime_flow_version = $8 \
        AND draft.draft_artifact_hash = $9 AND draft.execution_bundle_hash = $10 \
        AND draft.binding_base_artifact_hash = $11 AND draft.validated_draft_hash = $12"
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
        // The mutable draft is WRITTEN as exact text: neither write may cast
        // it, because casting is parsing and save does not parse.
        for statement in [insert_flow_draft_sql(), update_flow_draft_sql()] {
            assert!(statement.contains("definition"), "{statement}");
            assert!(!statement.contains("graph_json"), "{statement}");
            assert!(!statement.contains("jsonb"), "{statement}");
        }
        // The read prefers exact text and falls back only for pre-wamn-ftfc.2
        // rows, which have no exact text to return.
        let select = select_flow_draft_sql();
        assert!(
            select.contains("COALESCE(definition, graph_json::text)"),
            "{select}"
        );
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
            "runtime_flow_version",
            "execution_bundle_hash",
            "binding_base_artifact_hash",
            "validated_draft_hash",
        ] {
            assert!(insert.contains(field), "validated insert omits {field}");
        }
        assert!(!insert.contains("release_manifests"));
        assert!(!insert.contains("INSERT INTO catalog.flow_artifacts"));
        assert!(insert.contains("COALESCE(document.definition, document.graph_json::text) = $15"));
        assert!(insert.contains("document.flow_id, $8, $9::text::jsonb"));
        assert!(
            insert.contains("ON CONFLICT (tenant_id, draft_id, draft_revision, draft_content_hash")
        );

        let select = select_validated_flow_draft_sql();
        for predicate in [
            "draft_id = $2",
            "draft_revision = $3",
            "draft_content_hash = $4",
            "environment = $7",
            "runtime_flow_version = $8",
            "draft_artifact_hash = $9",
            "execution_bundle_hash = $10",
            "binding_base_artifact_hash = $11",
            "validated_draft_hash = $12",
        ] {
            assert!(
                select.contains(predicate),
                "validated lookup omits {predicate}"
            );
        }
    }

    #[test]
    fn call_flow_callee_resolution_reads_the_exact_pinned_release_member() {
        let select = select_call_flow_callee_plan_sql();
        for predicate in [
            "member.tenant_id = $1",
            "member.catalog_id = $2",
            "member.catalog_version = $3",
            "member.flow_id = $4",
        ] {
            assert!(
                select.contains(predicate),
                "callee lookup omits {predicate}"
            );
        }
        assert!(select.contains("JOIN catalog.execution_bundles AS bundle"));
        assert!(select.contains("bundle.exact_bytes"));
        assert!(!select.contains("catalog_heads"));
        assert!(!select.contains("environment"));
    }

    #[test]
    fn immutable_validation_preserves_the_exact_edit_origin_after_workspace_mutation() {
        let insert = insert_validated_flow_draft_sql();
        assert!(insert.contains("document.revision, document.edited_at"));
        assert!(insert.contains("RETURNING draft_edited_at"));

        let reload = select_validated_flow_draft_sql();
        assert!(reload.contains("draft.draft_id, draft.draft_revision, draft.draft_edited_at"));
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
}
