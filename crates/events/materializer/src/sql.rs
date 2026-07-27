//! The materializer guest's read statements — pure `String` builders in the
//! `wamn-run-state`/`wamn-api` discipline: identifiers are static literals,
//! every runtime value is a `$n` bind, and each carries the explicit
//! `tenant_id = current_setting('app.tenant', true)` predicate (R8b-b —
//! behaviorally inert under RLS, defense-in-depth on top of it).

/// The registration sweep: every stored registration document for the bound
/// tenant, with its denormalized identity columns alongside. The full
/// declaration is the jsonb document
/// (`wamn_event_reg::EventRegistration::from_json` is the decoder); the
/// columns are lookup denormalizations (catalog-schema.sql). No params.
pub fn select_registrations_sql() -> String {
    "SELECT registration_id, catalog_id, flow_id, registration::text AS registration \
       FROM catalog.event_registrations \
      WHERE tenant_id = current_setting('app.tenant', true) \
      ORDER BY catalog_id, registration_id"
        .to_string()
}

/// One subscribed flow pinned through the environment's applied catalog head.
///
/// Params: `$1` catalog id, `$2` environment, `$3` flow id. Returns 0 rows
/// when the head or release member is absent, so the caller holds the
/// registration. Event resolution deliberately has no attachment join.
pub fn select_release_flow_sql() -> String {
    "SELECT h.applied_catalog_version, r.flow_version, a.graph_json::text AS graph_json \
       FROM catalog.catalog_heads AS h \
       JOIN catalog.release_flows AS r \
         ON r.tenant_id = h.tenant_id AND r.catalog_id = h.catalog_id \
        AND r.catalog_version = h.applied_catalog_version \
       JOIN catalog.flow_artifacts AS a \
         ON a.tenant_id = r.tenant_id AND a.flow_id = r.flow_id \
        AND a.flow_version = r.flow_version \
      WHERE h.tenant_id = current_setting('app.tenant', true) \
        AND h.catalog_id = $1 AND h.environment = $2 AND r.flow_id = $3"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_tenant_scoped_and_parameterized() {
        let regs = select_registrations_sql();
        assert!(regs.contains("current_setting('app.tenant', true)"));
        assert!(regs.contains("catalog.event_registrations"));
        assert!(
            regs.contains("registration::text"),
            "the jsonb doc travels as text for the guest decoder"
        );

        let flow = select_release_flow_sql();
        assert!(flow.contains("current_setting('app.tenant', true)"));
        assert!(flow.contains("$1"));
        assert!(flow.contains("$2"));
        assert!(flow.contains("$3"));
        assert!(
            flow.contains("catalog.catalog_heads")
                && flow.contains("catalog.release_flows")
                && flow.contains("catalog.flow_artifacts"),
            "event candidates resolve the applied release"
        );
        assert!(
            !flow.contains("attachment"),
            "events never resolve attachments"
        );
    }
}
