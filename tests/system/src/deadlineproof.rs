//! Repository proof for callable-flow attempt deadline enforcement.

#[cfg(test)]
mod tests {
    use wamn_run_state::attempt::AttemptDispatchResult;
    use wamn_run_state::transitions::mark_attempt_dispatched_sql;

    #[test]
    fn final_send_boundary_has_typed_deadline_refusals() {
        let sql = mark_attempt_dispatched_sql();
        let attempt_check = sql
            .find("attempt_deadline_at <= now()")
            .expect("attempt deadline check");
        let dispatch_write = sql
            .find("SET attempt_dispatched_at = now()")
            .expect("dispatch marker write");
        assert!(attempt_check < dispatch_write);
        assert_eq!(
            AttemptDispatchResult::from_code("attempt-deadline-expired"),
            Some(AttemptDispatchResult::AttemptDeadlineExpired)
        );
        assert_eq!(
            AttemptDispatchResult::from_code("run-deadline-expired"),
            Some(AttemptDispatchResult::RunDeadlineExpired)
        );
        assert!(!AttemptDispatchResult::AttemptDeadlineExpired.permits_dispatch());
        assert!(!AttemptDispatchResult::RunDeadlineExpired.permits_dispatch());
    }

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
