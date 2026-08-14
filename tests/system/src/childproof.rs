//! Repository proof for bounded, authorized `invoke-flow` execution.

#[cfg(test)]
pub mod tests {
    use serde_json::json;
    use wamn_run_state::child::{
        ChildCreateResult, create_or_recover_child_sql, release_child_sql,
    };
    use wamn_run_state::transitions::StoredCallerOutcome;

    #[test]
    fn creation_authorizes_but_occurrence_recovery_does_not() {
        let sql = create_or_recover_child_sql();
        let recovery = sql
            .find("WHEN c.run_id IS NOT NULL THEN 'ready'")
            .expect("occurrence recovery branch");
        let activation = sql
            .find("d.activation_enabled IS DISTINCT FROM true")
            .expect("creation activation check");
        let policy = sql
            .find("NOT (d.caller_policy->'allowed-callers' ? p.flow_id)")
            .expect("creation caller policy");
        assert!(recovery < activation && recovery < policy);
        assert!(sql.contains("c.catalog_version IS DISTINCT FROM p.catalog_version"));
        assert!(sql.contains("c.input_json IS DISTINCT FROM $11::text::jsonb"));

        assert_eq!(
            ChildCreateResult::from_parts(
                "released",
                "running",
                Some("child-1".into()),
                None,
                Some("responded".into()),
                Some(json!({"recommendation": "approve"})),
                Some(200),
                Some("respond".into()),
                Some("sha256:out".into()),
            ),
            Some(ChildCreateResult::Released {
                child_run_id: "child-1".into(),
                outcome: StoredCallerOutcome {
                    kind: "responded".into(),
                    body: json!({"recommendation": "approve"}),
                    http_status: Some(200),
                    release_node_id: Some("respond".into()),
                    hash: Some("sha256:out".into()),
                },
            })
        );
    }

    #[test]
    fn service_actor_is_trusted_fresh_and_bounded() {
        let sql = create_or_recover_child_sql();
        assert!(sql.contains(
            "'subject', 'service:' || c.catalog_id || ':' || c.environment || ':' || $9::text"
        ));
        assert!(sql.contains("'caller', jsonb_build_object"));
        // The caller's own actor is demoted to lineage under `caller`, never
        // promoted to the child's actor. 50169c4 nested the untrusted half of
        // the context under `source`, so the read is `->'source'->'actor'` —
        // pin the whole COALESCE so repointing or dropping it fails here.
        assert!(
            sql.contains("'actor', COALESCE(c.caller_context->'source'->'actor', 'null'::jsonb)")
        );
        assert!(sql.contains("$11::text::jsonb,"));
        assert!(sql.contains("invocation_context"));
        assert!(!sql.contains("$11::text::jsonb ||"));
        assert!(sql.contains("p.invoke_depth + 1 > $13::int"));
        assert!(sql.contains("fs.child_count >= $14::bigint"));
        assert!(sql.contains("LEAST("));
        assert!(sql.contains("c.parent_response_deadline"));
        assert!(sql.contains("c.run_deadline_ms * interval '1 millisecond'"));
    }

    #[test]
    fn release_wakes_atomically() {
        let release = release_child_sql();
        assert!(release.contains("released AS"));
        assert!(release.contains("cleared_parent AS"));
        assert!(release.contains("woken_parent AS"));
    }
}
