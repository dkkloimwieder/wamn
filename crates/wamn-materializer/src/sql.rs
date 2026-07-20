//! The materializer guest's read statements — pure `String` builders in the
//! `wamn-run-queue`/`wamn-api` discipline: identifiers are static literals,
//! every runtime value is a `$n` bind, and each carries the explicit
//! `tenant_id = current_setting('app.tenant', true)` predicate (R8b-b —
//! behaviorally inert under RLS, defense-in-depth on top of it).

/// The registration sweep: every stored registration document for the bound
/// tenant, with the denormalized flow id alongside (the flows-registry read
/// key). The full declaration is the jsonb document
/// (`wamn_event_reg::EventRegistration::from_json` is the decoder); the
/// columns are lookup denormalizations (catalog-schema.sql). No params.
pub fn select_registrations_sql() -> String {
    "SELECT registration_id, flow_id, registration::text AS registration \
       FROM catalog.event_registrations \
      WHERE tenant_id = current_setting('app.tenant', true) \
      ORDER BY catalog_id, registration_id"
        .to_string()
}

/// One subscribed flow's ACTIVE graph — the ordering/policy declaration source
/// (`wamn_flow::Flow::from_json`). Unqualified `flows` resolves through the
/// host-injected `search_path`, exactly as the flowrunner and dispatcher read
/// it. Params: `$1` flow_id. Returns 0 rows when the flow is missing/inactive
/// — the caller HOLDS the registration (delayed, never lost).
pub fn select_active_flow_sql() -> String {
    "SELECT version, graph_json::text AS graph_json FROM flows \
      WHERE tenant_id = current_setting('app.tenant', true) AND flow_id = $1 AND active"
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

        let flow = select_active_flow_sql();
        assert!(flow.contains("current_setting('app.tenant', true)"));
        assert!(flow.contains("$1"));
        assert!(flow.contains("AND active"));
        assert!(
            flow.contains("FROM flows "),
            "unqualified — resolves via the host-injected search_path"
        );
    }
}
