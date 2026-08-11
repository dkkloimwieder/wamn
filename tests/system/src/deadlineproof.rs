//! Repository proof for bounded host effect waits.

#[cfg(test)]
mod tests {
    #[test]
    fn interrupted_store_is_disposed_before_any_follow_on_call() {
        let host = include_str!("../../../crates/execution/host/src/lib.rs");
        assert!(host.contains("self.live.take();"));
        assert!(host.contains("execution instance disposed"));
        assert!(host.contains("live.store.set_epoch_deadline(deadline_ticks(self.ttl_ms));"));

        let executor = include_str!("../../../services/executor/src/lib.rs");
        assert!(executor.contains("if executor.is_disposed()"));
        assert!(executor.contains("execution instance was interrupted and disposed"));
    }

    #[test]
    fn host_http_and_database_waits_share_a_finite_ceiling() {
        let host = include_str!("../../../crates/execution/host/src/lib.rs");
        assert!(host.contains("bounded_outgoing_config(config)"));
        assert!(host.contains(".min(MAX_HOST_CALL_DURATION)"));

        let postgres =
            include_str!("../../../crates/platform/runtime/src/plugins/wamn_postgres/pool.rs");
        assert!(postgres.contains("bounded_wait_timeout_ms"));
        assert!(postgres.contains("bounded_statement_timeout_ms"));
    }
}
