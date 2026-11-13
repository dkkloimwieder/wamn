//! Declared checks, helper functions, triggers, and grants for the run schema.

use std::borrow::Cow;

const RUNS_ADMISSION_SCOPE_CHECK_DEF: &str =
    "CHECK (package_id <> ''::text AND environment <> ''::text)";
const RUNS_WIRING_IDENTITY_CHECK_DEF: &str = "CHECK (wiring_id IS NULL AND wiring_version IS NULL OR wiring_id IS NOT NULL AND wiring_version IS NOT NULL AND wiring_id <> ''::text AND wiring_version > 0)";
/// The execution identities of a run: a candidate case, an
/// automation run under a service principal, an event run admitted from a
/// registration with no caller (kept for stored rows), an event run under its
/// workflow principal, or
/// a manual or schedule workflow run under its executing principal and role.
const RUNS_EXECUTION_GRAIN_CHECK_DEF: &str = "CHECK (wiring_id IS NOT NULL AND wiring_version IS NOT NULL AND wiring_id <> ''::text AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND binding_world_json IS NOT NULL AND jsonb_typeof(binding_world_json) = 'array'::text OR trigger_source IS NOT NULL AND trigger_source = ANY (ARRAY['http'::text, 'internal'::text, 'studio'::text]) AND wiring_id IS NOT NULL AND wiring_version IS NOT NULL AND wiring_id <> ''::text AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND manifest_digest IS NOT NULL AND binding_world_json IS NULL OR NOT trigger_source IS DISTINCT FROM 'automation'::text AND service_principal_id IS NOT NULL AND wiring_id IS NOT NULL AND wiring_id <> ''::text AND wiring_version IS NOT NULL AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND binding_world_json IS NULL OR NOT trigger_source IS DISTINCT FROM 'event'::text AND registration_id IS NOT NULL AND registration_id <> ''::text AND service_principal_id IS NULL AND wiring_id IS NOT NULL AND wiring_id <> ''::text AND wiring_version IS NOT NULL AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND binding_world_json IS NULL OR NOT trigger_source IS DISTINCT FROM 'event'::text AND registration_id IS NULL AND service_principal_id IS NULL AND wiring_id IS NOT NULL AND wiring_id <> ''::text AND wiring_version IS NOT NULL AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND manifest_digest IS NOT NULL AND node_digests_json IS NOT NULL AND jsonb_typeof(node_digests_json) = 'object'::text AND execution_principal_id IS NOT NULL AND execution_role IS NOT NULL AND execution_role <> ''::text AND invoked_by IS NOT NULL AND invoked_by <> ''::text AND binding_world_json IS NULL OR NOT trigger_source IS DISTINCT FROM 'manual'::text AND registration_id IS NULL AND service_principal_id IS NULL AND wiring_id IS NOT NULL AND wiring_id <> ''::text AND wiring_version IS NOT NULL AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND manifest_digest IS NOT NULL AND node_digests_json IS NOT NULL AND jsonb_typeof(node_digests_json) = 'object'::text AND execution_principal_id IS NOT NULL AND execution_role IS NOT NULL AND execution_role <> ''::text AND NOT invoked_by IS DISTINCT FROM 'manual'::text AND invoked_by_user_id IS NOT NULL AND binding_world_json IS NULL OR NOT trigger_source IS DISTINCT FROM 'schedule'::text AND registration_id IS NULL AND service_principal_id IS NULL AND wiring_id IS NOT NULL AND wiring_id <> ''::text AND wiring_version IS NOT NULL AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND manifest_digest IS NOT NULL AND node_digests_json IS NOT NULL AND jsonb_typeof(node_digests_json) = 'object'::text AND execution_principal_id IS NOT NULL AND execution_role IS NOT NULL AND execution_role <> ''::text AND NOT invoked_by IS DISTINCT FROM 'schedule'::text AND invoked_by_user_id IS NULL AND binding_world_json IS NULL)";
/// Only an event, manual or schedule workflow run carries an execution identity, an
/// invoker and frozen node digests.
const RUNS_EXECUTION_IDENTITY_CHECK_DEF: &str = "CHECK (NOT trigger_source IS DISTINCT FROM 'manual'::text OR NOT trigger_source IS DISTINCT FROM 'schedule'::text OR NOT trigger_source IS DISTINCT FROM 'event'::text OR execution_principal_id IS NULL AND execution_role IS NULL AND invoked_by IS NULL AND invoked_by_user_id IS NULL AND node_digests_json IS NULL)";
pub(super) const RUNS_ADMISSION_PINS_TRIGGER_DEF: &str = "CREATE TRIGGER runs_admission_pins_immutable BEFORE UPDATE OF package_id, environment, capture_mode, wiring_id, wiring_version, wiring_hash, binding_world_json, manifest_digest, service_principal_id, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable()";

/// The qual `wamn_run.environment_policies` must carry, as `pg_policy` renders
/// it. Re-keyed onto `current_user` with the rest of the guest-reachable minimum
/// (`wamn-0h0g.22.6.3`); if this drifts from `deploy/sql/run-state.sql` the
/// reconciler REVERTS the sweep on every existing run-plane database.
pub(super) const ENVIRONMENT_POLICY_PROJECT_ENV_KEY_QUAL: &str =
    "wamn_authority.project_key(project_env_key) = wamn_authority.current_project_key()";

#[derive(Clone, Copy)]
pub(super) enum CheckOrigin {
    Inline(&'static str),
    Table,
}

#[derive(Clone, Copy)]
pub(super) struct CheckSpec {
    pub(super) table: &'static str,
    pub(super) name: &'static str,
    pub(super) definition: &'static str,
    pub(super) origin: CheckOrigin,
}

/// PostgreSQL 18's canonical CHECK constraint list for the four run-plane record
/// files. The live shell reads the same `pg_get_constraintdef(..., true)` form.
/// The throwaway-PG condition applies the deploy SQL and pins that this catalog is a
/// byte-for-byte projection of the schema of record.
pub(super) const CHECK_SPECS: &[CheckSpec] = &[
    CheckSpec {
        table: "environment_policies",
        name: "environment_policies_project_env_key_check",
        definition: "CHECK (project_env_key <> ''::text)",
        origin: CheckOrigin::Inline("project_env_key"),
    },
    CheckSpec {
        table: "environment_policies",
        name: "environment_policies_expected_environment_check",
        definition: "CHECK (expected_environment <> ''::text)",
        origin: CheckOrigin::Inline("expected_environment"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_project_env_key_check",
        definition: "CHECK (project_env_key <> ''::text)",
        origin: CheckOrigin::Inline("project_env_key"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_event_depth_check",
        definition: "CHECK (event_depth >= 0 AND event_depth <= 16)",
        origin: CheckOrigin::Inline("event_depth"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_status_check",
        definition: "CHECK (status = ANY (ARRAY['dispatched'::text, 'running'::text, 'completed'::text, 'failed'::text, 'infrastructure-failure'::text]))",
        origin: CheckOrigin::Inline("status"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_capture_mode_check",
        definition: "CHECK (capture_mode = ANY (ARRAY['full'::text, 'off'::text]))",
        origin: CheckOrigin::Inline("capture_mode"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_admission_context_version_check",
        definition: "CHECK (admission_context_version = '0.1'::text)",
        origin: CheckOrigin::Inline("admission_context_version"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_caller_outcome_type_check",
        definition: "CHECK (caller_outcome_type = ANY (ARRAY['responded'::text, 'failed'::text]))",
        origin: CheckOrigin::Inline("caller_outcome_type"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_caller_http_status_check",
        definition: "CHECK (caller_http_status >= 100 AND caller_http_status <= 599)",
        origin: CheckOrigin::Inline("caller_http_status"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_fail_type_check",
        definition: "CHECK (fail_type = ANY (ARRAY['terminal'::text, 'retry-exhausted'::text, 'invalid-input'::text, 'runaway-budget'::text, 'depth-budget'::text, 'dispatch-budget'::text, 'unresolvable-name'::text, 'hash-invalid-bytes'::text, 'foreign-revision'::text, 'incompatible-contract'::text, 'unbound-requirement'::text]))",
        origin: CheckOrigin::Inline("fail_type"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_check",
        definition: RUNS_ADMISSION_SCOPE_CHECK_DEF,
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_invocation_context_check",
        definition: "CHECK (jsonb_typeof(invocation_context) = 'object'::text AND octet_length(invocation_context::text) <= 16384)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check1",
        definition: "CHECK (event_source_run_id IS NULL AND event_root_run_id IS NULL AND event_depth IS NULL OR trigger_source = 'event'::text AND event_source_run_id IS NOT NULL AND event_source_run_id <> ''::text AND event_root_run_id IS NOT NULL AND event_root_run_id <> ''::text AND event_depth IS NOT NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check2",
        definition: "CHECK (event_depth IS DISTINCT FROM 0 OR event_source_run_id = run_id AND event_root_run_id = run_id)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check6",
        definition: "CHECK ((caller_released_at IS NULL) = (caller_outcome_type IS NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_updated_after_created",
        // The executor stamps `updated_at` from its own host clock (D2b) while
        // `created_at` keeps the server default, so a worker running behind the
        // database must fail the write instead of storing a backwards order.
        definition: "CHECK (updated_at >= created_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check7",
        definition: "CHECK (caller_outcome_type IS NULL OR caller_outcome_json IS NOT NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check8",
        definition: "CHECK (caller_outcome_type <> 'responded'::text OR caller_release_node_id IS NOT NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check9",
        definition: "CHECK (response_deadline_at IS NULL OR run_deadline_at IS NULL OR response_deadline_at <= run_deadline_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_capture_mode_source_check",
        // `pg_get_constraintdef` renders `IS NOT DISTINCT FROM` in this
        // equivalent canonical form.
        definition: "CHECK (capture_mode <> 'full'::text OR NOT trigger_source IS DISTINCT FROM 'scenario-draft'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_release_record_check",
        // The claim-time manifest record: absent, or well formed. The effective
        // release id is already an immutable admission pin. Table-origin and
        // explicitly named so it can never collide with the retired child-run
        // `runs_check3` numbering.
        definition: "CHECK (manifest_digest IS NULL OR manifest_digest ~ '^sha256:[0-9a-f]{64}$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_wiring_identity_check",
        definition: RUNS_WIRING_IDENTITY_CHECK_DEF,
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_execution_grain_check",
        definition: RUNS_EXECUTION_GRAIN_CHECK_DEF,
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_execution_identity_check",
        definition: RUNS_EXECUTION_IDENTITY_CHECK_DEF,
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_service_principal_check",
        definition: "CHECK ((NOT trigger_source IS DISTINCT FROM 'automation'::text) = (service_principal_id IS NOT NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "run_queue",
        name: "run_queue_project_env_key_check",
        definition: "CHECK (project_env_key <> ''::text)",
        origin: CheckOrigin::Inline("project_env_key"),
    },
    CheckSpec {
        table: "run_queue",
        name: "run_queue_lease_generation_check",
        definition: "CHECK (lease_generation >= 0)",
        origin: CheckOrigin::Inline("lease_generation"),
    },
];

const GUARD_EVENT_LINEAGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_event_lineage_immutable()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF NEW.event_source_run_id IS DISTINCT FROM OLD.event_source_run_id\n       OR NEW.event_root_run_id IS DISTINCT FROM OLD.event_root_run_id\n       OR NEW.event_depth IS DISTINCT FROM OLD.event_depth THEN\n        RAISE EXCEPTION 'event causation lineage is immutable';\n    END IF;\n    RETURN NEW;\nEND\n$function$\n";

const GUARD_RUN_ADMISSION_PINS_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_run_admission_pins_immutable()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF NEW.package_id IS DISTINCT FROM OLD.package_id\n       OR NEW.environment IS DISTINCT FROM OLD.environment\n       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode\n       OR NEW.wiring_id IS DISTINCT FROM OLD.wiring_id\n       OR NEW.wiring_version IS DISTINCT FROM OLD.wiring_version\n       OR NEW.wiring_hash IS DISTINCT FROM OLD.wiring_hash\n       OR NEW.binding_world_json IS DISTINCT FROM OLD.binding_world_json\n       OR NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest\n       OR NEW.service_principal_id IS DISTINCT FROM OLD.service_principal_id\n       OR NEW.execution_principal_id IS DISTINCT FROM OLD.execution_principal_id\n       OR NEW.execution_role IS DISTINCT FROM OLD.execution_role\n       OR NEW.invoked_by IS DISTINCT FROM OLD.invoked_by\n       OR NEW.invoked_by_user_id IS DISTINCT FROM OLD.invoked_by_user_id\n       OR NEW.node_digests_json IS DISTINCT FROM OLD.node_digests_json THEN\n        RAISE EXCEPTION USING\n            ERRCODE = '55000',\n            MESSAGE = 'run-admission-pin-immutable';\n    END IF;\n    RETURN NEW;\nEND\n$function$\n";

const GUARD_TERMINAL_RUN_DELETE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_terminal_run_delete()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF OLD.status NOT IN ('completed', 'failed', 'infrastructure-failure') THEN\n        RAISE EXCEPTION USING\n            ERRCODE = '55000',\n            MESSAGE = 'run-delete-nonterminal';\n    END IF;\n    RETURN OLD;\nEND\n$function$\n";

const RUNS_EVENT_LINEAGE_TRIGGER_DEF: &str = "CREATE TRIGGER runs_event_lineage_immutable BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_event_lineage_immutable()";
const RUNS_TERMINAL_DELETE_ONLY_TRIGGER_DEF: &str = "CREATE TRIGGER runs_terminal_delete_only BEFORE DELETE ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_terminal_run_delete()";

const GUARD_EVENT_LINEAGE_SQL: &str = r"CREATE OR REPLACE FUNCTION wamn_run.guard_event_lineage_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.event_source_run_id IS DISTINCT FROM OLD.event_source_run_id
       OR NEW.event_root_run_id IS DISTINCT FROM OLD.event_root_run_id
       OR NEW.event_depth IS DISTINCT FROM OLD.event_depth THEN
        RAISE EXCEPTION 'event causation lineage is immutable';
    END IF;
    RETURN NEW;
END
$$;";

const GUARD_RUN_ADMISSION_PINS_SQL: &str = r"CREATE OR REPLACE FUNCTION wamn_run.guard_run_admission_pins_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.package_id IS DISTINCT FROM OLD.package_id
       OR NEW.environment IS DISTINCT FROM OLD.environment
       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode
       OR NEW.wiring_id IS DISTINCT FROM OLD.wiring_id
       OR NEW.wiring_version IS DISTINCT FROM OLD.wiring_version
       OR NEW.wiring_hash IS DISTINCT FROM OLD.wiring_hash
       OR NEW.binding_world_json IS DISTINCT FROM OLD.binding_world_json
       OR NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest
       OR NEW.service_principal_id IS DISTINCT FROM OLD.service_principal_id
       OR NEW.execution_principal_id IS DISTINCT FROM OLD.execution_principal_id
       OR NEW.execution_role IS DISTINCT FROM OLD.execution_role
       OR NEW.invoked_by IS DISTINCT FROM OLD.invoked_by
       OR NEW.invoked_by_user_id IS DISTINCT FROM OLD.invoked_by_user_id
       OR NEW.node_digests_json IS DISTINCT FROM OLD.node_digests_json THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-admission-pin-immutable';
    END IF;
    RETURN NEW;
END
$$;";

const GUARD_TERMINAL_RUN_DELETE_SQL: &str = r"CREATE OR REPLACE FUNCTION wamn_run.guard_terminal_run_delete()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.status NOT IN ('completed', 'failed', 'infrastructure-failure') THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-delete-nonterminal';
    END IF;
    RETURN OLD;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_terminal_run_delete() FROM PUBLIC;";

const RUNS_EVENT_LINEAGE_TRIGGER_SQL: &str = "CREATE TRIGGER runs_event_lineage_immutable \
    BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth \
    ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION \
    wamn_run.guard_event_lineage_immutable();";
const RUNS_TERMINAL_DELETE_ONLY_TRIGGER_SQL: &str = "CREATE TRIGGER \
    runs_terminal_delete_only BEFORE DELETE ON wamn_run.runs FOR EACH ROW \
    EXECUTE FUNCTION wamn_run.guard_terminal_run_delete();";

/// The ONE encoding of the admission-pin trigger's `CREATE` (wamn-0h0g.20.9).
///
/// The steady-state trigger repair emits this exact frozen column list.
pub(super) const RUNS_ADMISSION_PINS_TRIGGER_SQL: &str = "CREATE TRIGGER runs_admission_pins_immutable \
    BEFORE UPDATE OF package_id, environment, \
    capture_mode, wiring_id, wiring_version, wiring_hash, \
    binding_world_json, manifest_digest, service_principal_id, \
    execution_principal_id, execution_role, invoked_by, invoked_by_user_id, \
    node_digests_json \
    ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION \
    wamn_run.guard_run_admission_pins_immutable();";

pub(super) struct HelperSpec {
    pub(super) name: &'static str,
    pub(super) definition: Cow<'static, str>,
    pub(super) sql: Cow<'static, str>,
}

fn borrowed_helper_spec(
    name: &'static str,
    definition: &'static str,
    sql: &'static str,
) -> HelperSpec {
    HelperSpec {
        name,
        definition: Cow::Borrowed(definition),
        sql: Cow::Borrowed(sql),
    }
}

pub(super) fn helper_specs() -> Vec<HelperSpec> {
    vec![
        borrowed_helper_spec(
            "guard_event_lineage_immutable",
            GUARD_EVENT_LINEAGE_DEF,
            GUARD_EVENT_LINEAGE_SQL,
        ),
        borrowed_helper_spec(
            "guard_run_admission_pins_immutable",
            GUARD_RUN_ADMISSION_PINS_DEF,
            GUARD_RUN_ADMISSION_PINS_SQL,
        ),
        borrowed_helper_spec(
            "guard_terminal_run_delete",
            GUARD_TERMINAL_RUN_DELETE_DEF,
            GUARD_TERMINAL_RUN_DELETE_SQL,
        ),
    ]
}

#[derive(Debug)]
pub(super) struct TriggerSpec {
    pub(super) table: String,
    pub(super) name: String,
    pub(super) definition: String,
    pub(super) sql: String,
}

/// Run-plane tables that carry the four record history stamps (wamn-jesy3.3).
pub(super) const RECORD_HISTORY_TABLES: [&str; 2] = ["environment_policies", "run_queue"];

pub(super) fn trigger_specs() -> Vec<TriggerSpec> {
    let mut specs = vec![
        TriggerSpec {
            table: "runs".to_string(),
            name: "runs_event_lineage_immutable".to_string(),
            definition: RUNS_EVENT_LINEAGE_TRIGGER_DEF.to_string(),
            sql: RUNS_EVENT_LINEAGE_TRIGGER_SQL.to_string(),
        },
        TriggerSpec {
            table: "runs".to_string(),
            name: "runs_admission_pins_immutable".to_string(),
            definition: RUNS_ADMISSION_PINS_TRIGGER_DEF.to_string(),
            sql: RUNS_ADMISSION_PINS_TRIGGER_SQL.to_string(),
        },
        TriggerSpec {
            table: "runs".to_string(),
            name: "runs_terminal_delete_only".to_string(),
            definition: RUNS_TERMINAL_DELETE_ONLY_TRIGGER_DEF.to_string(),
            sql: RUNS_TERMINAL_DELETE_ONLY_TRIGGER_SQL.to_string(),
        },
    ];
    for table in RECORD_HISTORY_TABLES {
        let definition = format!(
            "CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON wamn_run.{table} \
             FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by')"
        );
        specs.push(TriggerSpec {
            table: table.to_string(),
            name: "wamn_record_history_stamp".to_string(),
            sql: format!("{definition};"),
            definition,
        });
    }
    specs
}

#[derive(Clone, Copy)]
pub(super) enum AuthoringTableSchema {
    Catalog,
    RunPlane,
}

pub(super) struct AuthoringPrivilegeSpec {
    pub(super) schema: AuthoringTableSchema,
    pub(super) table: &'static str,
    pub(super) app: &'static [&'static str],
}

pub(super) const AUTHORING_PRIVILEGE_SPECS: &[AuthoringPrivilegeSpec] = &[
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "packages",
        app: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "package_migrations",
        app: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "releases",
        app: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_requirement",
        app: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "release_connection",
        app: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_binding",
        app: &["SELECT"],
    },
    // `runs` has a dedicated column-grant reconciler because capture_mode is
    // admission-owned while the remaining run columns retain app writes.
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "environment_policies",
        app: &["SELECT"],
        // wamn-0h0g.22.43 removes the last two dormant project-plane reads.
        // Keeping this empty is the converge half of the static DDL revoke.
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "runs",
        app: &["SELECT", "DELETE"],
    },
];

pub(super) const TABLE_PRIVILEGE_TYPES: [&str; 7] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "TRUNCATE",
    "REFERENCES",
    "TRIGGER",
];
