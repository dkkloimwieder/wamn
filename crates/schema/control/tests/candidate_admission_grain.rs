use std::collections::{BTreeMap, BTreeSet};

use wamn_schema_control::{
    BareSchemaName, RunPlaneActionKind, RunPlaneObservation, plan_run_plane,
};

#[test]
fn existing_runs_gain_the_complete_candidate_grain_without_backfill() {
    let mut observation = RunPlaneObservation {
        catalog_schema_present: true,
        tables: BTreeMap::from([(
            "runs".to_string(),
            BTreeSet::from([
                "tenant_id".to_string(),
                "run_id".to_string(),
                "flow_id".to_string(),
                "flow_version".to_string(),
                "wiring_id".to_string(),
                "wiring_version".to_string(),
            ]),
        )]),
        ..RunPlaneObservation::default()
    };
    for (column, ty) in [
        ("flow_id", "text"),
        ("flow_version", "integer"),
        ("wiring_id", "text"),
        ("wiring_version", "integer"),
    ] {
        let key = ("runs".to_string(), column.to_string());
        observation.column_types.insert(key.clone(), ty.to_string());
        observation.non_nullable_columns.insert(key);
    }

    let schema = BareSchemaName::new("project_run").unwrap();
    let plan = plan_run_plane(&schema, &observation);
    let action = plan
        .actions
        .iter()
        .find(|action| action.target == "runs.wiring-identity")
        .expect("candidate-grain cutover is planned");

    assert_eq!(action.kind, RunPlaneActionKind::AddColumn);
    for column in ["wiring_hash text", "binding_world_json jsonb"] {
        assert!(
            action
                .sql
                .contains(&format!("ADD COLUMN IF NOT EXISTS {column}"))
        );
    }
    for column in [
        "flow_id",
        "flow_version",
        "wiring_id",
        "wiring_version",
        "wiring_hash",
        "binding_world_json",
    ] {
        assert!(
            action
                .sql
                .contains(&format!("ALTER COLUMN {column} DROP NOT NULL")),
            "{column} is not honestly nullable during the legacy drain"
        );
    }
    assert!(
        action
            .sql
            .contains("ADD CONSTRAINT runs_execution_grain_check")
    );
    assert!(
        action
            .sql
            .contains("flow_id IS NOT NULL AND flow_version IS NOT NULL")
    );
    assert!(
        action
            .sql
            .contains("AND flow_id <> '' AND flow_version > 0")
    );
    assert!(
        action
            .sql
            .contains("flow_id IS NULL AND flow_version IS NULL")
    );
    assert!(
        action
            .sql
            .contains("AND wiring_id IS NOT NULL AND wiring_version IS NOT NULL")
    );
    assert!(!action.sql.contains("UPDATE project_run.runs"));
    assert!(!action.sql.contains("connection_generation_retention"));
}
