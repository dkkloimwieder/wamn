//! Persistence statements for the internal flow-draft application path.
//!
//! This module owns the typed mutable-document store boundary.

/// Insert the first revision of one mutable flow draft.
///
/// Params: tenant, draft id, flow id, exact definition text.
///
/// The definition is stored as `text`, byte for byte. Save deliberately does
/// not parse it, so a half-finished edit is preserved rather than refused.
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

/// Read one exact draft revision.
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
}
