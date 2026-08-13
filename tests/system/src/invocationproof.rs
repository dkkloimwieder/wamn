//! Black-box guards for run admission and the single-shot runner surface.

#[cfg(test)]
mod tests {
    use wamn_run_state::queue::begin_claimed_run_sql;

    const FLOWRUNNER_WIT: &str =
        include_str!("../../../components/execution/flowrunner/wit/world.wit");

    #[test]
    fn flowrunner_is_versioned_at_zero_one_and_exports_run_alone() {
        let code = FLOWRUNNER_WIT
            .lines()
            .filter(|line| !line.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(code.contains("package wamn:flowrunner@0.1.0;"));
        assert!(
            code.contains(
                "export run: func(run-id: string, payload: string) -> result<u32, string>;"
            )
        );
        assert_eq!(code.matches("export ").count(), 1);
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
