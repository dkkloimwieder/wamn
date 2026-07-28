//! Black-box contract guards for exact claimed-run execution.

#[cfg(test)]
mod tests {
    use wamn_run_state::queue::begin_claimed_run_sql;

    const FLOWRUNNER_WIT: &str =
        include_str!("../../../components/execution/flowrunner/wit/world.wit");

    #[test]
    fn exact_driver_is_versioned_by_shape_and_does_not_alias_run_next() {
        assert!(FLOWRUNNER_WIT.contains(
            "export execute-claimed: func(\n    run-id: string,\n    lease-owner: string,\n    lease-generation: s64,\n    lease-ttl-ms: u64,\n  ) -> result<u32, string>;"
        ));
        assert_eq!(FLOWRUNNER_WIT.matches("export execute-claimed:").count(), 1);
        assert_eq!(FLOWRUNNER_WIT.matches("export run-next:").count(), 1);
    }

    #[test]
    fn exact_driver_sql_has_one_locked_authority_and_no_available_scan() {
        let sql = begin_claimed_run_sql();
        assert!(sql.contains("FOR UPDATE OF q, r"));
        assert!(sql.contains("a.lease_owner IS DISTINCT FROM $2"));
        assert!(sql.contains("a.lease_generation <> $3"));
        assert!(sql.contains("a.lease_expires_at <= now()"));
        assert!(!sql.contains("available_at <="));
        assert!(!sql.contains("SKIP LOCKED"));
        assert!(!sql.contains("lease_generation + 1"));
    }
}
