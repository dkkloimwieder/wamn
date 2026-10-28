use std::error::Error as _;

use super::*;

fn schema(value: &str) -> BareSchemaName {
    BareSchemaName::new(value).expect("test schema is valid")
}

#[test]
fn bare_schema_delegates_postgresql_identifier_boundaries_to_pg_core() {
    let at_limit = format!("s{}", "a".repeat(62));
    let accepted = BareSchemaName::new(at_limit.clone()).expect("63 bytes are accepted");
    assert_eq!(accepted.as_str(), at_limit);
    assert_eq!(accepted.quoted(), format!("\"{at_limit}\""));

    let over_limit = format!("s{}", "a".repeat(63));
    let error = BareSchemaName::new(over_limit).expect_err("64 bytes are rejected");
    assert_eq!(
        error.reason(),
        "identifier exceeds PostgreSQL's 63-byte limit"
    );
    assert_eq!(
        error.source().map(ToString::to_string).as_deref(),
        Some("identifier exceeds PostgreSQL's 63-byte limit"),
        "the rejection retains pg-core's canonical error as its source"
    );

    assert_eq!(
        BareSchemaName::new("").unwrap_err().reason(),
        "identifier is empty"
    );
    assert_eq!(
        BareSchemaName::new("safe\0suffix").unwrap_err().reason(),
        "identifier contains NUL"
    );
}

#[test]
fn overlong_schema_inputs_cannot_alias_after_postgresql_truncation() {
    let first = format!("s{}", "a".repeat(63));
    let second = format!("s{}b", "a".repeat(62));
    assert_ne!(first, second);
    assert_eq!(&first.as_bytes()[..63], &second.as_bytes()[..63]);
    assert!(BareSchemaName::new(first).is_err());
    assert!(BareSchemaName::new(second).is_err());
}

#[test]
fn bare_schema_measures_utf8_bytes_before_checking_bare_syntax() {
    let over_limit = format!("s{}", "é".repeat(32));
    assert_eq!(
        BareSchemaName::new(over_limit).unwrap_err().reason(),
        "identifier exceeds PostgreSQL's 63-byte limit"
    );

    let within_limit_but_not_bare = format!("s{}", "é".repeat(31));
    assert_eq!(
        BareSchemaName::new(within_limit_but_not_bare)
            .unwrap_err()
            .reason(),
        "schema name must match the lowercase bare identifier syntax [a-z_][a-z0-9_]*"
    );
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
        scenario_author_role: Some(ScenarioAuthorRoleObservation {
            can_login: false,
            is_superuser: false,
            can_create_database: false,
            can_create_role: false,
            inherits_roles: false,
            can_replicate: false,
            bypasses_rls: false,
        }),
        environment_policy_row_security: Some(environment_policy_row_security_at_record()),
        ..Default::default()
    };
    obs.scenario_author_schema_usage
        .extend(["catalog".to_string(), "demo".to_string()]);
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
        for (grantee, privileges) in [("wamn_app", spec.app), (SCENARIO_AUTHOR_ROLE, spec.author)] {
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
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        obs.effect_table_owners
            .insert(table.to_string(), "platform_admin".to_string());
        // All three tables are at record without append authority — born
        // parked, matching `deploy/sql/run-state.sql`.
        let key = (table.to_string(), "wamn_app".to_string());
        let privileges: BTreeSet<String> = ["SELECT".to_string()].into_iter().collect();
        obs.effect_table_privileges
            .insert(key.clone(), privileges.clone());
        obs.effect_table_effective_privileges
            .insert(key.clone(), privileges.clone());
        obs.effect_table_effective_column_privileges
            .insert(key, privileges);
    }
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
    obs.indexes.insert(
        "effect_attempts_occurrence_key".to_string(),
        "CREATE UNIQUE INDEX effect_attempts_occurrence_key ON wamn_run.effect_attempts USING btree (env_key, run_id, frame_id, local_node_id, occurrence)".to_string(),
    );
    obs.indexes.insert(
        "effect_attempts_dispatch_identity_key".to_string(),
        EFFECT_ATTEMPTS_DISPATCH_IDENTITY_KEY_DEF.to_string(),
    );
    obs.indexes.insert(
        "effect_attempt_dispatches_occurrence_key".to_string(),
        EFFECT_DISPATCHES_OCCURRENCE_KEY_DEF.to_string(),
    );
    obs.defaulted_columns.insert((
        "effect_attempts".to_string(),
        "attempt_started_at".to_string(),
    ));
    for (table, column, ty, not_null) in [
        ("effect_attempts", "root_plan_hash", "text", true),
        ("effect_attempts", "current_plan_hash", "text", true),
        ("effect_attempts", "frame_id", "bigint", true),
        ("effect_attempts", "parent_frame_id", "bigint", false),
        ("effect_attempts", "call_site_id", "text", false),
        ("effect_attempts", "local_node_id", "text", true),
        ("effect_attempts", "source_artifact_hash", "text", true),
        ("effect_attempts", "requirement_name", "text", true),
        ("effect_attempt_dispatches", "run_id", "text", true),
        ("effect_attempt_dispatches", "frame_id", "bigint", true),
        ("effect_attempt_dispatches", "local_node_id", "text", true),
        ("effect_attempt_dispatches", "occurrence", "integer", true),
    ] {
        let key = (table.to_string(), column.to_string());
        obs.column_types.insert(key.clone(), ty.to_string());
        if not_null {
            obs.non_nullable_columns.insert(key);
        }
    }
    for (column, ty) in [("package_id", "text"), ("environment", "text")] {
        let key = ("runs".to_string(), column.to_string());
        obs.non_nullable_columns.insert(key.clone());
        obs.column_types.insert(key, ty.to_string());
    }
    obs.foreign_keys.insert(
        ("runs".to_string(), "runs_release_fk".to_string()),
        RUNS_RELEASE_FK_DEF.to_string(),
    );
    obs.triggers.insert(
        (
            "runs".to_string(),
            "runs_admission_pins_immutable".to_string(),
        ),
        RUNS_ADMISSION_PINS_TRIGGER_DEF.to_string(),
    );
    obs.foreign_keys.insert(
        (
            "effect_attempt_dispatches".to_string(),
            EFFECT_DISPATCH_ATTEMPT_FK_NAME.to_string(),
        ),
        EFFECT_DISPATCH_ATTEMPT_FK_DEF.to_string(),
    );
    obs.foreign_keys.insert(
        (
            "effect_attempt_outcomes".to_string(),
            EFFECT_OUTCOME_DISPATCH_FK_NAME.to_string(),
        ),
        EFFECT_OUTCOME_DISPATCH_FK_DEF.to_string(),
    );
    obs
}

#[test]
fn run_queue_record_columns_pin_the_global_fifo_shape() {
    let cols = record_columns(RUN_QUEUE_SQL, "wamn_run", "run_queue");
    let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        names,
        [
            "created_at",
            "created_by",
            "updated_at",
            "updated_by",
            "env_key",
            "run_id",
            "priority",
            "available_at",
            "stream_seq",
            "lease_owner",
            "lease_expires_at",
            "lease_generation",
            "attempts",
            "max_attempts",
            "enqueued_at",
        ]
    );
    let definitions: Vec<&str> = cols
        .iter()
        .map(|(_, definition)| definition.as_str())
        .collect();
    assert_eq!(
        definitions,
        [
            "created_at timestamptz NOT NULL",
            "created_by uuid NOT NULL",
            "updated_at timestamptz NOT NULL",
            "updated_by uuid NOT NULL",
            "env_key text NOT NULL CHECK (env_key <> '')",
            "run_id text NOT NULL",
            "priority int NOT NULL DEFAULT 0",
            "available_at timestamptz NOT NULL DEFAULT now()",
            "stream_seq bigint NOT NULL DEFAULT 0",
            "lease_owner text",
            "lease_expires_at timestamptz",
            "lease_generation bigint NOT NULL DEFAULT 0 CHECK (lease_generation >= 0)",
            "attempts int NOT NULL DEFAULT 0",
            "max_attempts int NOT NULL DEFAULT 20",
            "enqueued_at timestamptz NOT NULL DEFAULT now()",
        ]
    );
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
    assert!(status.contains("'effect-uncertain'"), "{status}");
    assert!(
        status.ends_with("))"),
        "CHECK closes inside the definition: {status}"
    );
}

#[test]
fn runs_failure_and_outcome_check_mirrors_are_exact_and_frozen() {
    let expected_fail_kind = "CHECK (fail_type = ANY (ARRAY['terminal'::text, 'retry-exhausted'::text, 'invalid-input'::text, 'runaway-budget'::text, 'effect-uncertain'::text, 'depth-budget'::text, 'dispatch-budget'::text, 'unresolvable-name'::text, 'hash-invalid-bytes'::text, 'foreign-revision'::text, 'incompatible-contract'::text, 'unbound-requirement'::text]))";
    let fail_type = CHECK_SPECS
        .iter()
        .find(|spec| spec.table == "runs" && spec.name == "runs_fail_type_check")
        .expect("runs.fail_type CHECK mirror exists");
    assert_eq!(fail_type.definition, expected_fail_kind);

    let status = CHECK_SPECS
        .iter()
        .find(|spec| spec.table == "runs" && spec.name == "runs_status_check")
        .expect("runs.status CHECK mirror exists");
    assert_eq!(
        status.definition,
        "CHECK (status = ANY (ARRAY['dispatched'::text, 'running'::text, 'completed'::text, 'failed'::text, 'infrastructure-failure'::text, 'effect-uncertain'::text]))"
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

/// A manual workflow run is its own grain
/// (docs/architecture/execution.md#environment-workflows): no flow, no service
/// principal, no registration, no binding world; a release pin, frozen node
/// digests as a JSON object, an executing principal and role, and the invoking
/// user. A schedule workflow run is the same grain with the schedule as its
/// cause and no invoking user. Every other grain leaves those columns NULL, and
/// admission pins them.
#[test]
fn runs_manual_grain_check_mirrors_are_exact() {
    let check = |name: &str| {
        CHECK_SPECS
            .iter()
            .find(|spec| spec.table == "runs" && spec.name == name)
            .unwrap_or_else(|| panic!("runs.{name} CHECK mirror exists"))
            .definition
    };
    let grain = check("runs_execution_grain_check");
    assert!(
        grain.contains(
            " OR flow_id IS NULL AND flow_version IS NULL \
             AND NOT trigger_source IS DISTINCT FROM 'manual'::text \
             AND registration_id IS NULL AND service_principal_id IS NULL \
             AND wiring_id IS NOT NULL AND wiring_id <> ''::text \
             AND wiring_version IS NOT NULL AND wiring_version > 0 \
             AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text \
             AND manifest_digest IS NOT NULL AND node_digests_json IS NOT NULL \
             AND jsonb_typeof(node_digests_json) = 'object'::text \
             AND execution_principal_id IS NOT NULL \
             AND execution_role IS NOT NULL AND execution_role <> ''::text \
             AND NOT invoked_by IS DISTINCT FROM 'manual'::text \
             AND invoked_by_user_id IS NOT NULL AND binding_world_json IS NULL OR "
        ),
        "{grain}"
    );
    assert!(
        grain.ends_with(
            " OR flow_id IS NULL AND flow_version IS NULL \
             AND NOT trigger_source IS DISTINCT FROM 'schedule'::text \
             AND registration_id IS NULL AND service_principal_id IS NULL \
             AND wiring_id IS NOT NULL AND wiring_id <> ''::text \
             AND wiring_version IS NOT NULL AND wiring_version > 0 \
             AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text \
             AND manifest_digest IS NOT NULL AND node_digests_json IS NOT NULL \
             AND jsonb_typeof(node_digests_json) = 'object'::text \
             AND execution_principal_id IS NOT NULL \
             AND execution_role IS NOT NULL AND execution_role <> ''::text \
             AND NOT invoked_by IS DISTINCT FROM 'schedule'::text \
             AND invoked_by_user_id IS NULL AND binding_world_json IS NULL)"
        ),
        "{grain}"
    );
    assert_eq!(
        check("runs_execution_identity_check"),
        "CHECK (NOT trigger_source IS DISTINCT FROM 'manual'::text \
         OR NOT trigger_source IS DISTINCT FROM 'schedule'::text \
         OR NOT trigger_source IS DISTINCT FROM 'event'::text \
         OR execution_principal_id IS NULL AND execution_role IS NULL \
         AND invoked_by IS NULL AND invoked_by_user_id IS NULL \
         AND node_digests_json IS NULL)"
    );
    // Only an automation run carries a service principal.
    assert_eq!(
        check("runs_service_principal_check"),
        "CHECK ((NOT trigger_source IS DISTINCT FROM 'automation'::text) \
         = (service_principal_id IS NOT NULL))"
    );
    for pinned in [
        "execution_principal_id",
        "execution_role",
        "invoked_by",
        "invoked_by_user_id",
        "node_digests_json",
    ] {
        assert!(
            RUNS_ADMISSION_PINS_TRIGGER_DEF.contains(&format!(" {pinned},"))
                || RUNS_ADMISSION_PINS_TRIGGER_DEF.contains(&format!(" {pinned} ON")),
            "the admission pins trigger names {pinned}"
        );
        assert!(
            RUNS_ADMISSION_PINS_TRIGGER_SQL.contains(pinned),
            "the admission pins trigger SQL names {pinned}"
        );
    }
}

/// Sections carry the table's whole apparatus: indexes, RLS, policy, grant.
#[test]
fn table_sections_carry_indexes_rls_and_grants() {
    let rq = table_section(RUN_QUEUE_SQL, "wamn_run", "run_queue");
    assert!(rq.contains("CREATE INDEX run_queue_claimable"));
    assert!(rq.contains("available_at, stream_seq, run_id, lease_expires_at"));
    assert!(rq.contains("FORCE ROW LEVEL SECURITY"));
    assert!(rq.contains("FROM PUBLIC, wamn_app"));
    assert!(!rq.contains("TO wamn_app"));
    for retired in [
        "partition_key",
        "partition_policy",
        "partition_owner",
        "run_dead_letters",
        "run_queue_partition",
    ] {
        assert!(
            !rq.contains(retired),
            "retired queue DDL remains: {retired}"
        );
    }

    let package = table_section(CATALOG_SCHEMA_SQL, "catalog", "packages");
    assert!(package.contains("artifact_digest"));
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
        7,
        "all seven retained run-plane tables are at target"
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
    for (column, check) in [
        ("capture_mode", "runs_capture_mode_check"),
        ("durability_class", "runs_durability_class_check"),
    ] {
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
                action.kind == RunPlaneActionKind::RepairConstraint
                    && action.target == format!("runs.{check}")
            }),
            "a bare {column} costs a second reconcile turn: {:#?}",
            plan.actions
        );
    }
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
            "connection_bindings".to_string(),
            "wamn_app".to_string(),
        ))
        .or_default()
        .insert("INSERT".to_string());
    obs.authoring_effective_table_privileges
        .entry((
            "catalog".to_string(),
            "releases".to_string(),
            SCENARIO_AUTHOR_ROLE.to_string(),
        ))
        .or_default()
        .insert("UPDATE".to_string());
    obs.authoring_effective_column_privileges
        .entry((
            "catalog".to_string(),
            "connection_requirements".to_string(),
            "wamn_app".to_string(),
        ))
        .or_default()
        .insert("UPDATE".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    for table in [
        "packages",
        "connection_bindings",
        "releases",
        "connection_requirements",
    ] {
        let repair = plan
            .actions
            .iter()
            .find(|action| {
                action.kind == RunPlaneActionKind::RepairAuthoringPrivilege
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
        .find(|action| action.kind == RunPlaneActionKind::RepairRowSecurity)
        .expect("environment-policy row-security drift must be repaired")
}

#[test]
fn exact_environment_policy_row_security_plans_no_repair() {
    let plan = plan_run_plane(&schema("demo"), &observation_at_record());
    assert!(
        plan.actions
            .iter()
            .all(|action| action.kind != RunPlaneActionKind::RepairRowSecurity)
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
fn missing_environment_policy_env_key_policy_mutant_plans_repair() {
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
            .contains("CREATE POLICY environment_policies_env_key")
    );
}

#[test]
fn widened_environment_policy_env_key_policy_mutant_plans_exact_replacement() {
    let mut obs = observation_at_record();
    let row_security = obs
        .environment_policy_row_security
        .as_mut()
        .expect("record relation");
    let policy = row_security
        .policies
        .get_mut("environment_policies_env_key")
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
    // must recreate BOTH arms. Recreating only the floor would silently
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
            action.kind == RunPlaneActionKind::RepairAuthoringPrivilege
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

#[test]
fn current_populated_single_target_creates_missing_peer_without_frame_refusal() {
    let mut obs = observation_at_record();
    obs.tables.remove("effect_attempts");

    let plan = plan_run_plane(&schema("demo"), &obs);
    assert!(plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::CreateTable && action.target == "effect_attempts"
    }));
}

#[test]
fn populated_current_frame_identity_drift_refuses() {
    #[expect(
        clippy::type_complexity,
        reason = "the table-driven test pairs each drift label with one noncapturing mutation"
    )]
    // Current frame identities retain their type, nullability, and checks.
    let cases: [(&str, fn(&mut RunPlaneObservation)); 3] = [
        ("wrong-type", |obs| {
            obs.column_types.insert(
                ("effect_attempts".to_string(), "frame_id".to_string()),
                "integer".to_string(),
            );
        }),
        ("wrong-nullability", |obs| {
            obs.non_nullable_columns.remove(&(
                "effect_attempts".to_string(),
                "requirement_name".to_string(),
            ));
        }),
        ("wrong-frame-check", |obs| {
            obs.checks.insert(
                (
                    "effect_attempts".to_string(),
                    "effect_attempts_frame_relation_check".to_string(),
                ),
                "CHECK (frame_id >= 0)".to_string(),
            );
        }),
    ];

    for (case, mutate) in cases {
        let mut obs = observation_at_record();
        mutate(&mut obs);
        obs.effect_record_rows = 1;
        let plan = plan_run_plane(&schema("demo"), &obs);
        let action = plan.actions.first().expect(case);
        assert_eq!(action.kind, RunPlaneActionKind::RefuseSchemaDrift, "{case}");
        assert!(action.sql.contains("effect-ledger-schema-drift"));
        assert_eq!(plan.actions.len(), 1);
    }
}

#[test]
fn effect_lineage_and_temporal_fks_are_repaired_on_existing_tables() {
    let mut obs = observation_at_record();
    for (table, name) in [
        ("effect_attempt_dispatches", EFFECT_DISPATCH_ATTEMPT_FK_NAME),
        ("effect_attempt_outcomes", EFFECT_OUTCOME_DISPATCH_FK_NAME),
    ] {
        obs.foreign_keys
            .remove(&(table.to_string(), name.to_string()));
    }

    let plan = plan_run_plane(&schema("demo"), &obs);
    let targets: BTreeSet<String> = plan
        .actions
        .into_iter()
        .filter(|action| action.kind == RunPlaneActionKind::RepairForeignKey)
        .map(|action| action.target)
        .collect();
    for target in [
        "effect_attempt_dispatches.effect_attempt_dispatches_attempt_fk",
        "effect_attempt_outcomes.effect_attempt_outcomes_dispatch_fk",
    ] {
        assert!(
            targets.contains(target),
            "missing repair for {target}: {targets:#?}"
        );
    }
}

/// From zero (an empty database): the full run-plane set in FK order behind
/// the schema ensure, plus the whole catalog schema — the fixture-wipe
/// restore path (manifestations 3 + 5).
#[test]
fn from_zero_plans_the_full_set_in_order() {
    let obs = RunPlaneObservation::default();
    let plan = plan_run_plane(&schema("wamn_runner_demo"), &obs);
    let kinds: Vec<RunPlaneActionKind> = plan.actions.iter().map(|a| a.kind).collect();
    assert_eq!(kinds[0], RunPlaneActionKind::EnsureScenarioAuthorRole);
    assert_eq!(kinds[1], RunPlaneActionKind::EnsureSchema);
    let creates: Vec<&str> = plan
        .actions
        .iter()
        .filter(|a| a.kind == RunPlaneActionKind::CreateTable)
        .map(|a| a.target.as_str())
        .collect();
    assert_eq!(
        creates,
        [
            "environment_policies",
            "runs",
            "effect_attempts",
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
            "operator_run_actions",
            "run_queue"
        ]
    );
    assert!(
        plan.actions
            .iter()
            .any(|a| a.kind == RunPlaneActionKind::EnsureCatalogSchema)
    );
    let pin_helper = plan
        .actions
        .iter()
        .position(|action| {
            action.kind == RunPlaneActionKind::RepairHelperFunction
                && action.target == "guard_run_admission_pins_immutable"
        })
        .expect("run admission-pin helper is provisioned");
    let runs_table = plan
        .actions
        .iter()
        .position(|action| {
            action.kind == RunPlaneActionKind::CreateTable && action.target == "runs"
        })
        .expect("runs table is provisioned");
    assert!(
        pin_helper < runs_table,
        "the admission-pin helper must exist before the runs trigger"
    );
    // No column/index repairs on tables being created (sections carry them).
    assert!(!kinds.contains(&RunPlaneActionKind::AddColumn));
    assert!(!kinds.contains(&RunPlaneActionKind::CreateIndex));
    assert!(!kinds.contains(&RunPlaneActionKind::RepairForeignKey));
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
        .find(|action| action.kind == RunPlaneActionKind::RepairRunCapturePrivilege)
        .expect("broad application-role grant is repaired");
    assert_eq!(repair.target, "runs.capture_mode");
    assert!(
        repair
            .sql
            .contains("REVOKE ALL PRIVILEGES ON TABLE \"demo\".runs")
    );
    assert!(repair.sql.contains("REVOKE SELECT ("));
    assert!(repair.sql.contains("env_key"));
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
fn stale_scenario_author_reads_are_revoked_without_regrant() {
    let mut obs = observation_at_record();
    for table in ["environment_policies", "runs"] {
        let key = (
            "demo".to_string(),
            table.to_string(),
            SCENARIO_AUTHOR_ROLE.to_string(),
        );
        for map in [
            &mut obs.authoring_table_privileges,
            &mut obs.authoring_effective_table_privileges,
            &mut obs.authoring_effective_column_privileges,
        ] {
            map.insert(key.clone(), BTreeSet::from(["SELECT".to_string()]));
        }
    }

    let plan = plan_run_plane(&schema("demo"), &obs);
    let environment = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairAuthoringPrivilege
                && action.target == "demo.environment_policies"
        })
        .expect("the stale environment-policy read is repaired");
    assert!(
        environment.sql.contains(
            "REVOKE ALL PRIVILEGES ON TABLE \"demo\".\"environment_policies\" FROM wamn_scenario_author"
        ),
        "{}",
        environment.sql
    );
    assert!(
        !environment.sql.contains(
            "GRANT SELECT ON TABLE \"demo\".\"environment_policies\" TO wamn_scenario_author"
        ),
        "{}",
        environment.sql
    );

    let runs = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::RepairRunCapturePrivilege)
        .expect("the stale runs read is repaired");
    assert!(
        runs.sql.contains(
            "REVOKE ALL PRIVILEGES ON TABLE \"demo\".runs FROM PUBLIC, wamn_app, wamn_scenario_author"
        ),
        "{}",
        runs.sql
    );
    assert!(
        !runs
            .sql
            .contains("GRANT SELECT ON TABLE \"demo\".runs TO wamn_scenario_author"),
        "{}",
        runs.sql
    );
}

#[test]
fn drifted_and_missing_checks_plan_exact_repairs() {
    let mut obs = observation_at_record();
    obs.checks.insert(
        ("runs".to_string(), "runs_fail_type_check".to_string()),
        "CHECK (fail_type = 'terminal'::text)".to_string(),
    );
    obs.checks.remove(&(
        "effect_attempts".to_string(),
        "effect_attempts_deadline_check".to_string(),
    ));

    let plan = plan_run_plane(&schema("demo"), &obs);
    let repairs: Vec<&RunPlaneAction> = plan
        .actions
        .iter()
        .filter(|action| action.kind == RunPlaneActionKind::RepairConstraint)
        .collect();
    assert_eq!(
        repairs.len(),
        2,
        "only the two drifted checks: {repairs:#?}"
    );
    assert!(repairs.iter().any(|action| {
        action.target == "runs.runs_fail_type_check"
            && action
                .sql
                .contains("DROP CONSTRAINT \"runs_fail_type_check\"")
            && action.sql.contains("effect-uncertain")
    }));
    assert!(repairs.iter().any(|action| {
        action.target == "effect_attempts.effect_attempts_deadline_check"
            && !action.sql.contains("DROP CONSTRAINT")
            && action
                .sql
                .contains("attempt_started_at <= attempt_deadline_at")
    }));
}

// `cancelled_node_error_check_is_repaired` was retired here: its subject was
// the `node_runs_error_kind_check` spec, which wamn-0h0g.26.3.1 (204220e8)
// deleted from `CHECK_SPECS` with the projection it constrained.
// `drifted_and_missing_checks_plan_exact_repairs` above keeps the
// drop-then-add repair test on live record checks.

/// The separate test-set store is gone: a draft's own `cases` are the only
/// test source, so no relation, privilege, helper, or FK may name one.
#[test]
fn the_authoring_test_set_store_is_absent_from_the_record() {
    assert!(
        !CHECK_SPECS
            .iter()
            .any(|spec| spec.table == "authoring_test_sets")
    );
    assert!(
        !AUTHORING_PRIVILEGE_SPECS
            .iter()
            .any(|spec| spec.table == "authoring_test_sets")
    );
    assert!(
        !helper_specs()
            .iter()
            .any(|spec| spec.name == "reject_immutable_authoring_test_set_change")
    );
    assert!(
        !trigger_specs()
            .iter()
            .any(|trigger| trigger.table == "authoring_test_sets")
    );
    let source = select_run_plane_helper_functions_sql();
    assert!(!source.contains("authoring_test_sets"), "{source}");
}

/// The FRESH-INSTALL emitter and the RECONCILE emitter must agree, relation
/// by relation, on what `wamn_scenario_author` holds (wamn-0h0g.22.20).
///
/// This is the one assertion `observation_at_record` cannot make. That
/// fixture builds its privilege map FROM `AUTHORING_PRIVILEGE_SPECS`, so a
/// spec that disagrees with the shipped DDL stays self-consistent and every
/// planner test keeps passing while a provisioned environment converges
/// forever: the reconciler re-grants what the file never granted, and the
/// drift report names the revoked state as the drift. Reading the grants out
/// of `catalog-schema.sql` is the only way to make the two answer
/// independently.
///
/// Comment lines are dropped before the scan. Static checked-in DDL cannot
/// otherwise tell a real declaration from a COMMENT mentioning one, and this
/// direction of the check would false-RED on a comment that merely quotes a
/// grant.
///
/// THE SCAN IS SHAPE-AGNOSTIC (wamn-0h0g.22.33). It formerly matched only
/// `GRANT SELECT ON catalog.x TO wamn_scenario_author;` — the author as SOLE
/// grantee — which made the equality closed over the relations an emitter
/// spells that way and blind to every other spelling. The three relations
/// `.22.33` found were granted `TO wamn_app, wamn_scenario_author`, exactly
/// the shape the old scan skipped, so a grant landing in `catalog-schema.sql`
/// in that form would have been reported here as agreement.
#[test]
fn the_catalog_ddl_and_the_authoring_specs_agree_on_the_author() {
    let granted_to_author = catalog_selects_for(CATALOG_SCHEMA_SQL, "wamn_scenario_author");
    let specified_for_author: BTreeSet<String> = AUTHORING_PRIVILEGE_SPECS
        .iter()
        .filter(|spec| matches!(spec.schema, AuthoringTableSchema::Catalog))
        .filter(|spec| !spec.author.is_empty())
        .map(|spec| spec.table.to_string())
        .collect();
    assert_eq!(
        granted_to_author, specified_for_author,
        "the fresh install and the reconciler disagree on the author's \
             catalog surface; landing this revoke in a subset of its three \
             emitters is strictly worse than landing none of it"
    );

    // The scan has to be able to SEE a grant, or the equality above is two
    // empty sets agreeing with each other. The relations the confinement
    // narrowed to `wamn_app` are the positive control: read out of the same
    // file by the same function, differing only in the grantee.
    let granted_to_app = catalog_selects_for(CATALOG_SCHEMA_SQL, "wamn_app");
    assert!(
        granted_to_app.len()
            >= AUTHORING_PRIVILEGE_SPECS
                .iter()
                .filter(|spec| matches!(spec.schema, AuthoringTableSchema::Catalog))
                .count(),
        "the grant scan matched {} app grants, so its statement shape no \
             longer matches the file and the author scan establishes nothing",
        granted_to_app.len()
    );
}

/// Every `catalog` relation `sql` GRANTs `grantee` a SELECT on, whatever else
/// the statement grants and whoever else it grants to.
///
/// This reads the fresh-install emitter independently of
/// [`AUTHORING_PRIVILEGE_SPECS`], which drives run-plane reconciliation. The
/// equality above and the literal fixtures below keep those two emitters in
/// agreement (`wamn-0h0g.22.33`).
fn catalog_selects_for(sql: &str, grantee: &str) -> BTreeSet<String> {
    let body = sql
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("--"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut relations_read = BTreeSet::new();
    for statement in body.split(';') {
        let statement = statement.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(rest) = statement.strip_prefix("GRANT ") else {
            continue;
        };
        let Some((privileges, rest)) = rest.split_once(" ON catalog.") else {
            continue;
        };
        let grants_select = privileges
            .split(',')
            .map(str::trim)
            .any(|privilege| matches!(privilege, "SELECT" | "ALL" | "ALL PRIVILEGES"));
        if !grants_select {
            continue;
        }
        let Some((relations, grantees)) = rest.split_once(" TO ") else {
            continue;
        };
        if !grantees
            .split(',')
            .map(str::trim)
            .any(|name| name == grantee)
        {
            continue;
        }
        for relation in relations.split(',').map(str::trim) {
            relations_read.insert(
                relation
                    .strip_prefix("catalog.")
                    .unwrap_or(relation)
                    .to_string(),
            );
        }
    }
    relations_read
}

/// The scanner's answer on a LITERAL fixture (wamn-0h0g.22.33).
///
/// The cross-check above compares two sets the scan itself produces, so a
/// scan that silently stopped matching would report agreement. Deriving the
/// expectation from `CATALOG_SCHEMA_SQL` would be a tautology over the very
/// text under test, so the value is pinned here as a literal instead — one
/// case per shape a grant has ever taken in the tree.
#[test]
fn the_author_grant_scan_sees_every_shape_a_grant_can_take() {
    let fixture = "\
GRANT SELECT ON catalog.sole TO wamn_scenario_author;
GRANT SELECT ON catalog.trailing TO wamn_app, wamn_scenario_author;
GRANT SELECT, INSERT ON catalog.multi_privilege TO wamn_scenario_author;
GRANT SELECT ON catalog.listed_a, catalog.listed_b TO wamn_scenario_author;
GRANT SELECT ON catalog.app_only TO wamn_app;
GRANT USAGE ON SCHEMA catalog TO wamn_scenario_author;
GRANT wamn_scenario_author TO wamn_app;
GRANT INSERT ON catalog.write_only TO wamn_scenario_author;
-- GRANT SELECT ON catalog.commented TO wamn_scenario_author;
";
    assert_eq!(
        catalog_selects_for(fixture, "wamn_scenario_author"),
        [
            "listed_a",
            "listed_b",
            "multi_privilege",
            "sole",
            "trailing"
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<String>>()
    );
    assert_eq!(
        catalog_selects_for(fixture, "wamn_app"),
        ["app_only", "trailing"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<String>>()
    );
}

#[test]
fn extra_record_check_is_removed_but_floor_check_is_untouched() {
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
        .filter(|action| action.kind == RunPlaneActionKind::DropExtraConstraint)
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
            .filter(|action| action.kind == RunPlaneActionKind::RepairHelperFunction)
            .count(),
        5
    );
    let terminal_delete_guard = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairHelperFunction
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
    assert!(!terminal_delete_guard.sql.contains("effect-uncertain"));
    assert!(!terminal_delete_guard.sql.contains("SECURITY DEFINER"));
    assert!(plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairTrigger
            && action.target == "runs.runs_event_lineage_immutable"
    }));
    assert!(plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairTrigger
            && action.target == "runs.runs_admission_pins_immutable"
    }));
    let terminal_delete_trigger = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairTrigger
                && action.target == "runs.runs_terminal_delete_only"
        })
        .expect("terminal-delete trigger repair");
    assert!(
        terminal_delete_trigger
            .sql
            .contains("runs_terminal_delete_only BEFORE DELETE ON")
    );
    assert!(plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::RepairTrigger
            && action.target == "operator_run_actions.operator_run_actions_update_immutable"
    }));
    assert_eq!(
        plan.actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::RepairTrigger)
            .count(),
        17
    );
}

#[test]
fn effect_table_acl_repair_removes_table_and_column_drift() {
    let mut obs = observation_at_record();
    obs.effect_table_effective_privileges.insert(
        (
            "effect_attempts".to_string(),
            SCENARIO_AUTHOR_ROLE.to_string(),
        ),
        ["SELECT".to_string()].into_iter().collect(),
    );
    obs.effect_table_effective_column_privileges
        .entry(("effect_attempts".to_string(), "wamn_app".to_string()))
        .or_default()
        .insert("UPDATE".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    let table_action = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairEffectTablePrivilege
                && action.target == "demo.effect_attempts"
        })
        .expect("effect table ACL repair");
    assert!(table_action.sql.contains("REVOKE SELECT ("));
    assert!(table_action.sql.contains("has_any_column_privilege"));
    // BORN PARKED: the attempt table is re-granted READ ONLY, and the block
    // refuses to see the server still report the guest holding INSERT.
    assert!(
        table_action
            .sql
            .contains("GRANT SELECT ON TABLE \"demo\".\"effect_attempts\" TO wamn_app")
    );
    assert!(!table_action.sql.contains("GRANT SELECT, INSERT"));
    assert!(table_action.sql.contains(
        "ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
             WHERE pg_catalog.has_table_privilege('wamn_app'"
    ));
    assert!(table_action.sql.contains(
        "has_any_column_privilege('wamn_app', \
             '\"demo\".\"effect_attempts\"', 'INSERT,UPDATE,REFERENCES')"
    ));
}

/// The convergent author, for one sibling table. `deploy/sql/run-state.sql`
/// is only the BIRTH author; this builder is what an already-provisioned
/// database converges onto, so restoring the append here would re-mint the
/// dormant authority on every reconcile even with the DDL parked.
#[test]
fn dispatch_table_reconciles_parked() {
    assert_sibling_table_reconciles_parked("effect_attempt_dispatches");
}

/// The same arm for the second sibling, named separately so a mutant that
/// re-arms exactly one relation cannot hide behind the other.
#[test]
fn outcome_table_reconciles_parked() {
    assert_sibling_table_reconciles_parked("effect_attempt_outcomes");
}

fn assert_sibling_table_reconciles_parked(table: &str) {
    let mut obs = observation_at_record();
    obs.effect_table_effective_column_privileges
        .entry((table.to_string(), "wamn_app".to_string()))
        .or_default()
        .insert("UPDATE".to_string());
    let plan = plan_run_plane(&schema("demo"), &obs);
    let action = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairEffectTablePrivilege
                && action.target == format!("demo.{table}")
        })
        .expect("sibling table ACL repair");
    // The re-grant is READ ONLY…
    assert!(
        action.sql.contains(&format!(
            "GRANT SELECT ON TABLE \"demo\".\"{table}\" TO wamn_app"
        )),
        "{table}: sibling table is not re-granted read-only: {}",
        action.sql
    );
    assert!(
        !action.sql.contains("GRANT SELECT, INSERT"),
        "{table}: reconciler still re-mints a live append: {}",
        action.sql
    );
    // …and the generated self-check moved with it, so the step REFUSES to see
    // the server report an append the record does not carry.
    assert!(
        action.sql.contains(
            "ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                 WHERE pg_catalog.has_table_privilege('wamn_app'"
        ),
        "{table}: table-level self-check does not forbid INSERT: {}",
        action.sql
    );
    assert!(
        action.sql.contains(&format!(
            "has_any_column_privilege('wamn_app', \
                 '\"demo\".\"{table}\"', 'INSERT,UPDATE,REFERENCES')"
        )),
        "{table}: column-level self-check does not forbid INSERT: {}",
        action.sql
    );
}

#[test]
fn extra_record_trigger_is_removed_but_floor_trigger_is_untouched() {
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
        .filter(|action| action.kind == RunPlaneActionKind::DropExtraTrigger)
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
    obs.indexes.remove("run_queue_claimable");
    let plan = plan_run_plane(&schema("poc_f1"), &obs);
    let creates: Vec<&str> = plan
        .actions
        .iter()
        .filter(|a| a.kind == RunPlaneActionKind::CreateTable)
        .map(|a| a.target.as_str())
        .collect();
    assert_eq!(creates, ["run_queue"]);
    assert!(
        plan.actions
            .iter()
            .all(|a| a.kind != RunPlaneActionKind::AddColumn)
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
        rewrite_schema(RUN_STATE_SQL, &schema).contains("CREATE TABLE poc_f1.operator_run_actions")
    );
    assert!(
        !rewrite_schema(RUN_STATE_SQL, &schema).contains("SET search_path = pg_catalog, wamn_run")
    );
    assert!(
        !rewrite_schema(RUN_STATE_SQL, &schema)
            .contains("SET search_path = pg_catalog, pg_temp, poc_f1")
    );
}
/// Named mutant guard: omitting any one field lets a disabled/unforced RLS
/// flag or a missing/widened policy falsely observe as converged.
#[test]
fn environment_policy_row_security_observation_reads_the_exact_contract() {
    let flags = select_environment_policy_row_security_sql();
    assert!(flags.contains("relrowsecurity"));
    assert!(flags.contains("relforcerowsecurity"));

    let policies = select_environment_policy_policies_sql();
    for field in [
        "polname",
        "polcmd",
        "polpermissive",
        "polroles",
        "polqual",
        "polwithcheck",
    ] {
        assert!(policies.contains(field), "missing policy field {field}");
    }
    assert!(policies.contains("ORDER BY policy.polname"));
}

/// Observation SQL pins (the shell binds these verbatim; the live gate
/// shows they observe real state).
#[test]
fn observation_sql_is_pinned() {
    assert!(select_schema_columns_sql().contains("NOT a.attisdropped"));
    assert!(select_schema_indexes_sql().contains("pg_indexes"));
    assert!(catalog_schema_present_sql().contains("'catalog'"));
    assert!(select_schema_checks_sql().contains("con.contype = 'c'"));
    assert!(select_schema_checks_sql().contains("pg_get_constraintdef"));
    assert!(select_schema_foreign_keys_sql().contains("con.contype = 'f'"));
    assert!(select_schema_triggers_sql().contains("NOT t.tgisinternal"));
    assert!(select_scenario_author_role_sql().contains("rolbypassrls"));
    assert!(select_app_scenario_author_membership_sql().contains("'MEMBER'"));
    assert!(select_run_capture_privileges_sql().contains("has_table_privilege"));
    assert!(select_run_capture_privileges_sql().contains("has_column_privilege"));
    assert!(select_run_capture_privileges_sql().contains("capture_mode"));
    assert!(!select_authoring_table_privileges_sql().contains("authoring_report_reservations"));
    // Every observation query must see every relation the privilege
    // reconciler owns, or the planner reads an empty privilege set and plans
    // a repair that can never converge. Driven off the spec so a table added
    // to the reconciler and forgotten in an observation fails here.
    for observation in [
        select_authoring_table_privileges_sql(),
        select_authoring_effective_table_privileges_sql(),
        select_authoring_effective_column_privileges_sql(),
        select_authoring_table_owners_sql(),
    ] {
        for spec in AUTHORING_PRIVILEGE_SPECS {
            assert!(
                observation.contains(spec.table),
                "{}: {observation}",
                spec.table
            );
        }
    }
    assert!(select_authoring_effective_table_privileges_sql().contains("has_table_privilege"));
    assert!(select_authoring_effective_table_privileges_sql().contains("'releases'"));
    assert!(select_authoring_table_owners_sql().contains("relation.relowner"));
    assert!(
        select_authoring_effective_column_privileges_sql().contains("has_any_column_privilege")
    );
    assert!(select_authoring_effective_column_privileges_sql().contains("('SELECT'::text)"));
    assert!(select_scenario_author_schema_usage_sql().contains("has_schema_privilege"));
    assert!(select_run_plane_helper_functions_sql().contains("pg_get_functiondef"));
    assert!(
        select_run_plane_helper_functions_sql().contains("reject_immutable_effect_fact_change")
    );
    assert!(
        select_run_plane_helper_functions_sql()
            .contains("reject_immutable_operator_run_action_change")
    );
}
