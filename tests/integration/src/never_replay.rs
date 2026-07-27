//! T-NR contract proof over the production transition builder.

#[cfg(test)]
pub mod tests {
    use wamn_run_state::attempt::{AttemptDispatchResult, AttemptStartResult};
    use wamn_run_state::transitions::{
        begin_attempt_sql, complete_attempt_error_sql, complete_attempt_success_sql,
        mark_attempt_dispatched_sql,
    };

    #[test]
    fn sent_but_unrecorded_never_replay_has_no_redispatch_path() {
        let sql = begin_attempt_sql();
        assert!(sql.contains("n.attempt_dispatched_at IS NULL THEN 'prepared'"));
        assert!(sql.contains("WHEN n.recovery_class = 'never-replay' THEN 'effect-uncertain'"));
        assert!(!AttemptStartResult::EffectUncertain.permits_dispatch());
    }

    #[test]
    fn committed_intent_before_send_is_resumable_and_send_is_single_marked() {
        let begin = begin_attempt_sql();
        assert!(begin.contains("THEN 'prepared'"));
        assert!(begin.contains("WHEN c.result_code = 'prepared'"));
        assert!(AttemptStartResult::Started.permits_dispatch());

        let mark = mark_attempt_dispatched_sql();
        assert!(mark.contains("SET attempt_dispatched_at = now()"));
        assert!(mark.contains("THEN 'already-dispatched'"));
        assert!(AttemptDispatchResult::Marked.permits_dispatch());
        assert!(!AttemptDispatchResult::AlreadyDispatched.permits_dispatch());
    }

    #[test]
    fn crash_before_intent_commit_leaves_no_protocol_mutation() {
        let sql = begin_attempt_sql();
        assert_eq!(sql.matches("INSERT INTO node_runs").count(), 1);
        assert_eq!(sql.matches("WITH input AS").count(), 1);
        assert!(
            sql.find("inserted AS").expect("intent CTE")
                < sql.find("renewed AS").expect("renew CTE")
        );
    }

    #[test]
    fn missing_key_and_absent_purity_controls_refuse_replay() {
        let sql = begin_attempt_sql();
        assert!(sql.contains("THEN 'missing-attempt-key'"));
        assert!(!AttemptStartResult::MissingAttemptKey.permits_dispatch());
        assert!(!AttemptStartResult::EffectUncertain.permits_dispatch());
    }

    #[test]
    fn completion_seams_update_the_intent_instead_of_inserting() {
        for sql in [complete_attempt_success_sql(), complete_attempt_error_sql()] {
            assert!(sql.contains("UPDATE node_runs AS n"));
            assert!(!sql.contains("INSERT INTO node_runs"));
            assert!(sql.contains("locked_attempt AS MATERIALIZED"));
        }
    }
}
