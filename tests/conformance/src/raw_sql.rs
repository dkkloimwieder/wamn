//! Static conformance for the trusted RawSql grant boundary.

#[cfg(test)]
mod tests {
    const RUNNER: &str = include_str!("../../../components/execution/flowrunner/src/lib.rs");

    #[test]
    fn trusted_lookup_is_database_and_tenant_scoped() {
        assert!(RUNNER.contains("FROM app_system.configurations"));
        assert!(RUNNER.contains("tenant_id = NULLIF(current_setting('app.tenant', true), '')"));
        assert!(RUNNER.contains("AND config_key = $1"));
        assert!(RUNNER.contains("LIMIT 2"));
        assert!(RUNNER.contains("raw_sql_enabled_rows(&result.rows)"));
    }

    #[test]
    fn node_authored_values_cannot_forge_the_grant() {
        let context_start = RUNNER
            .find("let mut ctx = wamn_node_guest::caps::CapsCtx")
            .expect("production standard-node context");
        let context_end = RUNNER[context_start..]
            .find("let granted =")
            .map(|offset| context_start + offset)
            .expect("grant calculation follows context construction");
        let construction = &RUNNER[context_start..context_end];
        assert!(construction.contains("&& raw_sql_enabled()"));
        assert!(construction.contains("Capability::RawSql"));
        assert!(!construction.contains("d.config"));
        assert!(!construction.contains("d.payload"));
    }
}
