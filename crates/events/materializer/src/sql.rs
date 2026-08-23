//! The materializer guest's tenant-scoped registration sweep.

/// Read registration documents and durable identities; no flow or plan lookup remains.
pub fn select_registrations_sql() -> String {
    "SELECT registration_id, catalog_id, registration::text AS registration \
       FROM catalog.event_registrations \
      WHERE tenant_id = current_setting('app.tenant', true) \
      ORDER BY catalog_id, registration_id"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_is_tenant_scoped_and_has_no_execution_tail() {
        let sql = select_registrations_sql();
        assert!(sql.contains("current_setting('app.tenant', true)"));
        assert!(sql.contains("catalog.event_registrations"));
        for retired in [
            "flow_id",
            "release_flows",
            "flow_artifacts",
            "execution_bundles",
            "execution_bundle_hash",
            "exact_bytes",
        ] {
            assert!(
                !sql.contains(retired),
                "retired stream lookup survived: {retired}"
            );
        }
    }
}
