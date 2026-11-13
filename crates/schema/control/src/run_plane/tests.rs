use super::*;

fn schema(value: &str) -> BareSchemaName {
    BareSchemaName::new(value).expect("test schema is valid")
}

#[test]
fn bare_schema_refuses_unusable_postgresql_names() {
    assert!(BareSchemaName::new(format!("s{}", "a".repeat(62))).is_ok());
    for value in [
        String::new(),
        "safe\0suffix".to_owned(),
        format!("s{}", "a".repeat(63)),
        format!("s{}", "é".repeat(32)),
        format!("s{}", "é".repeat(31)),
    ] {
        assert!(BareSchemaName::new(value).is_err());
    }
}

#[test]
fn overlong_schema_inputs_cannot_alias_after_postgresql_truncation() {
    let first = format!("s{}", "a".repeat(63));
    let second = format!("s{}b", "a".repeat(62));
    assert!(BareSchemaName::new(first).is_err());
    assert!(BareSchemaName::new(second).is_err());
}

#[test]
fn bare_schema_rejects_syntax_that_the_unquoted_rewrite_cannot_represent() {
    for value in ["1bad", "Upper", "has-hyphen", "a b", "drop;schema"] {
        assert!(
            BareSchemaName::new(value).is_err(),
            "{value:?} must be rejected"
        );
    }
}

/// Build the observation the record itself describes: every record table
/// with its record columns, every record index with the record statement as
/// its live definition, the catalog schema complete, nothing legacy.
fn observation_at_record() -> RunPlaneObservation {
    let mut obs = RunPlaneObservation {
        catalog_schema_present: true,
        environment_policy_row_security: Some(environment_policy_row_security_at_record()),
        ..Default::default()
    };
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            let cols = record_columns(file, "wamn_run", &table)
                .into_iter()
                .map(|(c, _)| c)
                .collect();
            obs.tables.insert(table.clone(), cols);
        }
        for (name, _, stmt) in index_statements(file, "wamn_run") {
            obs.indexes.insert(name, stmt);
        }
    }
    for table in record_tables(CATALOG_SCHEMA_SQL, "catalog") {
        obs.catalog_tables.insert(table.clone());
    }
    for spec in AUTHORING_PRIVILEGE_SPECS {
        let schema_name = match spec.schema {
            AuthoringTableSchema::Catalog => "catalog",
            AuthoringTableSchema::RunPlane => "demo",
        };
        obs.authoring_table_owners.insert(
            (schema_name.to_string(), spec.table.to_string()),
            "platform_admin".to_string(),
        );
        {
            let (grantee, privileges) = ("wamn_app", spec.app);
            if !privileges.is_empty() {
                let key = (
                    schema_name.to_string(),
                    spec.table.to_string(),
                    grantee.to_string(),
                );
                let expected: BTreeSet<String> = privileges
                    .iter()
                    .map(|privilege| (*privilege).to_string())
                    .collect();
                obs.authoring_table_privileges
                    .insert(key.clone(), expected.clone());
                obs.authoring_effective_table_privileges
                    .insert(key.clone(), expected.clone());
                let expected_columns: BTreeSet<String> = expected
                    .into_iter()
                    .filter(|privilege| {
                        ["SELECT", "INSERT", "UPDATE", "REFERENCES"].contains(&privilege.as_str())
                    })
                    .collect();
                if !expected_columns.is_empty() {
                    obs.authoring_effective_column_privileges
                        .insert(key, expected_columns);
                }
            }
        }
    }
    obs.app_run_capture_privileges = (false, false, true);
    for spec in CHECK_SPECS {
        obs.checks.insert(
            (spec.table.to_string(), spec.name.to_string()),
            spec.definition.to_string(),
        );
    }
    for spec in helper_specs() {
        obs.helper_functions
            .insert(spec.name.to_string(), spec.definition.into_owned());
    }
    for spec in trigger_specs() {
        obs.triggers
            .insert((spec.table, spec.name), spec.definition);
    }
    for (column, ty) in [("package_id", "text"), ("environment", "text")] {
        let key = ("runs".to_string(), column.to_string());
        obs.non_nullable_columns.insert(key.clone());
        obs.column_types.insert(key, ty.to_string());
    }
    obs.triggers.insert(
        (
            "runs".to_string(),
            "runs_admission_pins_immutable".to_string(),
        ),
        RUNS_ADMISSION_PINS_TRIGGER_DEF.to_string(),
    );
    obs
}

/// The multi-line `runs.status` CHECK parses whole (paren-depth), and
/// `fail_type` — the fqg.16 sibling — is present as a column.
#[test]
fn multi_line_column_definitions_parse_whole() {
    let cols = record_columns(RUN_STATE_SQL, "wamn_run", "runs");
    let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
    assert!(names.contains(&"status"));
    assert!(names.contains(&"fail_type"));
    assert!(
        !names.contains(&"'infrastructure-failure',"),
        "continuation line misparsed"
    );
    let status = &cols.iter().find(|(c, _)| c == "status").unwrap().1;
    assert!(status.contains("'infrastructure-failure'"), "{status}");
    assert!(
        status.ends_with("))"),
        "CHECK closes inside the definition: {status}"
    );
}

#[test]
fn runs_failure_and_outcome_check_mirrors_are_exact_and_frozen() {
    let expected_fail_type = "CHECK (fail_type = ANY (ARRAY['terminal'::text, 'retry-exhausted'::text, 'invalid-input'::text, 'runaway-budget'::text, 'depth-budget'::text, 'dispatch-budget'::text, 'unresolvable-name'::text, 'hash-invalid-bytes'::text, 'foreign-revision'::text, 'incompatible-contract'::text, 'unbound-requirement'::text]))";
    let fail_type = CHECK_SPECS
        .iter()
        .find(|spec| spec.table == "runs" && spec.name == "runs_fail_type_check")
        .expect("runs.fail_type CHECK mirror exists");
    assert_eq!(fail_type.definition, expected_fail_type);

    let status = CHECK_SPECS
        .iter()
        .find(|spec| spec.table == "runs" && spec.name == "runs_status_check")
        .expect("runs.status CHECK mirror exists");
    assert_eq!(
        status.definition,
        "CHECK (status = ANY (ARRAY['dispatched'::text, 'running'::text, 'completed'::text, 'failed'::text, 'infrastructure-failure'::text]))"
    );

    let caller_outcome = CHECK_SPECS
        .iter()
        .find(|spec| spec.table == "runs" && spec.name == "runs_caller_outcome_type_check")
        .expect("runs.caller_outcome_type CHECK mirror exists");
    assert_eq!(
        caller_outcome.definition,
        "CHECK (caller_outcome_type = ANY (ARRAY['responded'::text, 'failed'::text]))"
    );
}

/// THE load-bearing self-consistency invariant: an observation derived from
/// the record itself plans NOTHING. Whatever the record files evolve into,
/// a schema at record is a no-op — this is what makes the verb idempotent
/// at target by construction.
#[test]
fn observation_at_record_plans_a_noop() {
    let plan = plan_run_plane(&schema("demo"), &observation_at_record());
    assert!(plan.is_noop(), "actions: {:#?}", plan.actions);
    assert!(plan.extra_columns.is_empty());
    assert_eq!(
        plan.at_target.len(),
        3,
        "all retained run-plane tables are at target"
    );
}

/// Old and new names of the renamed columns or checks of one table.
/// A current column still requires its declared CHECK constraint.
///
/// A bare `ADD COLUMN` leaves exactly this observation behind — carrier
/// present, its inline CHECK absent — because the exact-CHECK pass skips an
/// inline spec while the OBSERVATION lacks the column. The pass that follows
/// is therefore NOT a no-op, which is the predicate wamn-0h0g.20.9 exists to
/// restore.
#[test]
fn a_pin_carrier_added_without_its_inline_check_does_not_converge() {
    let (column, check) = ("capture_mode", "runs_capture_mode_check");
    let mut obs = observation_at_record();
    obs.checks.remove(&("runs".to_string(), check.to_string()));
    assert!(
        obs.tables
            .get("runs")
            .is_some_and(|columns| columns.contains(column)),
        "the bare-ADD state has the carrier without its CHECK"
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    assert!(
        plan.actions.iter().any(|action| {
            action.type_ == RunPlaneActionType::RepairConstraint
                && action.target == format!("runs.{check}")
        }),
        "a bare {column} costs a second reconcile turn: {:#?}",
        plan.actions
    );
}

#[test]
fn effective_indirect_or_owner_authority_never_plans_false_clean() {
    // The project-authoring relations this once drifted (`flow_drafts`,
    // `draft_safe_connection_grants`) left
    // `AUTHORING_PRIVILEGE_SPECS` with wamn-0h0g.9.11.2 (38860fab). Each
    // drift SHAPE — ownership, effective table privilege, effective column
    // privilege — is re-pinned on a relation the reconciler still owns.
    let mut obs = observation_at_record();
    obs.authoring_table_owners.insert(
        ("catalog".to_string(), "packages".to_string()),
        "wamn_app".to_string(),
    );
    obs.authoring_effective_table_privileges
        .entry((
            "catalog".to_string(),
            "connection_binding".to_string(),
            "wamn_app".to_string(),
        ))
        .or_default()
        .insert("INSERT".to_string());
    obs.authoring_effective_table_privileges
        .entry((
            "catalog".to_string(),
            "releases".to_string(),
            "wamn_app".to_string(),
        ))
        .or_default()
        .insert("UPDATE".to_string());
    obs.authoring_effective_column_privileges
        .entry((
            "catalog".to_string(),
            "connection_requirement".to_string(),
            "wamn_app".to_string(),
        ))
        .or_default()
        .insert("UPDATE".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    for table in [
        "packages",
        "connection_binding",
        "releases",
        "connection_requirement",
    ] {
        let repair = plan
            .actions
            .iter()
            .find(|action| {
                action.type_ == RunPlaneActionType::RepairAuthoringPrivilege
                    && action.target == format!("catalog.{table}")
            })
            .expect("effective privilege drift is surfaced as an action");
        assert!(repair.sql.contains("has_table_privilege"));
        assert!(repair.sql.contains("has_any_column_privilege"));
        assert!(repair.sql.contains("relation.relowner"));
        assert!(
            repair
                .sql
                .contains("authoring-effective-privilege-out-of-bounds")
        );
    }
}

fn environment_policy_row_security_repair(obs: &RunPlaneObservation) -> RunPlaneAction {
    let plan = plan_run_plane(&schema("demo"), obs);
    plan.actions
        .into_iter()
        .find(|action| action.type_ == RunPlaneActionType::RepairRowSecurity)
        .expect("environment-policy row-security drift must be repaired")
}

#[test]
fn exact_environment_policy_row_security_plans_no_repair() {
    let plan = plan_run_plane(&schema("demo"), &observation_at_record());
    assert!(
        plan.actions
            .iter()
            .all(|action| action.type_ != RunPlaneActionType::RepairRowSecurity)
    );
}

#[test]
fn disabled_environment_policy_rls_mutant_plans_repair() {
    let mut obs = observation_at_record();
    obs.environment_policy_row_security
        .as_mut()
        .expect("record relation")
        .enabled = false;

    let repair = environment_policy_row_security_repair(&obs);
    assert!(repair.sql.contains("ENABLE ROW LEVEL SECURITY"));
}

#[test]
fn unforced_environment_policy_rls_mutant_plans_repair() {
    let mut obs = observation_at_record();
    obs.environment_policy_row_security
        .as_mut()
        .expect("record relation")
        .forced = false;

    let repair = environment_policy_row_security_repair(&obs);
    assert!(repair.sql.contains("FORCE ROW LEVEL SECURITY"));
}

#[test]
fn missing_environment_policy_project_env_key_policy_mutant_plans_repair() {
    let mut obs = observation_at_record();
    obs.environment_policy_row_security
        .as_mut()
        .expect("record relation")
        .policies
        .clear();

    let repair = environment_policy_row_security_repair(&obs);
    assert!(
        repair
            .sql
            .contains("CREATE POLICY environment_policies_project_env_key")
    );
}

#[test]
fn widened_environment_policy_project_env_key_policy_mutant_plans_exact_replacement() {
    let mut obs = observation_at_record();
    let row_security = obs
        .environment_policy_row_security
        .as_mut()
        .expect("record relation");
    let policy = row_security
        .policies
        .get_mut("environment_policies_project_env_key")
        .expect("record policy");
    policy.command = "all".to_string();
    policy.roles.insert("PUBLIC".to_string());
    policy.using_expression = Some("true".to_string());
    policy.check_expression = Some("true".to_string());
    row_security.policies.insert(
        "environment_policies_extra".to_string(),
        RowPolicyObservation {
            command: "select".to_string(),
            permissive: true,
            roles: BTreeSet::from(["PUBLIC".to_string()]),
            using_expression: Some("true".to_string()),
            check_expression: None,
        },
    );

    let repair = environment_policy_row_security_repair(&obs);
    assert!(repair.sql.contains("SELECT policy.polname"));
    assert!(repair.sql.contains("DROP POLICY %I"));
    assert!(repair.sql.contains("FOR SELECT TO wamn_app USING"));
    // wamn-0h0g.22.17: the repair drops EVERY policy on the relation, so it
    // must recreate BOTH arms. Recreating only the minimum would silently
    // revert the platform admission on every reconcile-run-plane.
    assert!(
        repair
            .sql
            .contains("CREATE POLICY environment_policies_platform")
    );
    assert!(
        repair
            .sql
            .contains("FOR SELECT TO wamn_platform USING (true)")
    );
    assert!(!repair.sql.contains("WITH CHECK"));
}

/// Column-level authority ALONE — no direct grant, no table-level effective
/// privilege, no ownership — must still surface a repair. `demo.authoring_test_reports`
/// carried this test until wamn-0h0g.9.11.2 (38860fab) removed it from
/// `AUTHORING_PRIVILEGE_SPECS`; `environment_policies` is the surviving
/// run-plane relation with a guest spec to hang it on.
#[test]
fn guest_column_authority_alone_never_plans_false_clean() {
    let mut obs = observation_at_record();
    obs.authoring_effective_column_privileges
        .entry((
            "demo".to_string(),
            "environment_policies".to_string(),
            "wamn_app".to_string(),
        ))
        .or_default()
        .insert("UPDATE".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    let repair = plan
        .actions
        .iter()
        .find(|action| {
            action.type_ == RunPlaneActionType::RepairAuthoringPrivilege
                && action.target == "demo.environment_policies"
        })
        .expect("column-level guest write authority must be surfaced");
    assert!(repair.sql.contains("DO $effective_acl$"));
    assert!(repair.sql.contains("pg_catalog.has_any_column_privilege"));
    assert!(
        repair
            .sql
            .contains("'wamn_app', '\"demo\".\"environment_policies\"', 'UPDATE'")
    );
    // Column authority alone must not trigger the direct-grant REVOKE arm.
    assert!(
        !repair.sql.contains("REVOKE ALL PRIVILEGES"),
        "{}",
        repair.sql
    );
    assert!(
        repair
            .sql
            .contains("authoring-effective-privilege-out-of-bounds")
    );
}

/// From zero (an empty database): the full run-plane set in FK order behind
/// the schema ensure, plus the whole catalog schema — the fixture-wipe
/// restore path (manifestations 3 + 5).
#[test]
fn from_zero_plans_the_full_set_in_order() {
    let obs = RunPlaneObservation::default();
    let plan = plan_run_plane(&schema("wamn_runner_demo"), &obs);
    let types: Vec<RunPlaneActionType> = plan.actions.iter().map(|a| a.type_).collect();
    assert_eq!(types[0], RunPlaneActionType::EnsureSchema);
    assert!(
        plan.actions
            .iter()
            .any(|a| a.type_ == RunPlaneActionType::EnsureCatalogSchema)
    );
    let pin_helper = plan
        .actions
        .iter()
        .position(|action| {
            action.type_ == RunPlaneActionType::RepairHelperFunction
                && action.target == "guard_run_admission_pins_immutable"
        })
        .expect("run admission-pin helper is provisioned");
    let runs_table = plan
        .actions
        .iter()
        .position(|action| {
            action.type_ == RunPlaneActionType::CreateTable && action.target == "runs"
        })
        .expect("runs table is provisioned");
    assert!(
        pin_helper < runs_table,
        "the admission-pin helper must exist before the runs trigger"
    );
    // No column/index repairs on tables being created (sections carry them).
    assert!(!types.contains(&RunPlaneActionType::AddColumn));
    assert!(!types.contains(&RunPlaneActionType::CreateIndex));
    // The rewrite reached the sections.
    let rq = plan
        .actions
        .iter()
        .find(|a| a.target == "run_queue")
        .unwrap();
    assert!(rq.sql.contains("CREATE TABLE wamn_runner_demo.run_queue"));
    assert!(!rq.sql.contains("wamn_run."));
}

/// An unknown live column is SURFACED, never dropped.
#[test]
fn extra_live_columns_are_surfaced_not_dropped() {
    let mut obs = observation_at_record();
    obs.tables
        .get_mut("run_queue")
        .unwrap()
        .insert("legacy_x".into());
    let plan = plan_run_plane(&schema("demo"), &obs);
    assert_eq!(
        plan.extra_columns,
        [("run_queue".to_string(), "legacy_x".to_string())]
    );
    assert!(plan.is_noop(), "extras plan no action: {:#?}", plan.actions);
}

#[test]
fn broad_app_run_grants_are_removed_without_regranting_writes() {
    let mut obs = observation_at_record();
    obs.app_run_capture_privileges = (true, true, true);
    obs.tables
        .get_mut("runs")
        .expect("runs table")
        .insert("legacy_extra".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    let repair = plan
        .actions
        .iter()
        .find(|action| action.type_ == RunPlaneActionType::RepairRunCapturePrivilege)
        .expect("broad application-role grant is repaired");
    assert_eq!(repair.target, "runs.capture_mode");
    assert!(
        repair
            .sql
            .contains("REVOKE ALL PRIVILEGES ON TABLE \"demo\".runs")
    );
    assert!(repair.sql.contains("REVOKE SELECT ("));
    assert!(repair.sql.contains("project_env_key"));
    assert!(repair.sql.contains("capture_mode"));
    assert!(!repair.sql.contains("GRANT INSERT"));
    assert!(!repair.sql.contains("GRANT UPDATE"));
    assert!(repair.sql.contains("has_any_column_privilege"));
    assert!(
        repair
            .sql
            .contains("run-capture-author-sql-write-authority")
    );
}

#[test]
fn drifted_and_missing_checks_plan_exact_repairs() {
    let mut obs = observation_at_record();
    obs.checks.insert(
        ("runs".to_string(), "runs_fail_type_check".to_string()),
        "CHECK (fail_type = 'terminal'::text)".to_string(),
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    let repairs: Vec<&RunPlaneAction> = plan
        .actions
        .iter()
        .filter(|action| action.type_ == RunPlaneActionType::RepairConstraint)
        .collect();
    assert_eq!(
        repairs.len(),
        1,
        "only the changed constraint: {repairs:#?}"
    );
    assert!(repairs.iter().any(|action| {
        action.target == "runs.runs_fail_type_check"
            && action
                .sql
                .contains("DROP CONSTRAINT \"runs_fail_type_check\"")
            && action.sql.contains("unbound-requirement")
    }));
}

// `cancelled_node_error_check_is_repaired` was retired here: its subject was
// the `node_runs_error_type_check` spec, which wamn-0h0g.26.3.1 (204220e8)
// deleted from `CHECK_SPECS` with the projection it constrained.
// `drifted_and_missing_checks_plan_exact_repairs` above keeps the
// drop-then-add repair test on live record checks.

#[test]
fn extra_record_check_is_removed_but_base_grants_check_is_untouched() {
    let mut obs = observation_at_record();
    obs.checks.insert(
        ("runs".to_string(), "legacy_runs_check".to_string()),
        "CHECK (true)".to_string(),
    );
    obs.tables
        .insert("widgets".to_string(), ["id".to_string()].into());
    obs.checks.insert(
        ("widgets".to_string(), "widgets_check".to_string()),
        "CHECK (true)".to_string(),
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    let drops: Vec<&RunPlaneAction> = plan
        .actions
        .iter()
        .filter(|action| action.type_ == RunPlaneActionType::DropExtraConstraint)
        .collect();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].target, "runs.legacy_runs_check");
}

#[test]
fn missing_helpers_and_record_triggers_are_repaired() {
    let mut obs = observation_at_record();
    obs.helper_functions.clear();
    obs.triggers.clear();
    let plan = plan_run_plane(&schema("demo"), &obs);
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| action.type_ == RunPlaneActionType::RepairHelperFunction)
            .count(),
        3
    );
    let terminal_delete_guard = plan
        .actions
        .iter()
        .find(|action| {
            action.type_ == RunPlaneActionType::RepairHelperFunction
                && action.target == "guard_terminal_run_delete"
        })
        .expect("terminal-delete guard repair");
    assert!(
        terminal_delete_guard.sql.contains(
            "IF OLD.status NOT IN ('completed', 'failed', 'infrastructure-failure') THEN"
        )
    );
    assert!(terminal_delete_guard.sql.contains("ERRCODE = '55000'"));
    assert!(
        terminal_delete_guard
            .sql
            .contains("MESSAGE = 'run-delete-nonterminal'")
    );
    assert!(!terminal_delete_guard.sql.contains("SECURITY DEFINER"));
    assert!(plan.actions.iter().any(|action| {
        action.type_ == RunPlaneActionType::RepairTrigger
            && action.target == "runs.runs_event_lineage_immutable"
    }));
    assert!(plan.actions.iter().any(|action| {
        action.type_ == RunPlaneActionType::RepairTrigger
            && action.target == "runs.runs_admission_pins_immutable"
    }));
    let terminal_delete_trigger = plan
        .actions
        .iter()
        .find(|action| {
            action.type_ == RunPlaneActionType::RepairTrigger
                && action.target == "runs.runs_terminal_delete_only"
        })
        .expect("terminal-delete trigger repair");
    assert!(
        terminal_delete_trigger
            .sql
            .contains("runs_terminal_delete_only BEFORE DELETE ON")
    );
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| action.type_ == RunPlaneActionType::RepairTrigger)
            .count(),
        5
    );
}

#[test]
fn extra_record_trigger_is_removed_but_base_grants_trigger_is_untouched() {
    let mut obs = observation_at_record();
    obs.triggers.insert(
        ("runs".to_string(), "legacy_runs_trigger".to_string()),
        "CREATE TRIGGER legacy_runs_trigger".to_string(),
    );
    obs.triggers.insert(
        ("widgets".to_string(), "widgets_trigger".to_string()),
        "CREATE TRIGGER widgets_trigger".to_string(),
    );
    let plan = plan_run_plane(&schema("demo"), &obs);
    let drops: Vec<&RunPlaneAction> = plan
        .actions
        .iter()
        .filter(|action| action.type_ == RunPlaneActionType::DropExtraTrigger)
        .collect();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].target, "runs.legacy_runs_trigger");
}

/// The queue-missing manifestation (the live poc_f1 case): run-state and
/// legacy flow registry (fixture-only) present, queue absent → exactly the
/// global queue create (plus the idempotent schema ensure).
#[test]
fn queue_missing_plans_only_the_queue_creates() {
    let mut obs = observation_at_record();
    obs.tables.remove("run_queue");
    obs.indexes.remove("run_queue_leasable");
    let plan = plan_run_plane(&schema("poc_f1"), &obs);
    let creates: Vec<&str> = plan
        .actions
        .iter()
        .filter(|a| a.type_ == RunPlaneActionType::CreateTable)
        .map(|a| a.target.as_str())
        .collect();
    assert_eq!(creates, ["run_queue"]);
    assert!(
        plan.actions
            .iter()
            .all(|a| a.type_ != RunPlaneActionType::AddColumn)
    );
}

/// The run-plane reconciler's dot-anchored rewrite changes qualified names
/// and the schema header, but not prose.
#[test]
fn schema_rewrite_is_dot_anchored() {
    let schema = schema("poc_f1");
    // `run-state.sql` is the only run-plane record carrying the schema
    // header. The legacy flow registry (fixture-only) was the second input
    // to this sweep until wamn-0h0g.12.102 (e45ca35b) deleted it with its
    // call site.
    let out = rewrite_schema(RUN_STATE_SQL, &schema);
    assert!(out.contains("CREATE TABLE poc_f1.runs"), "runs");
    assert!(!out.contains("wamn_run."), "no qualified wamn_run left");
    assert!(!out.contains("SCHEMA wamn_run"), "schema header rewritten");
    // The GUARDED schema-create form rewrites too (the pre-wamn-1wdq bug:
    // `SCHEMA wamn_run` is not a substring of `SCHEMA IF NOT EXISTS
    // wamn_run`, so the header create silently targeted `wamn_run`).
    assert!(out.contains("CREATE SCHEMA IF NOT EXISTS poc_f1 "));
    assert!(!out.contains("IF NOT EXISTS wamn_run"));
    // The prose mention of the wamn_run_store crate must survive verbatim.
    assert!(rewrite_schema(RUN_STATE_SQL, &schema).contains("wamn_run_store"));
    assert!(
        !rewrite_schema(RUN_STATE_SQL, &schema).contains("SET search_path = pg_catalog, wamn_run")
    );
    assert!(
        !rewrite_schema(RUN_STATE_SQL, &schema)
            .contains("SET search_path = pg_catalog, pg_temp, poc_f1")
    );
}
