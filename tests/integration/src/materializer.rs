//! Callable-flow materializer boundary proofs (wamn-5wd1.50).

#[cfg(test)]
pub mod tests {
    use wamn_run_state::admission::{AdmissionTransition, RunStateSchema, admission_transaction};

    const SHELL: &str = include_str!("../../../components/execution/materializer/src/main.rs");
    const PURE_SQL: &str = include_str!("../../../crates/events/materializer/src/sql.rs");
    const INPUT: &str = include_str!("../../../crates/events/materializer/src/input.rs");

    #[test]
    fn effect_shell_uses_only_the_central_event_admission_transition() {
        assert!(SHELL.contains("AdmissionProducer::Event"));
        assert!(SHELL.contains("recipe.lock_head()"));
        assert!(SHELL.contains("recipe.admit()"));
        for forbidden in [
            "write_ahead_triggered_run_sql",
            "enqueue_evt_sql",
            "enqueue_evt_with_policy_sql",
            "INSERT INTO wamn_run.runs",
            "INSERT INTO wamn_run.run_queue",
        ] {
            assert!(
                !SHELL.contains(forbidden),
                "materializer bypasses admission through {forbidden}"
            );
        }
    }

    #[test]
    fn event_resolution_has_no_attachment_path() {
        assert!(PURE_SQL.contains("catalog.event_registrations"));
        assert!(PURE_SQL.contains("catalog.catalog_heads"));
        assert!(PURE_SQL.contains("catalog.release_flows"));
        assert!(
            !PURE_SQL.contains("active_attachments")
                && !PURE_SQL.contains("release_attachments")
                && !PURE_SQL.contains("attachment_activation")
        );
    }

    #[test]
    fn admission_scopes_dedup_and_records_registration_evidence() {
        let schema = RunStateSchema::default();
        let sql = admission_transaction(AdmissionTransition::CallableFlow { schema: &schema })
            .admit()
            .to_string();
        // The event identity is composed ONCE in `input` and read from there by
        // both the existence lookup and the run insert (wamn-0h0g.19.7). The
        // stream position remains the identity for an event carrying no
        // author-supplied dedup id — which is every event the CDC materializer
        // produces — so this proof's subject is unchanged; what moved is that a
        // ROUTER-emitted derived event can now present its own key instead.
        assert!(sql.contains("'evt:' || $18::text || ':'"));
        assert!(sql.contains("$19::bigint::text) END AS event_idempotency_key"));
        assert!(sql.contains("r.idempotency_key = i.event_idempotency_key"));
        assert!(sql.contains("er.registration <> i.registration_document"));
        assert!(sql.contains("'{registration-hash}'"));
        assert!(sql.contains("CASE WHEN c.producer = 'event' THEN c.registration_id END"));
        assert!(sql.contains("CASE WHEN c.producer = 'event' THEN c.event_seq ELSE 0 END"));
        assert!(sql.contains("CASE WHEN c.producer = 'event' THEN c.event_source_run_id END"));
        assert!(sql.contains("sl.root_run_id <> i.event_root_run_id"));
        assert!(sql.contains("sl.depth + 1 <> i.event_depth"));
        assert!(sql.contains("i.input_json ? 'causation'"));
        assert!(sql.contains("(tenant_id, run_id, available_at, stream_seq)"));
        assert!(sql.contains("$24"));
        assert!(!sql.contains("$25"));
        for retired in ["partition_key", "partition_policy", "existing_queue"] {
            assert!(!sql.contains(retired), "admission still contains {retired}");
            assert!(!SHELL.contains(retired), "guest still binds {retired}");
        }
        assert!(SHELL.contains("text(&plan.source_run_id)"));
        assert!(SHELL.contains("text(&plan.causation.root)"));
        assert!(SHELL.contains("env_or(\"WAMN_MAT_RUN_SCHEMA\", \"wamn_run\")"));
        assert!(SHELL.contains("AdmissionTransition::CallableFlow"));
        assert!(!SHELL.contains("let recipe = admission_sql();"));
        assert!(!SHELL.contains("UPDATE wamn_run.runs"));
    }

    #[test]
    fn business_input_golden_keeps_trusted_metadata_out() {
        assert!(INPUT.contains(r#"{"event":"insert","new":{"id":"7","qty":"12.3400"}}"#));
        for forbidden in [
            "\"trigger\"",
            "\"entity\"",
            "\"table\"",
            "\"seq\"",
            "\"causation\"",
        ] {
            assert!(
                INPUT.contains(forbidden),
                "the golden must explicitly guard the {forbidden} home"
            );
        }
        assert!(INPUT.contains("assert_ne!(input.get(\"old\"), Some(&Value::Null))"));
    }
}
