//! Callable-flow materializer boundary proofs (wamn-5wd1.50).

#[cfg(test)]
pub mod tests {
    use wamn_run_state::admission::admission_sql;

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
        let sql = admission_sql().admit().to_string();
        assert!(sql.contains("'evt:' || i.registration_id || ':' || i.event_seq::text"));
        assert!(sql.contains("er.registration <> i.registration_document"));
        assert!(sql.contains("'{registration-hash}'"));
        assert!(sql.contains("CASE WHEN c.producer = 'event' THEN c.registration_id END"));
        assert!(sql.contains("CASE WHEN c.producer = 'event' THEN c.event_seq ELSE 0 END"));
        assert!(sql.contains("CASE WHEN c.producer = 'event' THEN c.event_source_run_id END"));
        assert!(sql.contains("sl.root_run_id <> i.event_root_run_id"));
        assert!(sql.contains("sl.depth + 1 <> i.event_depth"));
        assert!(sql.contains("i.input_json ? 'causation'"));
        assert!(SHELL.contains("text(&plan.source_run_id)"));
        assert!(SHELL.contains("text(&plan.causation.root)"));
        assert!(SHELL.contains("env_or(\"WAMN_MAT_RUN_SCHEMA\", \"wamn_run\")"));
        assert!(SHELL.contains("admission_sql_for_schema(&cfg.run_schema)"));
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
