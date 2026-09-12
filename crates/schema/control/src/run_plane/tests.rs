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
        effect_writer_role: Some(EffectWriterRoleObservation {
            can_login: false,
            is_superuser: false,
            can_create_database: false,
            can_create_role: false,
            inherits_roles: false,
            can_replicate: false,
            bypasses_rls: false,
            can_connect: false,
            owns_objects: false,
            membership_out_of_bounds: false,
        }),
        effect_writer_schema_privileges: (true, false),
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
        for (grantee, privileges) in
            [("wamn_app", spec.app), (SCENARIO_AUTHOR_ROLE, spec.author)]
        {
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
                        ["SELECT", "INSERT", "UPDATE", "REFERENCES"]
                            .contains(&privilege.as_str())
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
        // All three tables are at record WITHOUT the writer's append
        // authority — born parked, matching `deploy/sql/run-state.sql`.
        let writer_at_record: &[&str] = &["SELECT"];
        for (grantee, privileges) in [
            ("wamn_app", &["SELECT"][..]),
            (EFFECT_WRITER_ROLE, writer_at_record),
        ] {
            let key = (table.to_string(), grantee.to_string());
            let privileges: BTreeSet<String> = privileges
                .iter()
                .map(|privilege| (*privilege).to_string())
                .collect();
            obs.effect_table_privileges
                .insert(key.clone(), privileges.clone());
            obs.effect_table_effective_privileges
                .insert(key.clone(), privileges.clone());
            obs.effect_table_effective_column_privileges
                .insert(key, privileges);
        }
    }
    for (table, columns) in EFFECT_WRITER_RUN_READ_COLUMNS {
        for column in columns {
            obs.effect_writer_run_column_privileges.insert(
                (table.to_string(), (*column).to_string()),
                ["SELECT".to_string()].into_iter().collect(),
            );
        }
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
        "CREATE UNIQUE INDEX effect_attempts_occurrence_key ON wamn_run.effect_attempts USING btree (tenant_id, run_id, frame_id, local_node_id, occurrence)".to_string(),
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
    for (column, ty) in [
        ("package_id", "text"),
        ("effective_release_id", "integer"),
        ("environment", "text"),
    ] {
        let key = ("runs".to_string(), column.to_string());
        obs.non_nullable_columns.insert(key.clone());
        obs.column_types.insert(key, ty.to_string());
    }
    // The candidate-admission grain carriers (wamn-0h0g.8.5.1) are all
    // NULLABLE at record: a legacy flow row leaves the wiring leg NULL and a
    // component-era wiring row leaves the flow leg NULL, so only the pair
    // CHECKs constrain them. `run_wiring_identity_contract_complete` reads
    // every one of these types, so the record observation must carry them or
    // the at-record schema plans its own cutover forever.
    for (column, ty) in [
        ("flow_id", "text"),
        ("flow_version", "integer"),
        ("wiring_id", "text"),
        ("wiring_version", "integer"),
        ("wiring_hash", "text"),
        ("binding_world_json", "jsonb"),
    ] {
        obs.column_types
            .insert(("runs".to_string(), column.to_string()), ty.to_string());
    }
    obs.indexes.insert(
        "runs_release".to_string(),
        RUNS_RELEASE_INDEX_DEF.to_string(),
    );
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

fn add_legacy_partition_plane(obs: &mut RunPlaneObservation) {
    obs.tables
        .get_mut("run_queue")
        .expect("record queue")
        .extend(["partition_key".to_string(), "partition_policy".to_string()]);
    obs.tables.insert(
        "partition_owner".to_string(),
        BTreeSet::from([
            "tenant_id".to_string(),
            "partition_key".to_string(),
            "lease_owner".to_string(),
            "lease_expires_at".to_string(),
            "acquired_at".to_string(),
        ]),
    );
    obs.tables.insert(
        "run_dead_letters".to_string(),
        BTreeSet::from([
            "tenant_id".to_string(),
            "run_id".to_string(),
            "partition_key".to_string(),
            "flow_id".to_string(),
            "reason".to_string(),
            "failed_at".to_string(),
        ]),
    );
    obs.checks.insert(
        ("run_queue".to_string(), RETIRED_PARTITION_CHECK.to_string()),
        "CHECK (partition_policy = ANY (ARRAY['blocking'::text, 'leapfrog'::text]))"
            .to_string(),
    );
    obs.indexes.insert(
        RETIRED_PARTITION_INDEX.to_string(),
        "CREATE INDEX run_queue_partition ON demo.run_queue USING btree \
             (tenant_id, partition_key) WHERE (partition_key IS NOT NULL)"
            .to_string(),
    );
    obs.indexes.insert(
        "run_queue_claimable".to_string(),
        "CREATE INDEX run_queue_claimable ON demo.run_queue USING btree \
             (tenant_id, available_at, stream_seq, lease_expires_at)"
            .to_string(),
    );
    add_legacy_flow_registry(obs);
}

/// The legacy flow registry (fixture-only), formerly `deploy/sql/flows.sql`,
/// was deleted by wamn-0h0g.12.102 (e45ca35b). It is no longer one of
/// `RUN_PLANE_FILES`, so an observation derived from the record no longer
/// carries it — but `partition_plane_cutover_sql` still locks and preflights
/// it for schemas that physically retain the table, so the fixture must
/// inject it.
fn add_legacy_flow_registry(obs: &mut RunPlaneObservation) {
    obs.tables.insert(
        "flows".to_string(),
        BTreeSet::from([
            "tenant_id".to_string(),
            "flow_id".to_string(),
            "version".to_string(),
            "active".to_string(),
            "graph_json".to_string(),
            "created_at".to_string(),
            "updated_at".to_string(),
        ]),
    );
}

fn add_legacy_child_run_state(obs: &mut RunPlaneObservation) {
    obs.tables
        .get_mut("runs")
        .expect("record runs")
        .extend(RETIRED_CHILD_RUN_COLUMNS.map(str::to_string));
    for (name, definition) in [
        (
            "runs_check3",
            "CHECK ((parent_run_id IS NULL) = (parent_node_id IS NULL) AND \
                 (parent_run_id IS NULL) = (parent_occurrence IS NULL))",
        ),
        (
            "runs_check4",
            "CHECK ((parent_run_id IS NULL) = (invoke_root_run_id IS NULL))",
        ),
        (
            "runs_check5",
            "CHECK ((waiting_child_run_id IS NULL) = (waiting_child_occurrence IS NULL) AND \
                 (waiting_child_run_id IS NULL) = (wait_generation IS NULL))",
        ),
        ("runs_invoke_depth_check", "CHECK (invoke_depth >= 0)"),
    ] {
        obs.checks.insert(
            ("runs".to_string(), name.to_string()),
            definition.to_string(),
        );
    }
    for index in RETIRED_CHILD_RUN_INDEXES {
        obs.indexes.insert(
            index.to_string(),
            format!("CREATE INDEX {index} ON demo.runs"),
        );
    }
}

#[test]
fn run_plane_record_tables_are_pinned() {
    assert_eq!(
        record_tables(RUN_STATE_SQL, "wamn_run"),
        [
            "environment_policies",
            "runs",
            "effect_attempts",
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
            "operator_run_actions",
        ]
    );
    assert_eq!(record_tables(RUN_QUEUE_SQL, "wamn_run"), ["run_queue"]);
}

#[test]
fn run_queue_record_columns_pin_the_global_fifo_shape() {
    let cols = record_columns(RUN_QUEUE_SQL, "wamn_run", "run_queue");
    let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        names,
        [
            "tenant_id",
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
            "tenant_id text NOT NULL CHECK (tenant_id <> '')",
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

#[test]
fn fresh_schema_omits_execution_bundle_carriers() {
    let runs = table_section(RUN_STATE_SQL, "wamn_run", "runs");
    for column in [
        "package_id      text NOT NULL",
        "effective_release_id int NOT NULL",
        "environment     text NOT NULL",
    ] {
        assert!(runs.contains(column), "runs contract missing {column}");
    }
    assert!(runs.contains("effective_release_id > 0"));
    assert!(runs.contains("CONSTRAINT runs_release_fk"));
    assert!(runs.contains(
        "FOREIGN KEY (tenant_id, effective_release_id)\n        REFERENCES catalog.effective_releases"
    ));
    assert!(runs.contains(
        "CREATE INDEX runs_release ON wamn_run.runs (tenant_id, effective_release_id)"
    ));
    assert!(RUN_STATE_SQL.contains("MESSAGE = 'run-admission-pin-immutable'"));
    assert!(!runs.contains(RETIRED_EXECUTION_BUNDLE_COLUMN));
    assert!(!CATALOG_SCHEMA_SQL.contains("catalog.execution_bundles"));

    // The claim-time manifest record is separate from the immutable
    // admission-pinned effective release id.
    assert!(!runs.contains("release_version int"));
    assert!(runs.contains("manifest_digest text"));
    assert!(runs.contains("CONSTRAINT runs_release_record_check"));
    assert!(runs.contains(
        "manifest_digest IS NULL\n      OR manifest_digest ~ '^sha256:[0-9a-f]{64}$'"
    ));
    // The durability-class carrier joins the same column-scoped guard
    // (wamn-0h0g.20.1 rider 1): a column the trigger does not NAME never
    // fires its transition arm, so the class would be silently mutable.
    for trigger_fragment in [
        "BEFORE UPDATE OF flow_id, flow_version, package_id, effective_release_id, environment,",
        "capture_mode, durability_class, wiring_id, wiring_version,",
        "wiring_hash, binding_world_json, manifest_digest",
    ] {
        assert!(
            RUN_STATE_SQL.contains(trigger_fragment),
            "{trigger_fragment}"
        );
    }
    assert!(runs.contains("durability_class text NOT NULL DEFAULT 'standard'"));
    assert!(runs.contains("CHECK (durability_class IN ('standard', 'durable'))"));
    assert!(RUN_STATE_SQL.contains("MESSAGE = 'run-release-record-immutable'"));
    assert!(!RUN_STATE_SQL.contains("OLD.release_version IS NOT NULL"));
    assert!(RUN_STATE_SQL.contains("OLD.manifest_digest IS NOT NULL"));

    assert!(!CATALOG_SCHEMA_SQL.contains(RETIRED_EXECUTION_BUNDLE_COLUMN));
}

#[test]
fn execution_bundle_retirement_locks_and_drops_direct_carriers_before_the_table() {
    let mut obs = RunPlaneObservation::default();
    obs.tables.insert(
        "runs".to_string(),
        BTreeSet::from([RETIRED_EXECUTION_BUNDLE_COLUMN.to_string()]),
    );
    obs.catalog_tables.insert("execution_bundles".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
    let action = &plan.actions[0];
    assert_eq!(action.kind, RunPlaneActionKind::RetireExecutionBundles);
    assert_eq!(
        action.sql,
        "LOCK TABLE \"demo\".runs IN ACCESS EXCLUSIVE MODE;\n\
             ALTER TABLE \"demo\".runs DROP COLUMN IF EXISTS \
             execution_bundle_hash RESTRICT;\n\
             LOCK TABLE catalog.execution_bundles IN ACCESS EXCLUSIVE MODE;\n\
             -- Persisted bundle bytes are deliberately discarded without archive.\n\
             DROP TABLE catalog.execution_bundles RESTRICT;"
    );
    assert!(!action.sql.contains("CASCADE"));
}

/// The multi-line `runs.status` CHECK parses whole (paren-depth), and
/// `fail_kind` — the fqg.16 sibling — is present as a column.
#[test]
fn multi_line_column_definitions_parse_whole() {
    let cols = record_columns(RUN_STATE_SQL, "wamn_run", "runs");
    let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
    assert!(names.contains(&"status"));
    assert!(names.contains(&"fail_kind"));
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
    let run_columns = record_columns(RUN_STATE_SQL, "wamn_run", "runs")
        .into_iter()
        .map(|(column, _)| column)
        .collect::<BTreeSet<_>>();
    assert!(run_columns.contains("fail_kind"));
    assert!(run_columns.contains("caller_outcome_kind"));
    for retired in RETIRED_FAILURE_DETAIL_COLUMNS {
        assert!(
            !run_columns.contains(*retired),
            "fresh runs record restored retired failure detail {retired}"
        );
        assert!(
            !RUNS_ADMISSION_PINS_TRIGGER_DEF.contains(retired),
            "write-once trigger unexpectedly names retired failure detail {retired}"
        );
        assert!(
            !RUNS_ADMISSION_PINS_TRIGGER_SQL.contains(retired),
            "trigger SQL unexpectedly names retired failure detail {retired}"
        );
    }

    let expected_fail_kind = "CHECK (fail_kind = ANY (ARRAY['terminal'::text, 'retry-exhausted'::text, 'invalid-input'::text, 'runaway-budget'::text, 'effect-uncertain'::text, 'depth-budget'::text, 'dispatch-budget'::text, 'unresolvable-name'::text, 'hash-invalid-bytes'::text, 'foreign-revision'::text, 'incompatible-contract'::text, 'unbound-requirement'::text]))";
    let fail_kind = CHECK_SPECS
        .iter()
        .find(|spec| spec.table == "runs" && spec.name == "runs_fail_kind_check")
        .expect("runs.fail_kind CHECK mirror exists");
    assert_eq!(fail_kind.definition, expected_fail_kind);

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
        .find(|spec| spec.table == "runs" && spec.name == "runs_caller_outcome_kind_check")
        .expect("runs.caller_outcome_kind CHECK mirror exists");
    assert_eq!(
        caller_outcome.definition,
        "CHECK (caller_outcome_kind = ANY (ARRAY['responded'::text, 'failed'::text]))"
    );
}

#[test]
fn operator_action_record_is_exact_immutable_and_history_independent() {
    let columns = record_columns(RUN_STATE_SQL, "wamn_run", "operator_run_actions");
    let names: Vec<&str> = columns.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "tenant_id",
            "action_id",
            "correlation_id",
            "run_id",
            "action_kind",
            "basis",
            "evidence_ref",
            "principal",
            "principal_kind",
            "prior_run_status",
            "prior_started_node_frame_id",
            "prior_started_node_local_node_id",
            "prior_started_node_occurrence",
            "prior_started_node_status",
            "created_at",
        ]
    );
    let actions = table_section(RUN_STATE_SQL, "wamn_run", "operator_run_actions");
    assert!(actions.contains("CONSTRAINT operator_run_actions_run_key"));
    assert!(actions.contains("UNIQUE (tenant_id, run_id)"));
    assert!(actions.contains("CONSTRAINT operator_run_actions_correlation_key"));
    assert!(actions.contains("UNIQUE (tenant_id, correlation_id)"));
    assert!(actions.contains("FORCE ROW LEVEL SECURITY"));
    assert!(actions.contains("operator_run_actions_update_immutable"));
    assert!(actions.contains("operator_run_actions_delete_immutable"));
    assert!(!actions.contains("REFERENCES"));
    assert!(!RUN_STATE_SQL.contains("CREATE TABLE wamn_run.effect_disposition_requests"));
    assert!(!RUN_STATE_SQL.contains("CREATE TABLE wamn_run.effect_dispositions"));
}

/// Sections carry the table's whole apparatus: indexes, RLS, policy, grant.
#[test]
fn table_sections_carry_indexes_rls_and_grants() {
    let rq = table_section(RUN_QUEUE_SQL, "wamn_run", "run_queue");
    assert!(rq.contains("CREATE INDEX run_queue_claimable"));
    assert!(rq.contains("available_at, stream_seq, run_id, lease_expires_at"));
    assert!(rq.contains("FORCE ROW LEVEL SECURITY"));
    assert!(rq.contains("FROM PUBLIC, wamn_app, wamn_effect_writer"));
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
    assert!(package.contains("manifest_sha256"));

    let actions = table_section(RUN_STATE_SQL, "wamn_run", "operator_run_actions");
    assert!(actions.contains("operator_run_actions_delete_immutable"));
    assert!(actions.contains("REVOKE ALL PRIVILEGES"));
    assert!(!actions.contains("REFERENCES"));

    let hdr = header_section(RUN_STATE_SQL, "wamn_run");
    assert!(hdr.contains("CREATE SCHEMA IF NOT EXISTS wamn_run"));
    assert!(hdr.contains("GRANT USAGE ON SCHEMA wamn_run TO wamn_app"));
}

#[test]
fn index_statements_are_pinned() {
    let mut names: Vec<String> = RUN_PLANE_FILES
        .iter()
        .flat_map(|f| index_statements(f, "wamn_run"))
        .map(|(n, _, _)| n)
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            // The tenant-key expression indexes ride the re-keyed
            // predicates (`wamn-0h0g.22.6.3`): one per guest-reachable
            // run-plane relation, and the predicate sequential-scans
            // without them. `run_queue` and `operator_run_actions` carry no
            // guest grant, so they keep their claim and gain no index.
            "effect_attempt_dispatches_tkey",
            "effect_attempt_outcomes_tkey",
            "effect_attempts_bulk_scope",
            "effect_attempts_tkey",
            "environment_policies_tkey",
            "run_queue_claimable",
            "runs_event_root",
            "runs_flow",
            "runs_idempotency",
            "runs_release",
            "runs_response_deadline",
            "runs_run_deadline",
            "runs_tkey",
        ]
    );
    let (_, table, stmt) = index_statements(RUN_QUEUE_SQL, "wamn_run")
        .into_iter()
        .find(|(n, _, _)| n == "run_queue_claimable")
        .unwrap();
    assert_eq!(table, "run_queue");
    assert_eq!(
        stmt,
        "CREATE INDEX run_queue_claimable ON wamn_run.run_queue \
             (tenant_id, available_at, stream_seq, run_id, lease_expires_at)"
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
        7,
        "all seven retained run-plane tables are at target"
    );
}

#[test]
fn child_run_cutover_is_atomic_exact_and_idempotent() {
    let mut legacy = observation_at_record();
    add_legacy_child_run_state(&mut legacy);

    let plan = plan_run_plane(&schema("demo"), &legacy);
    assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
    let cutover = &plan.actions[0];
    assert_eq!(cutover.kind, RunPlaneActionKind::ChildRunCutover);
    assert_eq!(cutover.target, "runs.durable-child-state");
    assert!(cutover.sql.starts_with(
        "LOCK TABLE \"demo\".runs IN ACCESS EXCLUSIVE MODE; DO $child_run_cutover$"
    ));
    let refusal = cutover.sql.find("RAISE EXCEPTION").expect("refusal");
    let first_drop = cutover.sql.find("DROP INDEX").expect("first DDL");
    assert!(refusal < first_drop, "refusal must precede every DDL");
    assert!(
        cutover
            .sql
            .contains("durable-child-run-cutover-requires-no-child-or-wait-state")
    );
    for column in RETIRED_CHILD_RUN_COLUMNS {
        assert!(
            cutover
                .sql
                .contains(&format!("DROP COLUMN IF EXISTS \"{column}\"")),
            "missing retired column drop: {column}"
        );
    }
    for index in RETIRED_CHILD_RUN_INDEXES {
        assert!(
            cutover
                .sql
                .contains(&format!("DROP INDEX IF EXISTS \"demo\".\"{index}\"")),
            "missing retired index drop: {index}"
        );
    }
    for forbidden in ["CASCADE", "BEGIN;", "COMMIT;"] {
        assert!(
            !cutover.sql.contains(forbidden),
            "unsafe cutover SQL: {forbidden}"
        );
    }
    assert!(plan.extra_columns.is_empty());

    let current = observation_at_record();
    assert!(
        !plan_run_plane(&schema("demo"), &current)
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::ChildRunCutover)
    );
}

#[test]
fn failure_detail_cutover_handles_each_partial_shape_exactly_once() {
    for legacy_columns in [
        &["fail_node"][..],
        &["fail_reason"][..],
        &["fail_node", "fail_reason"][..],
    ] {
        let mut legacy = observation_at_record();
        legacy
            .tables
            .get_mut("runs")
            .expect("record runs")
            .extend(legacy_columns.iter().map(|column| (*column).to_string()));

        let plan = plan_run_plane(&schema("demo"), &legacy);
        assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
        let cutover = &plan.actions[0];
        assert_eq!(cutover.kind, RunPlaneActionKind::FailureDetailCutover);
        assert_eq!(cutover.target, "runs.failure-detail");
        assert!(
            cutover
                .sql
                .starts_with("LOCK TABLE \"demo\".runs IN ACCESS EXCLUSIVE MODE;")
        );
        assert!(cutover.sql.contains(
            "populated retired failure-detail values are\n-- deliberately discarded, not archived"
        ));
        assert!(
            cutover
                .sql
                .contains("DROP COLUMN IF EXISTS fail_node RESTRICT")
        );
        assert!(
            cutover
                .sql
                .contains("DROP COLUMN IF EXISTS fail_reason RESTRICT")
        );
        assert_eq!(cutover.sql.matches("DROP COLUMN IF EXISTS").count(), 2);
        for forbidden in ["CASCADE", "IS NOT NULL", "SELECT ", "BEGIN;", "COMMIT;"] {
            assert!(
                !cutover.sql.contains(forbidden),
                "unsafe failure-detail cutover SQL {forbidden:?}: {}",
                cutover.sql
            );
        }
        assert!(plan.extra_columns.is_empty());
    }

    assert!(
        !plan_run_plane(&schema("demo"), &observation_at_record())
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FailureDetailCutover)
    );
}

#[test]
fn failure_detail_cutover_leads_without_poisoning_following_acl_repairs() {
    let mut legacy = observation_at_record();
    legacy.tables.get_mut("runs").expect("record runs").extend(
        RETIRED_FAILURE_DETAIL_COLUMNS
            .iter()
            .map(|column| (*column).to_string()),
    );
    legacy.app_run_capture_privileges.0 = true;
    legacy.effect_writer_run_table_privileges.insert(
        "runs".to_string(),
        ["SELECT".to_string()].into_iter().collect(),
    );

    let plan = plan_run_plane(&schema("demo"), &legacy);
    assert_eq!(
        plan.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FailureDetailCutover),
        "actions: {:#?}",
        plan.actions
    );
    for kind in [
        RunPlaneActionKind::RepairRunCapturePrivilege,
        RunPlaneActionKind::RepairEffectWriterPrivilege,
    ] {
        let repair = plan
            .actions
            .iter()
            .find(|action| action.kind == kind && action.target.contains("runs"))
            .unwrap_or_else(|| panic!("missing {kind:?} after cutover"));
        for retired in RETIRED_FAILURE_DETAIL_COLUMNS {
            assert!(
                !repair.sql.contains(retired),
                "post-cutover {kind:?} still names {retired}: {}",
                repair.sql
            );
        }
    }
}

#[test]
fn rerun_lineage_cutover_is_row_preserving_exact_and_idempotent() {
    let mut legacy = observation_at_record();
    legacy.tables.get_mut("runs").expect("record runs").extend(
        RETIRED_RERUN_LINEAGE_COLUMNS
            .iter()
            .map(|column| (*column).to_string()),
    );
    legacy.indexes.insert(
        "runs_root".to_string(),
        rewrite_schema(RUNS_ROOT_INDEX_DEF, &schema("demo")),
    );

    let plan = plan_run_plane(&schema("demo"), &legacy);
    assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
    let cutover = &plan.actions[0];
    assert_eq!(cutover.kind, RunPlaneActionKind::RerunLineageCutover);
    assert_eq!(cutover.target, "runs.rerun-lineage");
    assert!(
        cutover
            .sql
            .starts_with("LOCK TABLE \"demo\".runs IN ACCESS EXCLUSIVE MODE;")
    );
    assert!(cutover.sql.contains(
        "expected_definition constant text := 'CREATE INDEX runs_root ON demo.runs USING btree (tenant_id, root_run_id) WHERE (root_run_id IS NOT NULL)'"
    ));
    assert!(cutover.sql.contains(
        "observed_definition IS NOT NULL\n       AND observed_definition <> expected_definition"
    ));
    let guard = cutover
        .sql
        .find("rerun-lineage-cutover-refuses-unknown-runs-root")
        .expect("exact-index refusal");
    let first_drop = cutover
        .sql
        .find("DROP INDEX IF EXISTS")
        .expect("drop index");
    assert!(guard < first_drop, "guard precedes destructive DDL");
    assert!(cutover.sql.contains("DROP COLUMN IF EXISTS replay_of"));
    assert!(cutover.sql.contains("DROP COLUMN IF EXISTS root_run_id"));
    for retained in ["event_source_run_id", "event_root_run_id", "event_depth"] {
        assert!(
            !cutover.sql.contains(retained),
            "cutover must not name retained event causation: {retained}"
        );
    }
    assert!(!cutover.sql.contains("CASCADE"));
    assert!(plan.extra_columns.is_empty());

    assert!(
        !plan_run_plane(&schema("demo"), &observation_at_record())
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::RerunLineageCutover)
    );
}

#[test]
fn rerun_lineage_cutover_refuses_an_unknown_same_name_index_before_ddl() {
    let mut legacy = observation_at_record();
    legacy.indexes.insert(
        "runs_root".to_string(),
        "CREATE INDEX runs_root ON demo.runs USING btree (tenant_id, flow_id)".to_string(),
    );

    let plan = plan_run_plane(&schema("demo"), &legacy);
    let cutover = plan
        .actions
        .first()
        .expect("same-name index requires guarded cutover");
    assert_eq!(cutover.kind, RunPlaneActionKind::RerunLineageCutover);
    let refusal = cutover.sql.find("RAISE EXCEPTION").expect("refusal");
    let destructive = cutover.sql.find("DROP INDEX IF EXISTS").expect("DDL");
    assert!(refusal < destructive);
    assert!(plan.extra_columns.is_empty());
}

#[test]
fn stored_suite_cutover_is_child_first_exact_and_idempotent() {
    let mut legacy = observation_at_record();
    for table in RETIRED_STORED_SUITE_TABLES {
        legacy.tables.insert(table.to_string(), BTreeSet::new());
    }
    for function in RETIRED_STORED_SUITE_FUNCTIONS {
        legacy
            .helper_functions
            .insert(function.to_string(), "legacy".to_string());
    }

    let plan = plan_run_plane(&schema("demo"), &legacy);
    assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
    let cutover = &plan.actions[0];
    assert_eq!(cutover.kind, RunPlaneActionKind::StoredSuiteCutover);
    assert_eq!(cutover.target, "stored-suite-persistence");
    assert_eq!(
        cutover.sql,
        "DROP TABLE IF EXISTS \"demo\".\"authoring_suite_reports\"; \
             DROP TABLE IF EXISTS \"demo\".\"authoring_suite_case_facts\"; \
             DROP TABLE IF EXISTS \"demo\".\"authoring_report_reservations\"; \
             DROP TABLE IF EXISTS \"demo\".\"test_cases\"; \
             DROP TABLE IF EXISTS \"demo\".\"test_suites\"; \
             DROP TABLE IF EXISTS \"demo\".\"authoring_test_sets\"; \
             DROP FUNCTION IF EXISTS \"demo\".\"guard_authoring_report_write\"(); \
             DROP FUNCTION IF EXISTS \"demo\".\"reject_immutable_authoring_report_change\"(); \
             DROP FUNCTION IF EXISTS \"demo\".\"reject_immutable_authoring_test_set_change\"(); \
             DROP TABLE IF EXISTS catalog.\"publish_gate_audit\""
    );
    for retained in [
        "authoring_test_run_reservations",
        "authoring_test_case_runs",
        "authoring_test_reports",
    ] {
        assert!(!cutover.sql.contains(retained), "cutover drops {retained}");
    }

    for table in RETIRED_STORED_SUITE_TABLES {
        legacy.tables.remove(table);
    }
    for function in RETIRED_STORED_SUITE_FUNCTIONS {
        legacy.helper_functions.remove(function);
    }
    assert!(plan_run_plane(&schema("demo"), &legacy).is_noop());
}

/// A schema provisioned before wamn-0h0g.15.27 still carries the FK columns
/// that reference `authoring_test_sets`. Nothing else in the planner removes
/// them — the FK reconciler repairs a fixed record list and has no
/// drop-extra arm — so the cutover must, and must do it BEFORE the parent
/// drop or the `DROP TABLE` refuses on the dependency.
#[test]
fn the_test_set_cutover_drops_its_fk_columns_before_the_parent_table() {
    let mut legacy = observation_at_record();
    legacy
        .tables
        .insert("authoring_test_sets".to_string(), BTreeSet::new());
    for table in RETIRED_TEST_SET_REFERENCE_TABLES {
        legacy
            .tables
            .entry(table.to_string())
            .or_default()
            .insert(RETIRED_TEST_SET_REFERENCE_COLUMN.to_string());
    }

    let plan = plan_run_plane(&schema("demo"), &legacy);
    let cutover = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::StoredSuiteCutover)
        .expect("a stale test-set store plans its cutover");
    let reservations = cutover
        .sql
        .find(
            "ALTER TABLE \"demo\".\"authoring_test_run_reservations\" \
                 DROP COLUMN IF EXISTS \"test_set_hash\"",
        )
        .expect("the reservation FK column drops");
    let reports = cutover
        .sql
        .find(
            "ALTER TABLE \"demo\".\"authoring_test_reports\" \
                 DROP COLUMN IF EXISTS \"test_set_hash\"",
        )
        .expect("the report FK column drops");
    let parent = cutover
        .sql
        .find("DROP TABLE IF EXISTS \"demo\".\"authoring_test_sets\"")
        .expect("the parent store drops");
    let helper = cutover
        .sql
        .find(
            "DROP FUNCTION IF EXISTS \
                 \"demo\".\"reject_immutable_authoring_test_set_change\"()",
        )
        .expect("the immutability helper drops");
    assert!(reservations < parent && reports < parent);
    assert!(parent < helper, "the triggers die with their table first");
    // The columns are cutover-owned, so they are physically removed rather
    // than reported and preserved as unknown extras.
    assert!(
        !plan.extra_columns.iter().any(|(table, column)| {
            column == RETIRED_TEST_SET_REFERENCE_COLUMN
                && RETIRED_TEST_SET_REFERENCE_TABLES.contains(&table.as_str())
        }),
        "extras: {:#?}",
        plan.extra_columns
    );

    legacy.tables.remove("authoring_test_sets");
    for table in RETIRED_TEST_SET_REFERENCE_TABLES {
        legacy
            .tables
            .get_mut(table)
            .expect("legacy table was inserted above")
            .remove(RETIRED_TEST_SET_REFERENCE_COLUMN);
    }
    assert!(plan_run_plane(&schema("demo"), &legacy).is_noop());
}

#[test]
fn orphaned_publish_gate_audit_independently_plans_the_cutover() {
    let mut legacy = observation_at_record();
    legacy
        .catalog_tables
        .insert(RETIRED_STORED_SUITE_CATALOG_TABLE.to_string());

    let plan = plan_run_plane(&schema("demo"), &legacy);
    let cutovers = plan
        .actions
        .iter()
        .filter(|action| action.kind == RunPlaneActionKind::StoredSuiteCutover)
        .collect::<Vec<_>>();
    assert_eq!(cutovers.len(), 1, "actions: {:#?}", plan.actions);
    assert_eq!(
        cutovers[0].sql,
        "DROP TABLE IF EXISTS \"demo\".\"authoring_suite_reports\"; \
             DROP TABLE IF EXISTS \"demo\".\"authoring_suite_case_facts\"; \
             DROP TABLE IF EXISTS \"demo\".\"authoring_report_reservations\"; \
             DROP TABLE IF EXISTS \"demo\".\"test_cases\"; \
             DROP TABLE IF EXISTS \"demo\".\"test_suites\"; \
             DROP TABLE IF EXISTS \"demo\".\"authoring_test_sets\"; \
             DROP FUNCTION IF EXISTS \"demo\".\"guard_authoring_report_write\"(); \
             DROP FUNCTION IF EXISTS \"demo\".\"reject_immutable_authoring_report_change\"(); \
             DROP FUNCTION IF EXISTS \"demo\".\"reject_immutable_authoring_test_set_change\"(); \
             DROP TABLE IF EXISTS catalog.\"publish_gate_audit\""
    );

    legacy
        .catalog_tables
        .remove(RETIRED_STORED_SUITE_CATALOG_TABLE);
    assert!(plan_run_plane(&schema("demo"), &legacy).is_noop());
}

#[test]
fn retired_legacy_admission_surface_is_dropped_exactly_once() {
    let mut legacy = observation_at_record();
    legacy.tables.insert(
        "invocation_admissions".to_string(),
        BTreeSet::from([
            "tenant_id".to_string(),
            "run_id".to_string(),
            "client_key_digest".to_string(),
        ]),
    );
    legacy.helper_functions.insert(
        "lock_catalog_head".to_string(),
        "legacy security definer".to_string(),
    );

    let plan = plan_run_plane(&schema("demo"), &legacy);
    let cutovers = plan
        .actions
        .iter()
        .filter(|action| action.kind == RunPlaneActionKind::RetireLegacyAdmissionSurface)
        .collect::<Vec<_>>();
    assert_eq!(cutovers.len(), 1, "actions: {:#?}", plan.actions);
    assert_eq!(
        cutovers[0].sql,
        "DROP TABLE \"demo\".invocation_admissions RESTRICT; \
             DROP FUNCTION \"demo\".lock_catalog_head(text, text, text) RESTRICT"
    );

    legacy.tables.remove("invocation_admissions");
    legacy.helper_functions.remove("lock_catalog_head");
    let at_target = plan_run_plane(&schema("demo"), &legacy);
    assert!(
        !at_target
            .actions
            .iter()
            .any(|action| { action.kind == RunPlaneActionKind::RetireLegacyAdmissionSurface }),
        "at-target schema repeated retirement: {:#?}",
        at_target.actions
    );
}

/// Why the cutover's ADD carries the inline CHECK rather than being bare.
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
            "effective_releases".to_string(),
            SCENARIO_AUTHOR_ROLE.to_string(),
        ))
        .or_default()
        .insert("UPDATE".to_string());
    obs.authoring_effective_column_privileges
        .entry((
            "catalog".to_string(),
            "effective_release_heads".to_string(),
            "wamn_app".to_string(),
        ))
        .or_default()
        .insert("UPDATE".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    for table in [
        "packages",
        "connection_bindings",
        "effective_releases",
        "effective_release_heads",
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
fn missing_environment_policy_tenant_policy_mutant_plans_repair() {
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
            .contains("CREATE POLICY environment_policies_tenant")
    );
}

#[test]
fn widened_environment_policy_tenant_policy_mutant_plans_exact_replacement() {
    let mut obs = observation_at_record();
    let row_security = obs
        .environment_policy_row_security
        .as_mut()
        .expect("record relation");
    let policy = row_security
        .policies
        .get_mut("environment_policies_tenant")
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

#[test]
fn environment_policy_writer_grants_are_revoked_and_refused_effectively() {
    let mut obs = observation_at_record();
    let key = (
        "demo".to_string(),
        "environment_policies".to_string(),
        EFFECT_WRITER_ROLE.to_string(),
    );
    obs.authoring_table_privileges
        .insert(key.clone(), BTreeSet::from(["UPDATE".to_string()]));
    obs.authoring_effective_table_privileges
        .insert(key.clone(), BTreeSet::from(["UPDATE".to_string()]));
    obs.authoring_effective_column_privileges
        .insert(key, BTreeSet::from(["UPDATE".to_string()]));

    let plan = plan_run_plane(&schema("demo"), &obs);
    let repair = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairAuthoringPrivilege
                && action.target == "demo.environment_policies"
        })
        .expect("policy writer drift must be repaired");
    assert!(repair.sql.contains(
        "REVOKE ALL PRIVILEGES ON TABLE \"demo\".\"environment_policies\" FROM wamn_effect_writer"
    ));
    assert!(
        repair
            .sql
            .contains("'wamn_effect_writer', '\"demo\".\"environment_policies\"', 'UPDATE'")
    );
    assert!(
        repair
            .sql
            .contains("authoring-effective-privilege-out-of-bounds")
    );
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

/// A complete legacy partition plane is removed by one leading locked
/// cutover; the generic drift planner must not duplicate any owned drop or
/// claim-index repair.
#[test]
fn legacy_partition_plane_plans_one_leading_cutover() {
    let mut obs = observation_at_record();
    add_legacy_partition_plane(&mut obs);
    // Keep the independent outbox teardown and registration cleanup in the
    // same observation to pin their ordering after the leading cutover.
    obs.tables
        .insert("outbox".into(), BTreeSet::from(["id".into()]));
    obs.tables
        .insert("evt_shadow".into(), BTreeSet::from(["id".into()]));
    obs.outbox_trigger_tables = vec!["receipts".into()];
    obs.outbox_function_present = true;
    obs.stale_registration_key_rows = 2;

    let plan = plan_run_plane(&schema("demo"), &obs);
    let sqls: Vec<&str> = plan.actions.iter().map(|a| a.sql.as_str()).collect();
    let kinds: Vec<RunPlaneActionKind> = plan.actions.iter().map(|a| a.kind).collect();
    let cutover = plan.actions.first().expect("partition cutover action");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    assert_eq!(cutover.target, "run_queue.partition-plane");
    assert!(cutover.sql.starts_with(
        "LOCK TABLE \"demo\".\"run_queue\", \"demo\".\"partition_owner\", \
             \"demo\".\"run_dead_letters\", \"demo\".\"flows\" IN ACCESS EXCLUSIVE MODE"
    ));
    assert!(cutover.sql.contains("graph_json ? 'ordering'"));
    assert!(cutover.sql.contains("graph_json ? 'partition-policy'"));
    assert!(
        cutover
            .sql
            .contains("lease_owner IS NOT NULL AND lease_expires_at > clock_timestamp()")
    );
    assert!(
        cutover
            .sql
            .contains("partition-plane-cutover-requires-drained-workers")
    );
    assert!(
        cutover
            .sql
            .contains("IF EXISTS (SELECT 1 FROM \"demo\".run_dead_letters)")
    );
    assert!(cutover.sql.contains(RETIRED_DEAD_LETTER_REFUSAL));
    assert!(
        cutover
            .sql
            .contains("DROP INDEX IF EXISTS \"demo\".run_queue_partition")
    );
    assert!(
        cutover
            .sql
            .contains("DROP CONSTRAINT IF EXISTS run_queue_partition_policy_check")
    );
    assert!(cutover.sql.contains("DROP COLUMN IF EXISTS partition_key"));
    assert!(
        cutover
            .sql
            .contains("DROP COLUMN IF EXISTS partition_policy")
    );
    assert!(
        cutover
            .sql
            .contains("DROP TABLE IF EXISTS \"demo\".\"partition_owner\"")
    );
    assert!(
        cutover
            .sql
            .contains("DROP TABLE IF EXISTS \"demo\".\"run_dead_letters\"")
    );
    assert!(!cutover.sql.contains("CASCADE"));
    assert!(cutover.sql.contains(
        "CREATE INDEX run_queue_claimable ON \"demo\".run_queue \
             (tenant_id, available_at, stream_seq, run_id, lease_expires_at)"
    ));
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == RunPlaneActionKind::PartitionPlaneCutover)
            .count(),
        1
    );
    assert!(!plan.actions.iter().any(|action| {
        matches!(
            action.kind,
            RunPlaneActionKind::CreateIndex
                | RunPlaneActionKind::RecreateIndex
                | RunPlaneActionKind::DropExtraConstraint
        ) && matches!(
            action.target.as_str(),
            "run_queue_claimable" | "run_queue.run_queue_partition_policy_check"
        )
    }));
    assert!(!plan.extra_columns.iter().any(|(table, column)| {
        table == "run_queue" && RETIRED_PARTITION_COLUMNS.contains(&column.as_str())
    }));
    assert!(sqls.contains(&"DROP TABLE IF EXISTS \"demo\".\"outbox\""));
    assert!(sqls.contains(&"DROP TABLE IF EXISTS \"demo\".\"evt_shadow\""));
    assert!(
        sqls.contains(&"DROP TRIGGER IF EXISTS wamn_outbox_event ON \"demo\".\"receipts\"")
    );
    assert!(sqls.contains(&"DROP FUNCTION IF EXISTS \"demo\".wamn_outbox_event()"));
    // Trigger drops precede the RESTRICT function drop.
    let trig = kinds
        .iter()
        .position(|k| *k == RunPlaneActionKind::DropLegacyTrigger)
        .unwrap();
    let func = kinds
        .iter()
        .position(|k| *k == RunPlaneActionKind::DropLegacyFunction)
        .unwrap();
    assert!(trig < func);
    assert!(sqls.contains(&strip_retired_registration_keys_sql()));
}

#[test]
fn persisted_retired_flow_ordering_is_a_leading_reprovision_refusal() {
    let mut obs = observation_at_record();
    add_legacy_flow_registry(&mut obs);
    obs.retired_authored_ordering_rows = 2;

    let plan = plan_run_plane(&schema("demo"), &obs);
    let cutover = plan.actions.first().expect("authored ordering cutover");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    assert!(cutover.sql.starts_with(
        "LOCK TABLE \"demo\".\"run_queue\", \"demo\".\"flows\" IN ACCESS EXCLUSIVE MODE"
    ));
    let refusal = cutover
        .sql
        .find(RETIRED_AUTHORED_ORDERING_REFUSAL)
        .expect("exact reprovision refusal");
    let first_ddl = cutover
        .sql
        .find("DROP INDEX")
        .expect("partition cutover DDL follows preflights");
    assert!(refusal < first_ddl);
    assert!(cutover.sql.contains("ERRCODE = '55000'"));
    assert!(cutover.sql.contains("graph_json ? 'ordering'"));
    assert!(cutover.sql.contains("graph_json ? 'partition-policy'"));
    assert!(
        count_retired_authored_ordering_rows_sql(&schema("demo"))
            .contains("WHERE graph_json ? 'ordering' OR graph_json ? 'partition-policy'")
    );

    obs.retired_authored_ordering_rows = 0;
    assert!(plan_run_plane(&schema("demo"), &obs).is_noop());
}

/// The leading cutover must remain executable against a partially
/// converged queue. A missing retained claim-index column is added later by
/// the generic planner, which then owns the final index recreation.
#[test]
fn partial_partition_plane_defers_claim_index_until_columns_exist() {
    let mut obs = observation_at_record();
    add_legacy_partition_plane(&mut obs);
    obs.tables
        .get_mut("run_queue")
        .expect("record queue")
        .remove("stream_seq");

    let plan = plan_run_plane(&schema("demo"), &obs);
    let cutover = plan.actions.first().expect("partition cutover action");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    assert!(!cutover.sql.contains("run_queue_claimable"));
    assert!(plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::AddColumn && action.target == "run_queue.stream_seq"
    }));
    let recreate = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RecreateIndex
                && action.target == "run_queue_claimable"
        })
        .expect("claimable index repair follows the retained column add");
    assert!(recreate.sql.contains(
        "CREATE INDEX run_queue_claimable ON demo.run_queue \
             (tenant_id, available_at, stream_seq, run_id, lease_expires_at)"
    ));
}

#[test]
fn partial_partition_plane_requires_unobservable_lease_tables_to_be_empty() {
    let mut obs = observation_at_record();
    add_legacy_partition_plane(&mut obs);
    obs.tables
        .get_mut("run_queue")
        .expect("record queue")
        .remove("lease_owner");
    obs.tables
        .get_mut("partition_owner")
        .expect("legacy owner table")
        .remove("lease_expires_at");

    let cutover = plan_run_plane(&schema("demo"), &obs)
        .actions
        .into_iter()
        .next()
        .expect("partition cutover action");
    assert_eq!(cutover.kind, RunPlaneActionKind::PartitionPlaneCutover);
    assert!(!cutover.sql.contains("clock_timestamp()"));
    assert!(cutover.sql.contains(
        "partition-plane-cutover-requires-observable-run-queue-leases-or-empty-queue"
    ));
    assert!(cutover.sql.contains(
        "partition-plane-cutover-requires-observable-partition-leases-or-empty-owner-table"
    ));
    assert!(
        cutover
            .sql
            .contains("DROP TABLE IF EXISTS \"demo\".\"partition_owner\"")
    );
}

#[test]
fn retired_effect_disposition_cutover_is_locked_empty_only_and_idempotent() {
    let mut obs = observation_at_record();
    obs.tables.insert(
        "effect_disposition_requests".into(),
        ["tenant_id".into(), "request_id".into()].into(),
    );
    obs.tables.insert(
        "effect_dispositions".into(),
        ["tenant_id".into(), "disposition_id".into()].into(),
    );
    obs.helper_functions.insert(
        "guard_effect_disposition_append".into(),
        "CREATE OR REPLACE FUNCTION demo.guard_effect_disposition_append()".into(),
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    let cutover = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::RetiredEffectDispositionCutover)
        .expect("retired disposition persistence is cut over atomically");
    let child_lock = cutover
        .sql
        .find("\"demo\".\"effect_dispositions\"")
        .expect("child is locked");
    let parent_lock = cutover
        .sql
        .find("\"demo\".\"effect_disposition_requests\"")
        .expect("parent is locked");
    assert!(child_lock < parent_lock, "child lock precedes parent lock");
    assert!(cutover.sql.contains(
        "retired-effect-disposition-history-requires-archive-or-environment-reprovision"
    ));
    assert!(cutover.sql.contains("ERRCODE = '55000'"));
    let child_drop = cutover
        .sql
        .find("DROP TABLE IF EXISTS \"demo\".effect_dispositions")
        .expect("child is dropped");
    let parent_drop = cutover
        .sql
        .find("DROP TABLE IF EXISTS \"demo\".effect_disposition_requests")
        .expect("parent is dropped");
    assert!(child_drop < parent_drop, "child drop precedes parent drop");
    assert!(
        cutover
            .sql
            .contains("DROP FUNCTION IF EXISTS \"demo\".guard_effect_disposition_append()")
    );

    assert!(
        !plan_run_plane(&schema("demo"), &observation_at_record())
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::RetiredEffectDispositionCutover)
    );
}

#[test]
fn empty_incompatible_effect_writer_shape_is_physically_retired() {
    let mut obs = observation_at_record();
    let attempt_columns = obs
        .tables
        .get_mut("effect_attempts")
        .expect("attempt table");
    for column in RETIRED_EFFECT_ATTEMPT_COLUMNS {
        attempt_columns.insert((*column).to_string());
    }
    for (table, column) in [
        ("effect_attempts", "attempt_index"),
        ("effect_attempts", "legacy_imported"),
    ] {
        obs.non_nullable_columns
            .insert((table.to_string(), column.to_string()));
        obs.defaulted_columns
            .insert((table.to_string(), column.to_string()));
    }
    obs.indexes.insert(
        "effect_attempts_occurrence".to_string(),
        "CREATE INDEX effect_attempts_occurrence ON wamn_run.effect_attempts USING btree (tenant_id, run_id, frame_id, local_node_id, occurrence, attempt_index)".to_string(),
    );
    obs.indexes.remove("effect_attempts_occurrence_key");
    obs.indexes.insert(
        postgres_visible_identifier(
            "effect_attempts_tenant_id_attempt_id_run_id_node_id_occurrence_key",
        )
        .to_string(),
        "CREATE UNIQUE INDEX effect_attempts_tenant_id_attempt_id_run_id_node_id_occurrence_key ON wamn_run.effect_attempts USING btree (tenant_id, attempt_id, run_id, frame_id, local_node_id, occurrence)".to_string(),
    );
    obs.indexes.insert(
        postgres_visible_identifier(
            "effect_attempts_tenant_id_run_id_node_id_occurrence_attempt_index_key",
        )
        .to_string(),
        "CREATE UNIQUE INDEX effect_attempts_tenant_id_run_id_node_id_occurrence_attempt_index_key ON wamn_run.effect_attempts USING btree (tenant_id, run_id, frame_id, local_node_id, occurrence, attempt_index)".to_string(),
    );
    obs.foreign_keys.insert(
        (
            "effect_attempts".to_string(),
            "effect_attempts_predecessor_fk".to_string(),
        ),
        "legacy predecessor".to_string(),
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover),
        "retired index names must not masquerade as legacy indexed columns"
    );
    let action = plan
        .actions
        .into_iter()
        .find(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("effect writer cutover");
    assert!(
        action
            .sql
            .contains("effect-writer-cutover-requires-empty-ledger")
    );
    assert!(
        action
            .sql
            .contains("DROP CONSTRAINT IF EXISTS effect_attempts_key_check")
    );
    assert!(action.sql.contains(r#"DROP COLUMN "attempt_index""#));
    assert!(!action.sql.contains("UPDATE "));
    assert!(!action.sql.contains("DELETE "));
    assert!(!action.sql.contains("INSERT INTO "));
}

// `populated_current_ledgers_do_not_block_projection_only_cleanup` was
// retired here: its subject was the node-runs half of the effect-writer
// cutover, which wamn-0h0g.26.3.1 (204220e8) deleted along with the
// projection. An observed `node_runs` now plans one leading `RetireNodeRuns`
// and returns, so no effect-writer cutover can name it.

#[test]
fn dispatch_type_and_index_drift_is_replaced_by_empty_cutover() {
    let mut obs = observation_at_record();
    obs.column_types.insert(
        (
            "effect_attempt_dispatches".to_string(),
            "frame_id".to_string(),
        ),
        "text".to_string(),
    );
    obs.indexes.insert(
        "effect_attempt_dispatches_occurrence_key".to_string(),
        "CREATE INDEX effect_attempt_dispatches_occurrence_key ON demo.effect_attempt_dispatches USING btree (tenant_id, attempt_id)".to_string(),
    );
    obs.indexes.insert(
        "effect_attempts_dispatch_identity_key".to_string(),
        "CREATE INDEX effect_attempts_dispatch_identity_key ON demo.effect_attempts USING btree (tenant_id, attempt_id)".to_string(),
    );

    let action = plan_run_plane(&schema("demo"), &obs)
        .actions
        .into_iter()
        .find(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("dispatch drift cutover");
    assert!(action.sql.contains("DROP COLUMN IF EXISTS frame_id"));
    assert!(action.sql.contains("ADD COLUMN frame_id bigint NOT NULL"));
    assert!(
        action
            .sql
            .contains("DROP INDEX IF EXISTS \"demo\".effect_attempt_dispatches_occurrence_key")
    );
    assert!(
        action
            .sql
            .contains("DROP INDEX IF EXISTS \"demo\".effect_attempts_dispatch_identity_key")
    );
}

#[test]
fn partial_dispatch_cutover_repairs_attempt_fk_after_creating_peer() {
    let mut obs = observation_at_record();
    obs.tables.remove("effect_attempts");
    obs.indexes.remove("effect_attempts_occurrence_key");
    obs.indexes.remove("effect_attempts_dispatch_identity_key");
    obs.defaulted_columns.remove(&(
        "effect_attempts".to_string(),
        "attempt_started_at".to_string(),
    ));
    obs.foreign_keys.remove(&(
        "effect_attempt_dispatches".to_string(),
        EFFECT_DISPATCH_ATTEMPT_FK_NAME.to_string(),
    ));

    let plan = plan_run_plane(&schema("demo"), &obs);
    let create_position = plan
        .actions
        .iter()
        .position(|action| {
            action.kind == RunPlaneActionKind::CreateTable && action.target == "effect_attempts"
        })
        .expect("missing attempt peer is created");
    let fk_position = plan
        .actions
        .iter()
        .position(|action| {
            action.kind == RunPlaneActionKind::RepairForeignKey
                && action.target
                    == "effect_attempt_dispatches.effect_attempt_dispatches_attempt_fk"
        })
        .expect("dispatch FK is repaired in the same plan");
    assert!(create_position < fk_position);
}

// `check_and_index_only_attempt_residue_still_retires` was retired here: the
// one surviving leg named a stray `node_runs` CHECK, and after
// wamn-0h0g.26.3.1 (204220e8) a CHECK on `node_runs` can only exist while the
// table does, which plans `RetireNodeRuns` and drops both together.
// `extra_record_check_is_removed_but_floor_check_is_untouched` keeps the
// independent-drop test on a live record table.

#[test]
fn drifted_occurrence_key_is_replaced_by_frame_identity_cutover() {
    let mut obs = observation_at_record();
    obs.indexes.insert(
        "effect_attempts_occurrence_key".to_string(),
        "CREATE INDEX effect_attempts_occurrence_key ON demo.effect_attempts USING btree (tenant_id, run_id, node_id, occurrence)".to_string(),
    );

    let action = plan_run_plane(&schema("demo"), &obs)
        .actions
        .into_iter()
        .find(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
        .expect("drifted occurrence identity plans frame cutover");
    assert!(
        action
            .sql
            .contains("DROP INDEX IF EXISTS \"demo\".effect_attempts_occurrence_key")
    );
    assert!(action.sql.contains(
        "ADD CONSTRAINT effect_attempts_occurrence_key\n        UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence)"
    ));
}

#[test]
fn frame_identity_cutover_is_empty_only_and_precedes_ddl() {
    let mut obs = observation_at_record();
    for column in EFFECT_FRAME_COLUMNS {
        obs.tables
            .get_mut("effect_attempts")
            .expect("attempt table")
            .remove(*column);
    }
    obs.indexes.insert(
        "effect_attempts_occurrence_key".to_string(),
        "CREATE INDEX effect_attempts_occurrence_key ON demo.effect_attempts USING btree (tenant_id, run_id, node_id, occurrence)".to_string(),
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    assert_eq!(
        plan.actions.first().map(|action| action.kind),
        Some(RunPlaneActionKind::FrameIdentityCutover),
        "frame cutover must precede all other DDL: {:#?}",
        plan.actions
    );
    let action = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
        .expect("frame identity cutover");
    assert_eq!(action.target, "effect_attempts.frame-identity");
    assert!(
        action
            .sql
            .contains("LOCK TABLE \"demo\".effect_attempts IN ACCESS EXCLUSIVE MODE")
    );
    assert!(
        action
            .sql
            .contains("LOCK TABLE \"demo\".effect_attempt_dispatches IN ACCESS EXCLUSIVE MODE")
    );
    assert!(action.sql.contains("ERRCODE = '55000'"));
    assert!(
        action
            .sql
            .contains("MESSAGE = 'frame-identity-cutover-requires-empty-effect-facts'")
    );
    assert!(
        action.sql.find("RAISE EXCEPTION").expect("refusal")
            < action.sql.find("ALTER TABLE").expect("ddl"),
        "refusal must precede all DDL: {}",
        action.sql
    );
    assert!(
        action
            .sql
            .contains("UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence)")
    );
    assert!(
        action
            .sql
            .contains("DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk")
    );
    assert!(
        action
            .sql
            .contains("ADD CONSTRAINT effect_attempts_dispatch_identity_key")
    );
    assert!(
        action
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk")
    );
    for verb in ["UPDATE ", "DELETE ", "INSERT INTO "] {
        assert!(
            !action.sql.contains(verb),
            "cutover fabricates history with {verb}"
        );
    }
    for frame_column in EFFECT_FRAME_COLUMNS {
        let target_suffix = format!(".{frame_column}");
        assert!(
            !plan.actions.iter().any(|planned| {
                planned.kind == RunPlaneActionKind::AddColumn
                    && planned.target.ends_with(&target_suffix)
            }),
            "frame column {frame_column} must be owned by the atomic cutover"
        );
    }
}

#[test]
fn frame_cutover_defers_dispatch_fk_to_concurrent_writer_cutover() {
    let mut obs = observation_at_record();
    obs.checks.insert(
        (
            "effect_attempts".to_string(),
            "effect_attempts_current_plan_hash_check".to_string(),
        ),
        "CHECK (current_plan_hash <> '')".to_string(),
    );
    obs.column_types.insert(
        (
            "effect_attempt_dispatches".to_string(),
            "frame_id".to_string(),
        ),
        "text".to_string(),
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    let frame_position = plan
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
        .expect("effect frame cutover");
    let writer_position = plan
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("dispatch coordinate cutover");
    assert!(frame_position < writer_position);

    let frame = &plan.actions[frame_position];
    assert!(
        frame
            .sql
            .contains("DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk")
    );
    assert!(
        !frame
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk"),
        "frame cutover must not restore an FK against incompatible dispatch coordinates"
    );
    assert!(
        plan.actions[writer_position]
            .sql
            .contains("ADD CONSTRAINT effect_attempt_dispatches_attempt_fk"),
        "the following writer cutover owns dispatch-coordinate and FK restoration"
    );
}

#[test]
fn current_populated_single_target_creates_missing_peer_without_frame_refusal() {
    let mut obs = observation_at_record();
    obs.tables.remove("effect_attempts");

    let plan = plan_run_plane(&schema("demo"), &obs);
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover),
        "an absent peer must not false-refuse: {:#?}",
        plan.actions
    );
    assert!(plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::CreateTable && action.target == "effect_attempts"
    }));
}

#[test]
fn frame_identity_cutover_is_idempotent_at_record_shape() {
    let plan = plan_run_plane(&schema("demo"), &observation_at_record());
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| action.kind == RunPlaneActionKind::FrameIdentityCutover)
    );
}

#[test]
fn frame_identity_contract_drift_uses_cutover_not_generic_repairs() {
    #[expect(
        clippy::type_complexity,
        reason = "the table-driven test pairs each drift label with one noncapturing mutation"
    )]
    // wamn-0h0g.26.3.1 (204220e8) retired the node-runs projection, so
    // `effect_attempts` is the sole frame-identity target and every case
    // below drifts it.
    let cases: [(&str, fn(&mut RunPlaneObservation)); 4] = [
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
        ("legacy-node-id", |obs| {
            obs.tables
                .get_mut("effect_attempts")
                .expect("attempt table")
                .insert("node_id".to_string());
        }),
    ];

    for (case, mutate) in cases {
        let mut obs = observation_at_record();
        mutate(&mut obs);
        let plan = plan_run_plane(&schema("demo"), &obs);
        let action = plan.actions.first().expect(case);
        assert_eq!(
            action.kind,
            RunPlaneActionKind::FrameIdentityCutover,
            "{case}: {:#?}",
            plan.actions
        );
        assert!(
            action.sql.contains("DROP COLUMN IF EXISTS node_id"),
            "{case}: {}",
            action.sql
        );
        assert!(
            action.sql.contains("LOCK TABLE \"demo\".effect_attempts"),
            "{case}: wrong effect target: {}",
            action.sql
        );
        assert!(
            action
                .sql
                .contains("ADD CONSTRAINT effect_attempts_source_artifact_check"),
            "{case}: wrong effect repair: {}",
            action.sql
        );
        assert!(!plan.actions.iter().any(|planned| {
            planned.kind == RunPlaneActionKind::AddColumn
                && planned.target == "effect_attempts.requirement_name"
        }));
        assert!(!plan.actions.iter().any(|planned| {
            planned.kind == RunPlaneActionKind::RepairConstraint
                && planned.target == "effect_attempts.effect_attempts_source_artifact_check"
        }));
        assert!(
            !plan
                .extra_columns
                .iter()
                .any(|(table, column)| table == "effect_attempts" && column == "node_id"),
            "{case}: legacy node_id should be cutover-owned, not surfaced"
        );
    }
}

#[test]
fn unsafe_legacy_attempt_upgrade_refuses() {
    let mut obs = observation_at_record();
    obs.tables
        .get_mut("effect_attempts")
        .expect("attempt table")
        .insert("attempt_index".to_string());
    obs.non_nullable_columns
        .insert(("effect_attempts".to_string(), "attempt_index".to_string()));
    let action = plan_run_plane(&schema("demo"), &obs)
        .actions
        .into_iter()
        .find(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("effect writer cutover");
    assert!(
        action
            .sql
            .contains("effect-writer-cutover-requires-empty-ledger")
    );
    assert!(
        action
            .sql
            .contains("EXISTS (SELECT 1 FROM \"demo\".\"effect_attempts\")")
    );
}

#[test]
fn writer_role_verification_precedes_empty_structural_cutover() {
    let mut obs = observation_at_record();
    obs.effect_writer_role = None;
    obs.tables
        .get_mut("effect_attempts")
        .expect("attempt table")
        .insert("attempt_key".to_string());
    obs.checks.insert(
        (
            "effect_attempts".to_string(),
            "effect_attempts_key_check".to_string(),
        ),
        "CHECK (true)".to_string(),
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    let verify = plan
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::VerifyEffectWriterRole)
        .expect("writer role verification");
    let cutover = plan
        .actions
        .iter()
        .position(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("writer structural cutover");
    assert!(verify < cutover);
    assert!(!plan.actions.iter().any(|action| {
        action.kind == RunPlaneActionKind::DropExtraConstraint
            && action.target == "effect_attempts.effect_attempts_key_check"
    }));
}

#[test]
fn populated_writer_cutover_refusal_precedes_role_verification() {
    let mut obs = observation_at_record();
    obs.effect_writer_role = None;
    obs.effect_record_rows = 1;
    obs.tables
        .get_mut("effect_attempts")
        .expect("attempt table")
        .insert("attempt_key".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    assert_eq!(plan.actions.len(), 1);
    assert_eq!(
        plan.actions[0].kind,
        RunPlaneActionKind::EffectWriterCutover
    );
    assert!(
        plan.actions[0]
            .sql
            .contains("effect-writer-cutover-requires-empty-ledger")
    );
}

#[test]
fn trusted_cdc_lineage_is_unchanged_by_attempt_retirement() {
    let mut obs = observation_at_record();
    obs.tables
        .get_mut("effect_attempts")
        .expect("attempt table")
        .insert("attempt_index".to_string());
    obs.non_nullable_columns
        .insert(("effect_attempts".to_string(), "attempt_index".to_string()));
    let action = plan_run_plane(&schema("demo"), &obs)
        .actions
        .into_iter()
        .find(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("effect writer cutover");
    for lineage in ["event_source_run_id", "event_root_run_id", "event_depth"] {
        assert!(!action.sql.contains(lineage));
    }
    assert!(!action.sql.contains("runs_event_lineage_immutable"));
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
    let cutover = plan
        .actions
        .iter()
        .find(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
        .expect("dispatch identity drift uses the empty-only writer cutover");
    assert!(cutover.sql.contains(EFFECT_DISPATCH_ATTEMPT_FK_NAME));

    let targets: BTreeSet<String> = plan
        .actions
        .into_iter()
        .filter(|action| action.kind == RunPlaneActionKind::RepairForeignKey)
        .map(|action| action.target)
        .collect();
    let outcome_target = "effect_attempt_outcomes.effect_attempt_outcomes_dispatch_fk";
    assert!(
        targets.contains(outcome_target),
        "missing repair for {outcome_target}: {targets:#?}"
    );
}

/// From zero (an empty database): the full run-plane set in FK order behind
/// the schema ensure, plus the whole catalog schema — the fixture-wipe
/// restore path (manifestations 3 + 5).
#[test]
fn from_zero_plans_the_full_set_in_order() {
    let obs = RunPlaneObservation::default();
    let plan = plan_run_plane(&schema("wamn_runner_demo"), &obs);
    let kinds: Vec<RunPlaneActionKind> = plan.actions.iter().map(|a| a.kind).collect();
    assert_eq!(kinds[0], RunPlaneActionKind::VerifyEffectWriterRole);
    assert_eq!(kinds[1], RunPlaneActionKind::EnsureScenarioAuthorRole);
    assert_eq!(kinds[2], RunPlaneActionKind::EnsureSchema);
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
fn populated_legacy_runs_gain_fail_closed_capture_mode_additively() {
    let mut obs = observation_at_record();
    obs.app_run_capture_privileges = (true, false, true);
    for map in [
        &mut obs.authoring_table_privileges,
        &mut obs.authoring_effective_table_privileges,
    ] {
        map.insert(
            (
                "demo".to_string(),
                "runs".to_string(),
                "wamn_app".to_string(),
            ),
            ["SELECT", "INSERT", "UPDATE", "DELETE"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
    }
    obs.tables
        .get_mut("runs")
        .expect("runs table")
        .remove("capture_mode");
    obs.checks
        .remove(&("runs".to_string(), "runs_capture_mode_check".to_string()));
    obs.checks.remove(&(
        "runs".to_string(),
        "runs_capture_mode_source_check".to_string(),
    ));

    let plan = plan_run_plane(&schema("demo"), &obs);
    let add = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::AddColumn && action.target == "runs.capture_mode"
        })
        .expect("capture mode added independently");
    assert_eq!(add.kind, RunPlaneActionKind::AddColumn);
    assert!(
        add.sql
            .starts_with("LOCK TABLE \"demo\".runs IN ACCESS EXCLUSIVE MODE")
    );
    assert!(add.sql.contains("capture_mode text NOT NULL DEFAULT 'off'"));
    assert!(add.sql.contains("CHECK (capture_mode IN ('full', 'off'))"));
    assert!(
        add.sql
            .contains("REVOKE ALL PRIVILEGES ON TABLE \"demo\".runs")
    );
    assert!(
        !plan
            .actions
            .iter()
            .any(|action| { action.kind == RunPlaneActionKind::RepairRunCapturePrivilege })
    );
    assert!(plan.actions.iter().any(|action| {
        action.target == "runs.runs_capture_mode_source_check"
            && action
                .sql
                .contains("NOT trigger_source IS DISTINCT FROM 'scenario-draft'")
    }));
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
    assert!(repair.sql.contains("tenant_id"));
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
        ("runs".to_string(), "runs_fail_kind_check".to_string()),
        "CHECK (fail_kind = 'terminal'::text)".to_string(),
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
        action.target == "runs.runs_fail_kind_check"
            && action
                .sql
                .contains("DROP CONSTRAINT \"runs_fail_kind_check\"")
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
///
/// Absent from the record is only half of it. A relation dropped from the
/// record but not RETIRED survives with live grants on every schema
/// provisioned before the change, and nothing REVOKEs on it — a privilege the
/// reconciler can no longer see (wamn-0h0g.15.78). So the store must also be
/// named by the retirement mechanism, together with the FK columns that would
/// block its drop.
#[test]
fn the_authoring_test_set_store_is_absent_from_the_record() {
    assert!(RETIRED_STORED_SUITE_TABLES.contains(&"authoring_test_sets"));
    assert!(
        RETIRED_STORED_SUITE_FUNCTIONS.contains(&"reject_immutable_authoring_test_set_change")
    );
    assert_eq!(
        RETIRED_STORED_SUITE_TABLES
            .iter()
            .position(|table| *table == "authoring_test_sets"),
        Some(RETIRED_STORED_SUITE_TABLES.len() - 1),
        "the FK parent drops last"
    );
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
    // A retired helper the observation cannot NAME is one the cutover can
    // never be planned for: the driver reads a fixed `proname IN (...)`.
    for function in RETIRED_STORED_SUITE_FUNCTIONS {
        assert!(
            select_run_plane_helper_functions_sql().contains(&format!("'{function}'")),
            "retired helper {function} is unobservable"
        );
    }
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
        .insert("receipts".to_string(), ["id".to_string()].into());
    obs.checks.insert(
        ("receipts".to_string(), "receipts_check".to_string()),
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
    assert!(terminal_delete_guard.sql.contains(
        "IF OLD.status NOT IN ('completed', 'failed', 'infrastructure-failure') THEN"
    ));
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
        12
    );
}

#[test]
fn operator_action_helper_and_acl_pin_admin_only_append_and_immutability() {
    assert!(
        REJECT_IMMUTABLE_OPERATOR_RUN_ACTION_CHANGE_SQL
            .contains("operator-run-action-immutable")
    );
    assert!(RUN_STATE_SQL.contains(
        "REVOKE ALL PRIVILEGES ON TABLE wamn_run.operator_run_actions\n    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer"
    ));
    assert!(!RUN_STATE_SQL.contains("GRANT INSERT ON wamn_run.operator_run_actions"));
    assert!(RUN_STATE_SQL.contains("operator_run_actions_update_immutable"));
    assert!(RUN_STATE_SQL.contains("operator_run_actions_delete_immutable"));
}

#[test]
fn effect_writer_surface_uses_acl_not_insert_authorization_triggers() {
    assert!(!RUN_STATE_SQL.contains("CREATE ROLE wamn_effect_writer"));
    assert!(!RUN_STATE_SQL.contains("guard_effect_writer_append"));
    assert!(!RUN_STATE_SQL.contains("writer_insert_guard"));
    assert!(RUN_STATE_SQL.contains(
        "REVOKE ALL PRIVILEGES ON TABLE wamn_run.effect_attempts\n    FROM PUBLIC, wamn_app, wamn_scenario_author, wamn_effect_writer"
    ));
    // BORN PARKED: read-only at record. This is the DDL-TEXT half only, and it
    // cannot tell a declaration from a comment mentioning one. The load-bearing
    // arm is THE SERVER'S refusal, asserted live over the applied DDL in
    // `crates/control/provision/tests/deploy_sql_authority.rs` and over the
    // reconciled result in `services/ctl/tests/run_plane_live.rs`.
    assert!(
        RUN_STATE_SQL
            .contains("GRANT SELECT ON wamn_run.effect_attempts TO wamn_effect_writer")
    );
    assert!(
        !RUN_STATE_SQL
            .contains("GRANT SELECT, INSERT ON wamn_run.effect_attempts TO wamn_effect_writer")
    );
    assert!(
        RUN_STATE_SQL.contains("ALTER TABLE wamn_run.effect_attempts FORCE ROW LEVEL SECURITY")
    );
    assert!(RUN_STATE_SQL.contains(
        "GRANT SELECT (tenant_id, run_id, status)\n    ON wamn_run.runs TO wamn_effect_writer"
    ));
    assert!(RUN_QUEUE_SQL.contains(
        "GRANT SELECT (tenant_id, run_id, lease_owner, lease_expires_at, lease_generation)\n    ON wamn_run.run_queue TO wamn_effect_writer"
    ));
    assert!(!RUN_STATE_SQL.contains("GRANT SELECT ON wamn_run.runs TO wamn_effect_writer"));
    assert!(
        !RUN_QUEUE_SQL.contains("GRANT SELECT ON wamn_run.run_queue TO wamn_effect_writer")
    );
}

#[test]
fn effect_writer_run_reads_reconcile_to_exact_columns_without_table_authority() {
    let mut obs = observation_at_record();
    obs.effect_writer_run_table_privileges.insert(
        "runs".to_string(),
        ["SELECT".to_string(), "UPDATE".to_string()]
            .into_iter()
            .collect(),
    );
    obs.effect_writer_run_column_privileges
        .remove(&("run_queue".to_string(), "lease_expires_at".to_string()));
    obs.tables
        .get_mut("run_queue")
        .expect("queue table")
        .remove("lease_expires_at");
    obs.effect_writer_run_column_privileges.insert(
        ("run_queue".to_string(), "lease_generation".to_string()),
        ["SELECT".to_string()].into_iter().collect(),
    );

    let plan = plan_run_plane(&schema("demo"), &obs);
    let runs = plan
        .actions
        .iter()
        .find(|action| action.target == "demo.runs.effect-read")
        .expect("runs writer-read repair");
    assert_eq!(runs.kind, RunPlaneActionKind::RepairEffectWriterPrivilege);
    assert!(
        runs.sql
            .contains("REVOKE ALL PRIVILEGES ON TABLE \"demo\".\"runs\"")
    );
    assert!(
        runs.sql
            .contains("GRANT SELECT (\"tenant_id\", \"run_id\", \"status\")")
    );
    assert!(runs.sql.contains("has_table_privilege"));

    let queue = plan
        .actions
        .iter()
        .find(|action| action.target == "demo.run_queue.effect-read")
        .expect("queue writer-read repair");
    let add = plan
        .actions
        .iter()
        .position(|action| {
            action.kind == RunPlaneActionKind::AddColumn
                && action.target == "run_queue.lease_expires_at"
        })
        .expect("missing allowed queue column is restored");
    let repair = plan
        .actions
        .iter()
        .position(|action| action.target == "demo.run_queue.effect-read")
        .unwrap();
    assert!(add < repair);
    assert!(queue.sql.contains(
        "GRANT SELECT (\"tenant_id\", \"run_id\", \"lease_owner\", \"lease_expires_at\", \"lease_generation\")"
    ));
    assert!(queue.sql.contains("attribute.attname"));
    assert!(!queue.sql.contains("ARRAY['lease_generation']"));
}

#[test]
fn effect_writer_acl_repair_removes_schema_table_and_column_drift() {
    let mut obs = observation_at_record();
    obs.effect_writer_schema_privileges = (true, true);
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
    let schema_action = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
                && action.target == "demo.usage"
        })
        .expect("writer schema ACL repair");
    assert!(
        schema_action
            .sql
            .contains("FROM PUBLIC, wamn_effect_writer")
    );
    assert!(
        schema_action
            .sql
            .contains("effect-writer-schema-privilege-out-of-bounds")
    );
    let table_action = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
                && action.target == "demo.effect_attempts"
        })
        .expect("writer table ACL repair");
    assert!(table_action.sql.contains("REVOKE SELECT ("));
    assert!(table_action.sql.contains("has_any_column_privilege"));
    // BORN PARKED: the attempt table is re-granted READ ONLY, and the block
    // refuses to see the server still report the writer holding INSERT.
    assert!(
        table_action.sql.contains(
            "GRANT SELECT ON TABLE \"demo\".\"effect_attempts\" TO wamn_effect_writer"
        )
    );
    assert!(!table_action.sql.contains("GRANT SELECT, INSERT"));
    assert!(table_action.sql.contains(
        "ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
             WHERE pg_catalog.has_table_privilege('wamn_effect_writer'"
    ));
    assert!(table_action.sql.contains(
        "has_any_column_privilege('wamn_effect_writer', \
             '\"demo\".\"effect_attempts\"', 'INSERT,UPDATE,REFERENCES')"
    ));
}

/// The convergent author, for one sibling table. `deploy/sql/run-state.sql`
/// is only the BIRTH author; this builder is what an already-provisioned
/// database converges onto, so restoring the append here would re-mint the
/// dormant authority on every reconcile even with the DDL parked.
#[test]
fn dispatch_table_reconciles_to_a_parked_writer() {
    assert_sibling_table_reconciles_parked("effect_attempt_dispatches");
}

/// The same arm for the second sibling, named separately so a mutant that
/// re-arms exactly one relation cannot hide behind the other.
#[test]
fn outcome_table_reconciles_to_a_parked_writer() {
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
            action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
                && action.target == format!("demo.{table}")
        })
        .expect("sibling table ACL repair");
    // The re-grant is READ ONLY…
    assert!(
        action.sql.contains(&format!(
            "GRANT SELECT ON TABLE \"demo\".\"{table}\" TO wamn_effect_writer"
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
                 WHERE pg_catalog.has_table_privilege('wamn_effect_writer'"
        ),
        "{table}: table-level self-check does not forbid INSERT: {}",
        action.sql
    );
    assert!(
        action.sql.contains(&format!(
            "has_any_column_privilege('wamn_effect_writer', \
                 '\"demo\".\"{table}\"', 'INSERT,UPDATE,REFERENCES')"
        )),
        "{table}: column-level self-check does not forbid INSERT: {}",
        action.sql
    );
}

#[test]
fn cutover_owned_columns_are_not_named_by_later_acl_repair() {
    let mut obs = observation_at_record();
    let columns = obs
        .tables
        .get_mut("effect_attempts")
        .expect("attempt table");
    columns.insert("node_id".to_string());
    columns.insert("attempt_key".to_string());
    for column in EFFECT_FRAME_COLUMNS {
        columns.remove(*column);
    }
    obs.effect_table_effective_column_privileges
        .entry(("effect_attempts".to_string(), "wamn_app".to_string()))
        .or_default()
        .insert("UPDATE".to_string());

    let plan = plan_run_plane(&schema("demo"), &obs);
    assert!(
        plan.actions
            .iter()
            .any(|action| { action.kind == RunPlaneActionKind::FrameIdentityCutover })
    );
    assert!(
        plan.actions
            .iter()
            .any(|action| { action.kind == RunPlaneActionKind::EffectWriterCutover })
    );
    let repair = plan
        .actions
        .iter()
        .find(|action| {
            action.kind == RunPlaneActionKind::RepairEffectWriterPrivilege
                && action.target == "demo.effect_attempts"
        })
        .expect("writer table ACL repair");
    assert!(repair.sql.contains(&quote_ident("attempt_id")));
    for dropped in ["node_id", "attempt_key"]
        .into_iter()
        .chain(EFFECT_FRAME_COLUMNS.iter().copied())
    {
        assert!(
            !repair.sql.contains(&quote_ident(dropped)),
            "later ACL repair names cutover-owned column {dropped}: {}",
            repair.sql
        );
    }
}

#[test]
fn extra_record_trigger_is_removed_but_floor_trigger_is_untouched() {
    let mut obs = observation_at_record();
    obs.triggers.insert(
        ("runs".to_string(), "legacy_runs_trigger".to_string()),
        "CREATE TRIGGER legacy_runs_trigger".to_string(),
    );
    obs.triggers.insert(
        ("receipts".to_string(), "receipts_trigger".to_string()),
        "CREATE TRIGGER receipts_trigger".to_string(),
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
        rewrite_schema(RUN_STATE_SQL, &schema)
            .contains("CREATE TABLE poc_f1.operator_run_actions")
    );
    assert!(
        !rewrite_schema(RUN_STATE_SQL, &schema)
            .contains("SET search_path = pg_catalog, wamn_run")
    );
    assert!(
        !rewrite_schema(RUN_STATE_SQL, &schema)
            .contains("SET search_path = pg_catalog, pg_temp, poc_f1")
    );
}

/// wamn-0h0g.12.123. Pinned SEPARATELY from the block below because the
/// exact shape of these two is the bug: `select_run_capture_privileges_sql`
/// encoded a grant shape the real grant could never satisfy, so drift stayed
/// permanently true and the reconciler planned a repair forever
/// (wamn-0h0g.12.40). These queries must observe ONLY what the reader's
/// `REVOKE`/`GRANT` repair can reach.
#[test]
fn dispatch_reader_observation_sql_is_pinned() {
    let schema_privileges = select_dispatch_reader_schema_privileges_sql();
    let table_privileges = select_dispatch_reader_table_privileges_sql();

    // DIRECT acl entries only. `has_schema_privilege` / `has_table_privilege`
    // would also report authority reached through PUBLIC or a group, which
    // `REVOKE … FROM "wamn_dispatch_reader"` cannot remove.
    for observation in [schema_privileges, table_privileges] {
        assert!(observation.contains("aclexplode"), "{observation}");
        assert!(
            observation.contains("acl.grantee = reader.oid"),
            "{observation}"
        );
        assert!(
            !observation.contains("has_schema_privilege"),
            "{observation}"
        );
        assert!(
            !observation.contains("has_table_privilege"),
            "{observation}"
        );
        assert!(
            !observation.contains("has_column_privilege"),
            "{observation}"
        );
        // The role name is bound, never inlined: `wamn_control_provision`
        // owns it, and a second copy here could drift from the builder.
        assert!(observation.contains("$2"), "{observation}");
        assert!(
            !observation.contains("wamn_dispatch_reader"),
            "{observation}"
        );
    }

    // Role absence must be observable and must not collapse into "no
    // privileges", which would be indistinguishable from a role that exists
    // and was never granted.
    assert!(schema_privileges.contains("reader.oid IS NOT NULL"));
    assert!(schema_privileges.contains("LEFT JOIN pg_catalog.pg_roles"));
    assert!(schema_privileges.contains("acldefault('n', namespace.nspowner)"));

    // Exactly the relkinds `GRANT/REVOKE … ON ALL TABLES IN SCHEMA` reaches.
    // A sequence grant observed here would be drift the repair could never
    // clear — the .12.40 shape again, one relkind over.
    assert!(table_privileges.contains("relation.relkind IN ('r', 'p', 'v', 'm', 'f')"));
    assert!(!table_privileges.contains("'S'"));
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
    assert!(select_outbox_trigger_tables_sql().contains("'wamn_outbox_event'"));
    assert!(select_outbox_function_present_sql().contains("pg_proc"));
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
    let writer_tables = select_effect_writer_run_table_privileges_sql();
    assert!(writer_tables.contains("has_table_privilege"));
    assert!(writer_tables.contains("relation.relname IN ('runs', 'run_queue')"));
    let writer_columns = select_effect_writer_run_column_privileges_sql();
    assert!(writer_columns.contains("has_column_privilege"));
    assert!(writer_columns.contains("attribute.attnum > 0"));
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
    assert!(select_authoring_effective_table_privileges_sql().contains("effective_releases"));
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
            .contains("reject_immutable_authoring_report_change")
    );
    assert!(select_run_plane_helper_functions_sql().contains("guard_authoring_report_write"));
    assert!(
        select_run_plane_helper_functions_sql()
            .contains("reject_immutable_operator_run_action_change")
    );
    assert!(
        select_run_plane_helper_functions_sql().contains("guard_effect_disposition_append")
    );
    assert_eq!(
        strip_retired_registration_keys_sql(),
        "UPDATE catalog.event_registrations \
             SET registration = registration - 'state' - 'partition-key' \
             WHERE registration ?| ARRAY['state', 'partition-key']"
    );
    assert_eq!(
        count_stale_registration_keys_sql(),
        "SELECT count(*) FROM catalog.event_registrations \
             WHERE registration ?| ARRAY['state', 'partition-key']"
    );
}
