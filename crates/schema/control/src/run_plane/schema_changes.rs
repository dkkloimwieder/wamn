//! Conditions and SQL for changing retired run-schema structures.

use super::schema::{
    normalize_observed_schema, quote_ident,
};

use super::{
    BareSchemaName, RunPlaneObservation, rewrite_schema,
};

use super::declarations::{
    CHECK_SPECS, EFFECT_ATTEMPTS_DISPATCH_IDENTITY_KEY_DEF, EFFECT_ATTEMPTS_OCCURRENCE_KEY_DEF,
    EFFECT_DISPATCHES_OCCURRENCE_KEY_DEF, EFFECT_DISPATCH_ATTEMPT_FK_DEF,
    EFFECT_DISPATCH_ATTEMPT_FK_NAME, EFFECT_DISPATCH_ATTEMPT_FK_SQL, EFFECT_FRAME_CHECKS,
    EFFECT_FRAME_COLUMNS, RETIRED_EFFECT_ATTEMPT_COLUMNS, RUNS_EXECUTION_GRAIN_CHECK_DEF,
    RUNS_ROOT_INDEX_DEF, RUNS_WIRING_IDENTITY_CHECK_DEF,
};

pub(super) fn effect_writer_cutover_owned_check(table: &str, name: &str) -> bool {
    table == "effect_attempts"
        && matches!(
            name,
            "effect_attempts_attempt_index_check"
                | "effect_attempts_lineage_check"
                | "effect_attempts_recovery_class_check"
                | "effect_attempts_key_check"
        )
}

pub(super) fn frame_identity_column(table: &str, column: &str) -> bool {
    table == "effect_attempts" && (EFFECT_FRAME_COLUMNS.contains(&column) || column == "node_id")
}

pub(super) fn frame_identity_check(table: &str, name: &str) -> bool {
    table == "effect_attempts" && EFFECT_FRAME_CHECKS.contains(&name)
}

fn expected_check_definition(table: &str, name: &str) -> Option<&'static str> {
    CHECK_SPECS
        .iter()
        .find(|spec| spec.table == table && spec.name == name)
        .map(|spec| spec.definition)
}

fn column_contract_complete(
    obs: &RunPlaneObservation,
    table: &str,
    column: &str,
    ty: &str,
    not_null: bool,
) -> bool {
    let key = (table.to_string(), column.to_string());
    obs.tables
        .get(table)
        .is_some_and(|columns| columns.contains(column))
        && obs
            .column_types
            .get(&key)
            .is_some_and(|actual| actual == ty)
        && obs.non_nullable_columns.contains(&key) == not_null
}

fn check_contract_complete(obs: &RunPlaneObservation, table: &str, names: &[&str]) -> bool {
    names.iter().all(|name| {
        expected_check_definition(table, name).is_some_and(|expected| {
            obs.checks
                .get(&(table.to_string(), (*name).to_string()))
                .is_some_and(|actual| actual == expected)
        })
    })
}

fn effect_frame_contract_complete(obs: &RunPlaneObservation, schema: &BareSchemaName) -> bool {
    let Some(columns) = obs.tables.get("effect_attempts") else {
        return true;
    };
    !columns.contains("node_id")
        && column_contract_complete(obs, "effect_attempts", "root_plan_hash", "text", true)
        && column_contract_complete(obs, "effect_attempts", "current_plan_hash", "text", true)
        && column_contract_complete(obs, "effect_attempts", "frame_id", "bigint", true)
        && column_contract_complete(obs, "effect_attempts", "parent_frame_id", "bigint", false)
        && column_contract_complete(obs, "effect_attempts", "call_site_id", "text", false)
        && column_contract_complete(obs, "effect_attempts", "local_node_id", "text", true)
        && column_contract_complete(obs, "effect_attempts", "source_artifact_hash", "text", true)
        && column_contract_complete(obs, "effect_attempts", "requirement_name", "text", true)
        && check_contract_complete(obs, "effect_attempts", EFFECT_FRAME_CHECKS)
        && (obs
            .indexes
            .get("effect_attempts_occurrence_key")
            .is_some_and(|definition| {
                normalize_observed_schema(definition, schema) == EFFECT_ATTEMPTS_OCCURRENCE_KEY_DEF
            })
            || retired_effect_frame_identity_complete(obs))
}

fn retired_effect_frame_identity_complete(obs: &RunPlaneObservation) -> bool {
    [
        (
            "effect_attempts_tenant_id_attempt_id_run_id_node_id_occurrence_key",
            "(tenant_id, attempt_id, run_id, frame_id, local_node_id, occurrence)",
        ),
        (
            "effect_attempts_tenant_id_run_id_node_id_occurrence_attempt_index_key",
            "(tenant_id, run_id, frame_id, local_node_id, occurrence, attempt_index)",
        ),
    ]
    .iter()
    .all(|(name, columns)| {
        observed_index(obs, name).is_some_and(|definition| {
            definition
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .split_once(" USING btree ")
                .is_some_and(|(_, actual)| actual == *columns)
        })
    })
}

fn observed_index<'a>(obs: &'a RunPlaneObservation, name: &str) -> Option<&'a String> {
    obs.indexes.get(postgres_visible_identifier(name))
}

pub(super) fn postgres_visible_identifier(name: &str) -> &str {
    if name.len() <= 63 {
        return name;
    }
    let mut end = 63;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameIdentityCutoverTargets {
    pub(super) effect: bool,
    dispatch: bool,
    restore_dispatch_fk: bool,
}

impl FrameIdentityCutoverTargets {
    pub(super) const fn needed(self) -> bool {
        self.effect
    }

    pub(super) fn includes_table(self, table: &str) -> bool {
        table == "effect_attempts" && self.effect
    }
}

pub(super) fn frame_identity_cutover_targets(
    obs: &RunPlaneObservation,
    schema: &BareSchemaName,
) -> FrameIdentityCutoverTargets {
    let effect = !effect_frame_contract_complete(obs, schema);
    let dispatch = effect && obs.tables.contains_key("effect_attempt_dispatches");
    FrameIdentityCutoverTargets {
        effect,
        dispatch,
        restore_dispatch_fk: dispatch && !effect_writer_ledger_cutover_needed(schema, obs),
    }
}

pub(super) fn frame_identity_cutover_sql(
    schema: &BareSchemaName,
    targets: FrameIdentityCutoverTargets,
) -> String {
    debug_assert!(targets.needed());
    let target = schema;
    let schema = target.quoted();
    let mut sql = String::new();
    let mut populated = Vec::new();
    if targets.effect {
        sql.push_str(&format!(
            "LOCK TABLE {schema}.effect_attempts IN ACCESS EXCLUSIVE MODE;\n"
        ));
        populated.push(format!("EXISTS (SELECT 1 FROM {schema}.effect_attempts)"));
    }
    if targets.dispatch {
        sql.push_str(&format!(
            "LOCK TABLE {schema}.effect_attempt_dispatches IN ACCESS EXCLUSIVE MODE;\n"
        ));
        populated.push(format!(
            "EXISTS (SELECT 1 FROM {schema}.effect_attempt_dispatches)"
        ));
    }
    sql.push_str(&format!(
        r#"DO $frame_identity_cutover$
BEGIN
    IF {} THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'frame-identity-cutover-requires-empty-effect-facts';
    END IF;
END
$frame_identity_cutover$;
"#,
        populated.join(" OR ")
    ));
    if targets.effect {
        if targets.dispatch {
            sql.push_str(&format!(
                "ALTER TABLE {schema}.effect_attempt_dispatches \
                 DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk;\n"
            ));
        }
        sql.push_str(&format!(
            r#"ALTER TABLE {schema}.effect_attempts
    DROP CONSTRAINT IF EXISTS effect_attempts_occurrence_key,
    DROP CONSTRAINT IF EXISTS effect_attempts_dispatch_identity_key,
    DROP CONSTRAINT IF EXISTS effect_attempts_root_plan_hash_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_current_plan_hash_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_frame_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_frame_relation_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_local_node_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_source_artifact_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_requirement_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_tenant_id_attempt_id_run_id_node_id_occurrence_key,
    DROP CONSTRAINT IF EXISTS effect_attempts_tenant_id_run_id_node_id_occurrence_attempt_index_key;
DROP INDEX IF EXISTS {schema}.effect_attempts_occurrence_key;
DROP INDEX IF EXISTS {schema}.effect_attempts_dispatch_identity_key;
DROP INDEX IF EXISTS {schema}.effect_attempts_occurrence;
DROP INDEX IF EXISTS {schema}.effect_attempts_tenant_id_attempt_id_run_id_node_id_occurrence_key;
DROP INDEX IF EXISTS {schema}.effect_attempts_tenant_id_run_id_node_id_occurrence_attempt_index_key;
ALTER TABLE {schema}.effect_attempts
    DROP COLUMN IF EXISTS node_id,
    DROP COLUMN IF EXISTS root_plan_hash,
    DROP COLUMN IF EXISTS current_plan_hash,
    DROP COLUMN IF EXISTS frame_id,
    DROP COLUMN IF EXISTS parent_frame_id,
    DROP COLUMN IF EXISTS call_site_id,
    DROP COLUMN IF EXISTS local_node_id,
    DROP COLUMN IF EXISTS source_artifact_hash,
    DROP COLUMN IF EXISTS requirement_name;
ALTER TABLE {schema}.effect_attempts
    ADD COLUMN root_plan_hash text NOT NULL,
    ADD COLUMN current_plan_hash text NOT NULL,
    ADD COLUMN frame_id bigint NOT NULL DEFAULT 0,
    ADD COLUMN parent_frame_id bigint,
    ADD COLUMN call_site_id text,
    ADD COLUMN local_node_id text NOT NULL,
    ADD COLUMN source_artifact_hash text NOT NULL,
    ADD COLUMN requirement_name text NOT NULL,
    ADD CONSTRAINT effect_attempts_root_plan_hash_check CHECK (root_plan_hash ~ '^sha256:[0-9a-f]{{64}}$'),
    ADD CONSTRAINT effect_attempts_current_plan_hash_check CHECK (current_plan_hash ~ '^sha256:[0-9a-f]{{64}}$'),
    ADD CONSTRAINT effect_attempts_frame_check CHECK (frame_id >= 0),
    ADD CONSTRAINT effect_attempts_frame_relation_check CHECK (
        (frame_id = 0 AND parent_frame_id IS NULL AND call_site_id IS NULL)
        OR (frame_id > 0 AND parent_frame_id IS NOT NULL AND parent_frame_id >= 0
            AND parent_frame_id < frame_id AND call_site_id IS NOT NULL
            AND call_site_id ~ '^[a-z0-9-]+$')
    ),
    ADD CONSTRAINT effect_attempts_local_node_check CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    ADD CONSTRAINT effect_attempts_source_artifact_check CHECK (source_artifact_hash ~ '^sha256:[0-9a-f]{{64}}$'),
    ADD CONSTRAINT effect_attempts_requirement_check CHECK (requirement_name <> ''),
    ADD CONSTRAINT effect_attempts_occurrence_key
        UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence),
    ADD CONSTRAINT effect_attempts_dispatch_identity_key
        UNIQUE (tenant_id, attempt_id, attempt_started_at,
                run_id, frame_id, local_node_id, occurrence);
"#
        ));
        if targets.restore_dispatch_fk {
            sql.push_str(&rewrite_schema(EFFECT_DISPATCH_ATTEMPT_FK_SQL, target));
            sql.push_str(";\n");
        }
    }
    sql
}

pub(super) fn effect_writer_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
    let target = schema;
    let schema = target.quoted();
    let ledger_cutover_needed = effect_writer_ledger_cutover_needed(target, obs);
    let present_ledgers: Vec<&str> = if ledger_cutover_needed {
        [
            "effect_attempts",
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
        ]
        .into_iter()
        .filter(|table| obs.tables.contains_key(*table))
        .collect()
    } else {
        Vec::new()
    };
    let mut sql = present_ledgers
        .iter()
        .map(|table| {
            format!(
                "LOCK TABLE {schema}.{} IN ACCESS EXCLUSIVE MODE;",
                quote_ident(table)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let populated = present_ledgers
        .iter()
        .map(|table| format!("EXISTS (SELECT 1 FROM {schema}.{})", quote_ident(table)))
        .collect::<Vec<_>>()
        .join(" OR ");
    let populated = if populated.is_empty() {
        "false".to_string()
    } else {
        populated
    };
    sql.push_str(&format!(
        r#"
DO $retire$
BEGIN
    IF {populated} THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'effect-writer-cutover-requires-empty-ledger';
    END IF;
END
$retire$;
"#,
    ));

    if !ledger_cutover_needed {
        return sql;
    }

    for table in &present_ledgers {
        sql.push_str(&format!(
            "DROP TRIGGER IF EXISTS {} ON {schema}.{};\n",
            quote_ident(&format!("{table}_insert_guard")),
            quote_ident(table),
        ));
    }
    sql.push_str(&format!(
        "DROP FUNCTION IF EXISTS {schema}.guard_effect_fact_append();\n"
    ));

    if obs.tables.contains_key("effect_attempt_dispatches") {
        sql.push_str(&format!(
            "ALTER TABLE {schema}.effect_attempt_dispatches \
             DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk;\n"
        ));
    }

    if let Some(attempt_columns) = obs.tables.get("effect_attempts") {
        sql.push_str(&format!(
            r#"ALTER TABLE {schema}.effect_attempts
    DROP CONSTRAINT IF EXISTS effect_attempts_predecessor_fk,
    DROP CONSTRAINT IF EXISTS effect_attempts_attempt_index_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_lineage_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_recovery_class_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_key_check,
    DROP CONSTRAINT IF EXISTS effect_attempts_tenant_id_attempt_id_run_id_node_id_occurrence_key,
    DROP CONSTRAINT IF EXISTS effect_attempts_tenant_id_run_id_node_id_occurrence_attempt_index_key,
    DROP CONSTRAINT IF EXISTS effect_attempts_tenant_id_attempt_id_attempt_started_at_key,
    DROP CONSTRAINT IF EXISTS effect_attempts_dispatch_identity_key;
DROP INDEX IF EXISTS {schema}.effect_attempts_occurrence;
DROP INDEX IF EXISTS {schema}.effect_attempts_tenant_id_attempt_id_run_id_node_id_occurrence_key;
DROP INDEX IF EXISTS {schema}.effect_attempts_tenant_id_run_id_node_id_occurrence_attempt_index_key;
DROP INDEX IF EXISTS {schema}.effect_attempts_dispatch_identity_key;
"#,
        ));
        for column in RETIRED_EFFECT_ATTEMPT_COLUMNS {
            if attempt_columns.contains(*column) {
                sql.push_str(&format!(
                    "ALTER TABLE {schema}.effect_attempts DROP COLUMN {};\n",
                    quote_ident(column),
                ));
            }
        }
        sql.push_str(&format!(
            "ALTER TABLE {schema}.effect_attempts \
             ALTER COLUMN attempt_started_at SET DEFAULT now(), \
             ADD CONSTRAINT effect_attempts_dispatch_identity_key \
             UNIQUE (tenant_id,attempt_id,attempt_started_at,run_id,frame_id,local_node_id,occurrence);\n"
        ));
    }

    if obs.tables.contains_key("effect_attempt_dispatches") {
        sql.push_str(&format!(
            r#"ALTER TABLE {schema}.effect_attempt_dispatches
    DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_attempt_fk,
    DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_occurrence_key,
    DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_frame_check,
    DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_local_node_check,
    DROP CONSTRAINT IF EXISTS effect_attempt_dispatches_occurrence_check,
    DROP COLUMN IF EXISTS run_id,
    DROP COLUMN IF EXISTS frame_id,
    DROP COLUMN IF EXISTS local_node_id,
    DROP COLUMN IF EXISTS occurrence;
DROP INDEX IF EXISTS {schema}.effect_attempt_dispatches_occurrence_key;
ALTER TABLE {schema}.effect_attempt_dispatches
    ADD COLUMN run_id text NOT NULL,
    ADD COLUMN frame_id bigint NOT NULL,
    ADD COLUMN local_node_id text NOT NULL,
    ADD COLUMN occurrence int NOT NULL,
    ADD CONSTRAINT effect_attempt_dispatches_frame_check CHECK (frame_id >= 0),
    ADD CONSTRAINT effect_attempt_dispatches_local_node_check CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    ADD CONSTRAINT effect_attempt_dispatches_occurrence_check CHECK (occurrence >= 0),
    ADD CONSTRAINT effect_attempt_dispatches_occurrence_key
        UNIQUE (tenant_id,run_id,frame_id,local_node_id,occurrence);
"#,
        ));
        if obs.tables.contains_key("effect_attempts") {
            sql.push_str(&rewrite_schema(EFFECT_DISPATCH_ATTEMPT_FK_SQL, target));
            sql.push_str(";\n");
        }
    }
    sql
}

pub(super) fn effect_writer_ledger_cutover_needed(schema: &BareSchemaName, obs: &RunPlaneObservation) -> bool {
    let attempts_need_cutover = obs.tables.get("effect_attempts").is_some_and(|columns| {
        RETIRED_EFFECT_ATTEMPT_COLUMNS
            .iter()
            .any(|column| columns.contains(*column))
            || !obs.defaulted_columns.contains(&(
                "effect_attempts".to_string(),
                "attempt_started_at".to_string(),
            ))
            || obs
                .indexes
                .get("effect_attempts_dispatch_identity_key")
                .is_none_or(|definition| {
                    normalize_observed_schema(definition, schema)
                        != EFFECT_ATTEMPTS_DISPATCH_IDENTITY_KEY_DEF
                })
    });
    let dispatches_need_cutover = obs
        .tables
        .get("effect_attempt_dispatches")
        .is_some_and(|_| {
            !column_contract_complete(obs, "effect_attempt_dispatches", "run_id", "text", true)
                || !column_contract_complete(
                    obs,
                    "effect_attempt_dispatches",
                    "frame_id",
                    "bigint",
                    true,
                )
                || !column_contract_complete(
                    obs,
                    "effect_attempt_dispatches",
                    "local_node_id",
                    "text",
                    true,
                )
                || !column_contract_complete(
                    obs,
                    "effect_attempt_dispatches",
                    "occurrence",
                    "integer",
                    true,
                )
                || obs
                    .indexes
                    .get("effect_attempt_dispatches_occurrence_key")
                    .is_none_or(|definition| {
                        normalize_observed_schema(definition, schema)
                            != EFFECT_DISPATCHES_OCCURRENCE_KEY_DEF
                    })
                || (obs.tables.contains_key("effect_attempts")
                    && obs
                        .foreign_keys
                        .get(&(
                            "effect_attempt_dispatches".to_string(),
                            EFFECT_DISPATCH_ATTEMPT_FK_NAME.to_string(),
                        ))
                        .is_none_or(|definition| {
                            normalize_observed_schema(definition, schema)
                                != EFFECT_DISPATCH_ATTEMPT_FK_DEF
                        }))
        });
    let retired_insert_guard_present = [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ]
    .into_iter()
    .any(|table| {
        obs.triggers
            .contains_key(&(table.to_string(), format!("{table}_insert_guard")))
    }) || obs
        .helper_functions
        .contains_key("guard_effect_fact_append");
    attempts_need_cutover || dispatches_need_cutover || retired_insert_guard_present
}

pub(super) const RETIRED_PARTITION_COLUMNS: [&str; 2] = ["partition_key", "partition_policy"];
const RETIRED_PARTITION_TABLES: [&str; 2] = ["partition_owner", "run_dead_letters"];
pub(super) const RETIRED_PARTITION_CHECK: &str = "run_queue_partition_policy_check";
pub(super) const RETIRED_PARTITION_INDEX: &str = "run_queue_partition";
pub(super) const RETIRED_AUTHORED_ORDERING_REFUSAL: &str =
    "retired-authored-ordering-requires-environment-reprovision";
/// Stable operator-facing refusal for retained history that cannot be cut over.
pub(super) const RETIRED_DEAD_LETTER_REFUSAL: &str =
    "retired-run-dead-letter-history-requires-archive-or-environment-reprovision";
const RUN_QUEUE_CLAIMABLE_COLUMNS: [&str; 5] = [
    "tenant_id",
    "available_at",
    "stream_seq",
    "run_id",
    "lease_expires_at",
];

pub(super) const RETIRED_CHILD_RUN_COLUMNS: [&str; 8] = [
    "parent_run_id",
    "parent_node_id",
    "parent_occurrence",
    "waiting_child_run_id",
    "waiting_child_occurrence",
    "wait_generation",
    "invoke_depth",
    "invoke_root_run_id",
];
pub(super) const RETIRED_CHILD_RUN_INDEXES: [&str; 3] = [
    "runs_parent_occurrence",
    "runs_invoke_root",
    "runs_waiting_child",
];

pub(super) fn child_run_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("runs").is_some_and(|columns| {
        RETIRED_CHILD_RUN_COLUMNS
            .iter()
            .any(|column| columns.contains(*column))
    }) || RETIRED_CHILD_RUN_INDEXES
        .iter()
        .any(|index| obs.indexes.contains_key(*index))
        || obs
            .checks
            .iter()
            .any(|((table, _), definition)| table == "runs" && retired_child_run_check(definition))
}

pub(super) fn retired_child_run_check(definition: &str) -> bool {
    RETIRED_CHILD_RUN_COLUMNS
        .iter()
        .any(|column| definition.contains(column))
}

pub(super) fn child_run_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
    let target = schema.quoted();
    let columns = obs
        .tables
        .get("runs")
        .expect("a child-run column, check, or index requires the runs table");
    let populated_refusals = RETIRED_CHILD_RUN_COLUMNS
        .iter()
        .filter(|column| columns.contains::<str>(column))
        .map(|column| {
            if *column == "invoke_depth" {
                format!("{} IS DISTINCT FROM 0", quote_ident(column))
            } else {
                format!("{} IS NOT NULL", quote_ident(column))
            }
        })
        .collect::<Vec<_>>();
    let drops = RETIRED_CHILD_RUN_COLUMNS
        .iter()
        .filter(|column| columns.contains::<str>(column))
        .map(|column| format!("DROP COLUMN IF EXISTS {}", quote_ident(column)))
        .collect::<Vec<_>>();

    let mut statements = vec![format!("LOCK TABLE {target}.runs IN ACCESS EXCLUSIVE MODE")];
    if !populated_refusals.is_empty() {
        statements.push(format!(
            "DO $child_run_cutover$ BEGIN \
               IF EXISTS (SELECT 1 FROM {target}.runs WHERE {}) \
               THEN RAISE EXCEPTION USING ERRCODE = '55000', \
                    MESSAGE = 'durable-child-run-cutover-requires-no-child-or-wait-state'; \
               END IF; \
             END $child_run_cutover$",
            populated_refusals.join(" OR "),
        ));
    }
    statements.extend(
        RETIRED_CHILD_RUN_INDEXES
            .iter()
            .map(|index| format!("DROP INDEX IF EXISTS {target}.{}", quote_ident(index))),
    );
    if !drops.is_empty() {
        statements.push(format!("ALTER TABLE {target}.runs {}", drops.join(", ")));
    }
    statements.join("; ")
}

pub(super) fn partition_plane_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("run_queue").is_some_and(|columns| {
        RETIRED_PARTITION_COLUMNS
            .iter()
            .any(|column| columns.contains(*column))
    }) || RETIRED_PARTITION_TABLES
        .iter()
        .any(|table| obs.tables.contains_key(*table))
        || obs
            .checks
            .contains_key(&("run_queue".to_string(), RETIRED_PARTITION_CHECK.to_string()))
        || obs.indexes.contains_key(RETIRED_PARTITION_INDEX)
        || obs.retired_authored_ordering_rows != 0
}

pub(super) fn run_queue_claim_index_ready(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("run_queue").is_some_and(|columns| {
        RUN_QUEUE_CLAIMABLE_COLUMNS
            .iter()
            .all(|column| columns.contains(*column))
    })
}

pub(super) fn partition_plane_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
    let target = schema.quoted();
    let run_queue_present = obs.tables.contains_key("run_queue");
    let dead_letters_present = obs.tables.contains_key("run_dead_letters");
    let run_queue_lease_observable = obs.tables.get("run_queue").is_some_and(|columns| {
        columns.contains("lease_owner") && columns.contains("lease_expires_at")
    });
    let partition_owner_lease_observable = obs
        .tables
        .get("partition_owner")
        .is_some_and(|columns| columns.contains("lease_expires_at"));
    let flow_graph_observable = obs
        .tables
        .get("flows")
        .is_some_and(|columns| columns.contains("graph_json"));
    let lock_targets = ["run_queue", "partition_owner", "run_dead_letters", "flows"]
        .into_iter()
        .filter(|table| obs.tables.contains_key(*table))
        .map(|table| format!("{target}.{}", quote_ident(table)))
        .collect::<Vec<_>>();
    let mut statements = vec![format!(
        "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
        lock_targets.join(", ")
    )];

    if flow_graph_observable {
        statements.push(format!(
            "DO $retired_authored_ordering$ BEGIN \
               IF EXISTS (SELECT 1 FROM {target}.flows \
                           WHERE graph_json ? 'ordering' \
                              OR graph_json ? 'partition-policy') \
               THEN RAISE EXCEPTION USING ERRCODE = '55000', \
                    MESSAGE = '{RETIRED_AUTHORED_ORDERING_REFUSAL}'; \
               END IF; \
             END $retired_authored_ordering$"
        ));
    }

    let mut active_lease_checks = Vec::new();
    if run_queue_lease_observable {
        active_lease_checks.push(format!(
            "EXISTS (SELECT 1 FROM {target}.run_queue \
              WHERE lease_owner IS NOT NULL AND lease_expires_at > clock_timestamp())"
        ));
    } else if run_queue_present {
        statements.push(format!(
            "DO $unobservable_run_queue_lease$ BEGIN \
               IF EXISTS (SELECT 1 FROM {target}.run_queue) \
               THEN RAISE EXCEPTION USING ERRCODE = '55000', \
                    MESSAGE = 'partition-plane-cutover-requires-observable-run-queue-leases-or-empty-queue'; \
               END IF; \
             END $unobservable_run_queue_lease$"
        ));
    }
    if partition_owner_lease_observable {
        active_lease_checks.push(format!(
            "EXISTS (SELECT 1 FROM {target}.partition_owner \
              WHERE lease_expires_at > clock_timestamp())"
        ));
    } else if obs.tables.contains_key("partition_owner") {
        statements.push(format!(
            "DO $unobservable_partition_lease$ BEGIN \
               IF EXISTS (SELECT 1 FROM {target}.partition_owner) \
               THEN RAISE EXCEPTION USING ERRCODE = '55000', \
                    MESSAGE = 'partition-plane-cutover-requires-observable-partition-leases-or-empty-owner-table'; \
               END IF; \
             END $unobservable_partition_lease$"
        ));
    }
    if !active_lease_checks.is_empty() {
        statements.push(format!(
            "DO $partition_plane_drain$ BEGIN \
               IF {} THEN RAISE EXCEPTION USING ERRCODE = '55000', \
                    MESSAGE = 'partition-plane-cutover-requires-drained-workers'; \
               END IF; \
             END $partition_plane_drain$",
            active_lease_checks.join(" OR ")
        ));
    }
    if dead_letters_present {
        statements.push(format!(
            "DO $retired_dead_letters$ BEGIN \
               IF EXISTS (SELECT 1 FROM {target}.run_dead_letters) \
               THEN RAISE EXCEPTION USING ERRCODE = '55000', \
                    MESSAGE = '{RETIRED_DEAD_LETTER_REFUSAL}'; \
               END IF; \
             END $retired_dead_letters$"
        ));
    }
    if run_queue_present {
        statements.push(format!(
            "DROP INDEX IF EXISTS {target}.{RETIRED_PARTITION_INDEX}"
        ));
        statements.push(format!(
            "ALTER TABLE {target}.run_queue \
               DROP CONSTRAINT IF EXISTS {RETIRED_PARTITION_CHECK}, \
               DROP COLUMN IF EXISTS partition_key, \
               DROP COLUMN IF EXISTS partition_policy"
        ));
        if run_queue_claim_index_ready(obs) {
            statements.push(format!("DROP INDEX IF EXISTS {target}.run_queue_claimable"));
            statements.push(format!(
                "CREATE INDEX run_queue_claimable ON {target}.run_queue \
                   (tenant_id, available_at, stream_seq, run_id, lease_expires_at)"
            ));
        }
    }
    for table in RETIRED_PARTITION_TABLES {
        if obs.tables.contains_key(table) {
            statements.push(format!(
                "DROP TABLE IF EXISTS {target}.{}",
                quote_ident(table)
            ));
        }
    }
    statements.join("; ")
}

/// Retired authoring-test persistence, ordered child first. Two distinct
/// retirements share this cutover because they share one drop ordering:
///
/// * wamn-0h0g.8.10 removed the stored-suite plane (`test_suites` through
///   `authoring_suite_reports`).
/// * wamn-0h0g.15.27 removed `authoring_test_sets`; a draft carries its own
///   cases, so the separate content-addressed store has no producer. It is the
///   PARENT of the two FKs below, so it drops last.
pub(super) const RETIRED_STORED_SUITE_TABLES: [&str; 6] = [
    "authoring_suite_reports",
    "authoring_suite_case_facts",
    "authoring_report_reservations",
    "test_cases",
    "test_suites",
    "authoring_test_sets",
];

/// Helper functions retained only long enough for the cutovers above:
/// the first two by wamn-0h0g.8.10, the third by wamn-0h0g.15.27.
pub(super) const RETIRED_STORED_SUITE_FUNCTIONS: [&str; 3] = [
    "guard_authoring_report_write",
    "reject_immutable_authoring_report_change",
    "reject_immutable_authoring_test_set_change",
];
pub(super) const RETIRED_STORED_SUITE_CATALOG_TABLE: &str = "publish_gate_audit";

/// The RETAINED record tables that referenced `authoring_test_sets`. Their
/// `test_set_hash` column carries the FK, so the parent cannot be dropped while
/// it stands — and nothing else in the planner would ever remove it: the FK
/// reconciler repairs a fixed record list and has no drop-extra arm, and the
/// column is `NOT NULL` with no default, so leaving it would refuse every
/// reservation and report INSERT. `DROP COLUMN` takes the dependent FK with it.
pub(super) const RETIRED_TEST_SET_REFERENCE_TABLES: [&str; 2] =
    ["authoring_test_run_reservations", "authoring_test_reports"];
pub(super) const RETIRED_TEST_SET_REFERENCE_COLUMN: &str = "test_set_hash";

fn retired_test_set_reference_columns(obs: &RunPlaneObservation) -> Vec<&'static str> {
    RETIRED_TEST_SET_REFERENCE_TABLES
        .into_iter()
        .filter(|table| {
            obs.tables
                .get(*table)
                .is_some_and(|columns| columns.contains(RETIRED_TEST_SET_REFERENCE_COLUMN))
        })
        .collect()
}

pub(super) fn stored_suite_cutover_needed(obs: &RunPlaneObservation) -> bool {
    RETIRED_STORED_SUITE_TABLES
        .iter()
        .any(|table| obs.tables.contains_key(*table))
        || RETIRED_STORED_SUITE_FUNCTIONS
            .iter()
            .any(|function| obs.helper_functions.contains_key(*function))
        || obs
            .catalog_tables
            .contains(RETIRED_STORED_SUITE_CATALOG_TABLE)
        || !retired_test_set_reference_columns(obs).is_empty()
}

pub(super) fn stored_suite_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
    let mut statements = Vec::new();
    // The FK-carrying columns go FIRST: `authoring_test_sets` is the parent of
    // both, and a plain DROP TABLE on a referenced relation refuses. Dropping
    // the column takes its dependent FK with it, so no separate constraint drop
    // is emitted.
    for table in retired_test_set_reference_columns(obs) {
        statements.push(format!(
            "ALTER TABLE {}.{} DROP COLUMN IF EXISTS {}",
            schema.quoted(),
            quote_ident(table),
            quote_ident(RETIRED_TEST_SET_REFERENCE_COLUMN)
        ));
    }
    statements.extend(
        RETIRED_STORED_SUITE_TABLES
            .iter()
            .map(|table| {
                format!(
                    "DROP TABLE IF EXISTS {}.{}",
                    schema.quoted(),
                    quote_ident(table)
                )
            })
            .chain(RETIRED_STORED_SUITE_FUNCTIONS.iter().map(|function| {
                format!(
                    "DROP FUNCTION IF EXISTS {}.{}()",
                    schema.quoted(),
                    quote_ident(function)
                )
            }))
            .chain(std::iter::once(format!(
                "DROP TABLE IF EXISTS catalog.{}",
                quote_ident(RETIRED_STORED_SUITE_CATALOG_TABLE)
            ))),
    );
    statements.join("; ")
}

pub(super) const RETIRED_RERUN_LINEAGE_COLUMNS: &[&str] = &["replay_of", "root_run_id"];
pub(super) const RETIRED_FAILURE_DETAIL_COLUMNS: &[&str] = &["fail_node", "fail_reason"];
pub(super) const RETIRED_EXECUTION_BUNDLE_COLUMN: &str = "execution_bundle_hash";
const RETIRED_EFFECT_DISPOSITION_TABLES: [&str; 2] =
    ["effect_disposition_requests", "effect_dispositions"];
const RETIRED_EFFECT_DISPOSITION_HELPER: &str = "guard_effect_disposition_append";

pub(super) fn execution_bundle_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.catalog_tables.contains("execution_bundles")
        || obs
            .tables
            .get("runs")
            .is_some_and(|columns| columns.contains(RETIRED_EXECUTION_BUNDLE_COLUMN))
}

pub(super) fn execution_bundle_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
    let mut dependents = Vec::new();
    if obs.tables.contains_key("runs") {
        dependents.push(format!("{}.runs", schema.quoted()));
    }

    let mut statements = Vec::new();
    if !dependents.is_empty() {
        statements.push(format!(
            "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
            dependents.join(", ")
        ));
        statements.extend(dependents.iter().map(|table| {
            format!(
                "ALTER TABLE {table} DROP COLUMN IF EXISTS \
                 {RETIRED_EXECUTION_BUNDLE_COLUMN} RESTRICT"
            )
        }));
    }
    if obs.catalog_tables.contains("execution_bundles") {
        statements
            .push("LOCK TABLE catalog.execution_bundles IN ACCESS EXCLUSIVE MODE".to_string());
        statements.push(
            "-- Persisted bundle bytes are deliberately discarded without archive.\n\
             DROP TABLE catalog.execution_bundles RESTRICT"
                .to_string(),
        );
    }
    statements.join(";\n") + ";"
}

pub(super) fn retired_effect_disposition_cutover_needed(obs: &RunPlaneObservation) -> bool {
    RETIRED_EFFECT_DISPOSITION_TABLES
        .iter()
        .any(|table| obs.tables.contains_key(*table))
        || obs
            .helper_functions
            .contains_key(RETIRED_EFFECT_DISPOSITION_HELPER)
}

pub(super) fn retired_effect_disposition_cutover_sql(
    schema: &BareSchemaName,
    obs: &RunPlaneObservation,
) -> String {
    let present = RETIRED_EFFECT_DISPOSITION_TABLES
        .iter()
        .filter(|table| obs.tables.contains_key(**table))
        .copied()
        .collect::<Vec<_>>();
    let locks = if present.is_empty() {
        String::new()
    } else {
        format!(
            "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE; ",
            present
                .iter()
                .rev()
                .map(|table| format!("{}.{}", schema.quoted(), quote_ident(table)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let populated = present
        .iter()
        .map(|table| {
            format!(
                "EXISTS (SELECT 1 FROM {}.{})",
                schema.quoted(),
                quote_ident(table)
            )
        })
        .collect::<Vec<_>>();
    let preflight = if populated.is_empty() {
        String::new()
    } else {
        format!(
            "DO $retired_effect_disposition$ BEGIN IF {} THEN \
             RAISE EXCEPTION USING ERRCODE = '55000', MESSAGE = \
             'retired-effect-disposition-history-requires-archive-or-environment-reprovision'; \
             END IF; END $retired_effect_disposition$; ",
            populated.join(" OR ")
        )
    };
    format!(
        "{locks}{preflight}\
         DROP TABLE IF EXISTS {}.effect_dispositions; \
         DROP TABLE IF EXISTS {}.effect_disposition_requests; \
         DROP FUNCTION IF EXISTS {}.guard_effect_disposition_append()",
        schema.quoted(),
        schema.quoted(),
        schema.quoted()
    )
}

pub(super) fn rerun_lineage_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("runs").is_some_and(|columns| {
        RETIRED_RERUN_LINEAGE_COLUMNS
            .iter()
            .any(|column| columns.contains(*column))
    }) || obs.indexes.contains_key("runs_root")
}

pub(super) fn rerun_lineage_cutover_sql(schema: &BareSchemaName) -> String {
    let target = schema.quoted();
    let expected_index = rewrite_schema(RUNS_ROOT_INDEX_DEF, schema);
    format!(
        r#"LOCK TABLE {target}.runs IN ACCESS EXCLUSIVE MODE;
DO $rerun_lineage_cutover$
DECLARE
    observed_definition text;
    expected_definition constant text := '{expected_index}';
BEGIN
    SELECT pg_catalog.pg_get_indexdef(index_relation.oid)
      INTO observed_definition
      FROM pg_catalog.pg_class AS index_relation
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = index_relation.relnamespace
     WHERE namespace.nspname = '{schema}'
       AND index_relation.relname = 'runs_root';
    IF observed_definition IS NOT NULL
       AND observed_definition <> expected_definition THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'rerun-lineage-cutover-refuses-unknown-runs-root';
    END IF;
END
$rerun_lineage_cutover$;
DROP INDEX IF EXISTS {target}.runs_root;
ALTER TABLE {target}.runs
    DROP COLUMN IF EXISTS replay_of,
    DROP COLUMN IF EXISTS root_run_id;"#
    )
}

pub(super) fn failure_detail_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("runs").is_some_and(|columns| {
        RETIRED_FAILURE_DETAIL_COLUMNS
            .iter()
            .any(|column| columns.contains(*column))
    })
}

pub(super) fn failure_detail_cutover_sql(schema: &BareSchemaName) -> String {
    let target = schema.quoted();
    format!(
        r#"LOCK TABLE {target}.runs IN ACCESS EXCLUSIVE MODE;
-- wamn-0h0g.12.173/.12.175: populated retired failure-detail values are
-- deliberately discarded, not archived. fail_node names a deleted plan
-- coordinate, and fail_reason is superseded by fail_kind plus the typed caller
-- outcome; retaining either would preserve a dangling or duplicate record.
ALTER TABLE {target}.runs
    DROP COLUMN IF EXISTS fail_node RESTRICT,
    DROP COLUMN IF EXISTS fail_reason RESTRICT;"#
    )
}

pub(super) fn run_wiring_identity_contract_complete(obs: &RunPlaneObservation) -> bool {
    let Some(columns) = obs.tables.get("runs") else {
        return false;
    };
    [
        ("flow_id", "text"),
        ("flow_version", "integer"),
        ("wiring_id", "text"),
        ("wiring_version", "integer"),
        ("wiring_hash", "text"),
        ("binding_world_json", "jsonb"),
    ]
    .into_iter()
    .all(|(column, expected_type)| {
        let key = ("runs".to_string(), column.to_string());
        columns.contains(column)
            && !obs.non_nullable_columns.contains(&key)
            && obs
                .column_types
                .get(&key)
                .is_some_and(|actual| actual == expected_type)
    })
    // wamn-0h0g.8.5.6: the retired second identifier must be GONE, not merely
    // unmentioned. Stated as a positive absence because the cutover below is
    // what removes it: a database that still carries the column has not reached
    // the contract, so the reconcile must still plan the cutover for it.
    && !columns.contains("gate_report_id")
        && obs
        .checks
        .get(&("runs".to_string(), "runs_wiring_identity_check".to_string()))
        .is_some_and(|definition| definition == RUNS_WIRING_IDENTITY_CHECK_DEF)
        && obs
            .checks
            .get(&("runs".to_string(), "runs_execution_grain_check".to_string()))
            .is_some_and(|definition| definition == RUNS_EXECUTION_GRAIN_CHECK_DEF)
}

pub(super) fn wiring_identity_cutover_sql(schema: &BareSchemaName) -> String {
    let target = schema.quoted();
    format!(
        r#"LOCK TABLE {target}.runs IN ACCESS EXCLUSIVE MODE;
-- The carriers are ADDED in their own statement. PostgreSQL analyses every
-- subcommand of one ALTER TABLE against the PRE-EXISTING relation, so an
-- `ALTER COLUMN` beside the `ADD COLUMN` that creates it fails with 42703
-- `column "wiring_id" does not exist` — on exactly the legacy schemas this
-- cutover exists to upgrade, and never on one that already has them.
ALTER TABLE {target}.runs
    ADD COLUMN IF NOT EXISTS flow_id text,
    ADD COLUMN IF NOT EXISTS flow_version integer,
    ADD COLUMN IF NOT EXISTS wiring_id text,
    ADD COLUMN IF NOT EXISTS wiring_version integer,
    ADD COLUMN IF NOT EXISTS wiring_hash text,
    ADD COLUMN IF NOT EXISTS binding_world_json jsonb;
ALTER TABLE {target}.runs
    ALTER COLUMN flow_id TYPE text USING flow_id::text,
    ALTER COLUMN flow_version TYPE integer USING flow_version::integer,
    ALTER COLUMN wiring_id TYPE text USING wiring_id::text,
    ALTER COLUMN wiring_version TYPE integer USING wiring_version::integer,
    ALTER COLUMN wiring_hash TYPE text USING wiring_hash::text,
    ALTER COLUMN binding_world_json TYPE jsonb USING binding_world_json::jsonb,
    ALTER COLUMN flow_id DROP NOT NULL,
    ALTER COLUMN flow_version DROP NOT NULL,
    ALTER COLUMN wiring_id DROP NOT NULL,
    ALTER COLUMN wiring_version DROP NOT NULL,
    ALTER COLUMN wiring_hash DROP NOT NULL,
    ALTER COLUMN binding_world_json DROP NOT NULL,
    DROP CONSTRAINT IF EXISTS runs_wiring_identity_check,
    ADD CONSTRAINT runs_wiring_identity_check CHECK (
      (wiring_id IS NULL AND wiring_version IS NULL)
      OR (wiring_id IS NOT NULL AND wiring_version IS NOT NULL
          AND wiring_id <> '' AND wiring_version > 0)
    ),
    DROP CONSTRAINT IF EXISTS runs_execution_grain_check,
    ADD CONSTRAINT runs_execution_grain_check CHECK (
      (flow_id IS NOT NULL AND flow_version IS NOT NULL
       AND flow_id <> '' AND flow_version > 0
       AND wiring_hash IS NULL
       AND binding_world_json IS NULL)
      OR
      (flow_id IS NULL AND flow_version IS NULL
       AND wiring_id IS NOT NULL AND wiring_version IS NOT NULL
       AND wiring_id <> '' AND wiring_version > 0
       AND wiring_hash IS NOT NULL
       AND wiring_hash ~ '^sha256:[0-9a-f]{{64}}$'
       AND binding_world_json IS NOT NULL
       AND jsonb_typeof(binding_world_json) = 'array')
    );
-- The retired second identifier is DROPPED LAST, in its own statement
-- (wamn-0h0g.8.5.6). Both CHECKs above named it, so a `DROP COLUMN` beside them
-- would be analysed against the pre-existing relation and refuse with a
-- dependency error; dropping it after the constraints have been rewritten
-- leaves nothing referring to it. `IF EXISTS` is what makes this converge over
-- a database that already ran the cutover.
ALTER TABLE {target}.runs DROP COLUMN IF EXISTS gate_report_id;"#
    )
}
