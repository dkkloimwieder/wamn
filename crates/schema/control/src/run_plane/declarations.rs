//! Declared checks, helper functions, triggers, and grants for the run schema.

use std::borrow::Cow;

const RUNS_ADMISSION_SCOPE_CHECK_DEF: &str =
    "CHECK (package_id <> ''::text AND effective_release_id > 0 AND environment <> ''::text)";
pub(super) const RUNS_WIRING_IDENTITY_CHECK_DEF: &str = "CHECK (wiring_id IS NULL AND wiring_version IS NULL OR wiring_id IS NOT NULL AND wiring_version IS NOT NULL AND wiring_id <> ''::text AND wiring_version > 0)";
pub(super) const RUNS_EXECUTION_GRAIN_CHECK_DEF: &str = "CHECK (flow_id IS NOT NULL AND flow_version IS NOT NULL AND flow_id <> ''::text AND flow_version > 0 AND wiring_hash IS NULL AND binding_world_json IS NULL OR flow_id IS NULL AND flow_version IS NULL AND wiring_id IS NOT NULL AND wiring_version IS NOT NULL AND wiring_id <> ''::text AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND binding_world_json IS NOT NULL AND jsonb_typeof(binding_world_json) = 'array'::text)";
#[cfg(test)]
pub(super) const RUNS_RELEASE_FK_DEF: &str = "FOREIGN KEY (tenant_id, effective_release_id) REFERENCES catalog.effective_releases(tenant_id, effective_release_id)";
#[cfg(test)]
pub(super) const RUNS_RELEASE_INDEX_DEF: &str =
    "CREATE INDEX runs_release ON wamn_run.runs USING btree (tenant_id, effective_release_id)";
pub(super) const RUNS_ROOT_INDEX_DEF: &str = "CREATE INDEX runs_root ON wamn_run.runs USING btree (tenant_id, root_run_id) WHERE (root_run_id IS NOT NULL)";
pub(super) const RUNS_ADMISSION_PINS_TRIGGER_DEF: &str = "CREATE TRIGGER runs_admission_pins_immutable BEFORE UPDATE OF flow_id, flow_version, package_id, effective_release_id, environment, capture_mode, durability_class, wiring_id, wiring_version, wiring_hash, binding_world_json, manifest_digest ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable()";
/// The qual `wamn_run.environment_policies` must carry, as `pg_policy` renders
/// it. Re-keyed onto `current_user` with the rest of the guest-reachable floor
/// (`wamn-0h0g.22.6.3`); if this drifts from `deploy/sql/run-state.sql` the
/// reconciler REVERTS the sweep on every existing run-plane database.
pub(super) const ENVIRONMENT_POLICY_TENANT_QUAL: &str =
    "wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()";

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
/// The throwaway-PG gate applies the deploy SQL and pins that this catalog is a
/// byte-for-byte projection of the schema of record.
pub(super) const CHECK_SPECS: &[CheckSpec] = &[
    CheckSpec {
        table: "environment_policies",
        name: "environment_policies_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "environment_policies",
        name: "environment_policies_expected_environment_check",
        definition: "CHECK (expected_environment <> ''::text)",
        origin: CheckOrigin::Inline("expected_environment"),
    },
    CheckSpec {
        table: "environment_policies",
        name: "environment_policies_durability_class_check",
        definition: "CHECK (durability_class = ANY (ARRAY['standard'::text, 'durable'::text]))",
        origin: CheckOrigin::Inline("durability_class"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
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
        definition: "CHECK (status = ANY (ARRAY['dispatched'::text, 'running'::text, 'completed'::text, 'failed'::text, 'infrastructure-failure'::text, 'effect-uncertain'::text]))",
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
        name: "runs_durability_class_check",
        definition: "CHECK (durability_class = ANY (ARRAY['standard'::text, 'durable'::text]))",
        origin: CheckOrigin::Inline("durability_class"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_admission_context_version_check",
        definition: "CHECK (admission_context_version = '0.1'::text)",
        origin: CheckOrigin::Inline("admission_context_version"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_caller_outcome_kind_check",
        definition: "CHECK (caller_outcome_kind = ANY (ARRAY['responded'::text, 'failed'::text]))",
        origin: CheckOrigin::Inline("caller_outcome_kind"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_caller_http_status_check",
        definition: "CHECK (caller_http_status >= 100 AND caller_http_status <= 599)",
        origin: CheckOrigin::Inline("caller_http_status"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_fail_kind_check",
        definition: "CHECK (fail_kind = ANY (ARRAY['terminal'::text, 'retry-exhausted'::text, 'invalid-input'::text, 'runaway-budget'::text, 'effect-uncertain'::text, 'depth-budget'::text, 'dispatch-budget'::text, 'unresolvable-name'::text, 'hash-invalid-bytes'::text, 'foreign-revision'::text, 'incompatible-contract'::text, 'unbound-requirement'::text]))",
        origin: CheckOrigin::Inline("fail_kind"),
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
        definition: "CHECK ((caller_released_at IS NULL) = (caller_outcome_kind IS NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check7",
        definition: "CHECK (caller_outcome_kind IS NULL OR caller_outcome_json IS NOT NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check8",
        definition: "CHECK (caller_outcome_kind <> 'responded'::text OR caller_release_node_id IS NOT NULL)",
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
        table: "effect_attempts",
        name: "effect_attempts_tenant_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_root_plan_hash_check",
        definition: "CHECK (root_plan_hash ~ '^sha256:[0-9a-f]{64}$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_current_plan_hash_check",
        definition: "CHECK (current_plan_hash ~ '^sha256:[0-9a-f]{64}$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_frame_check",
        definition: "CHECK (frame_id >= 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_frame_relation_check",
        definition: "CHECK (frame_id = 0 AND parent_frame_id IS NULL AND call_site_id IS NULL OR frame_id > 0 AND parent_frame_id IS NOT NULL AND parent_frame_id >= 0 AND parent_frame_id < frame_id AND call_site_id IS NOT NULL AND call_site_id ~ '^[a-z0-9-]+$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_local_node_check",
        definition: "CHECK (local_node_id ~ '^[a-z0-9-]+$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_source_artifact_check",
        definition: "CHECK (source_artifact_hash ~ '^sha256:[0-9a-f]{64}$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_requirement_check",
        definition: "CHECK (requirement_name <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_occurrence_check",
        definition: "CHECK (occurrence >= 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_seq_check",
        definition: "CHECK (seq >= 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_generation_fact_check",
        definition: "CHECK (generation_fact_kind = ANY (ARRAY['not-required'::text, 'attested'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_generation_values_check",
        definition: "CHECK (generation_fact_kind = 'not-required'::text AND connection_name IS NULL AND connection_generation IS NULL AND credential_generation IS NULL OR generation_fact_kind = 'attested'::text AND connection_name IS NOT NULL AND connection_name <> ''::text AND connection_generation IS NOT NULL AND connection_generation <> ''::text AND credential_generation IS NOT NULL AND credential_generation <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_author_check",
        definition: "CHECK (verified_author_principal IS NULL OR verified_author_principal <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_publisher_check",
        definition: "CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_deadline_check",
        definition: "CHECK (attempt_started_at <= attempt_deadline_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_input_ref_check",
        definition: "CHECK (attempt_input_ref <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempt_dispatches",
        name: "effect_attempt_dispatches_tenant_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempt_dispatches",
        name: "effect_attempt_dispatches_frame_check",
        definition: "CHECK (frame_id >= 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempt_dispatches",
        name: "effect_attempt_dispatches_local_node_check",
        definition: "CHECK (local_node_id ~ '^[a-z0-9-]+$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempt_dispatches",
        name: "effect_attempt_dispatches_occurrence_check",
        definition: "CHECK (occurrence >= 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempt_dispatches",
        name: "effect_attempt_dispatches_time_check",
        definition: "CHECK (attempt_started_at <= dispatched_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempt_outcomes",
        name: "effect_attempt_outcomes_tenant_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempt_outcomes",
        name: "effect_attempt_outcomes_status_check",
        definition: "CHECK (outcome_status = ANY (ARRAY['success'::text, 'error'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempt_outcomes",
        name: "effect_attempt_outcomes_time_check",
        definition: "CHECK (dispatched_at <= recorded_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_tenant_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_correlation_check",
        definition: "CHECK (correlation_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_run_check",
        definition: "CHECK (run_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_kind_check",
        definition: "CHECK (action_kind = 'terminalize-effect-uncertain'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_basis_check",
        definition: "CHECK (basis = ANY (ARRAY['external-evidence'::text, 'counterparty-confirmation'::text, 'operator-judgment'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_evidence_check",
        definition: "CHECK (evidence_ref <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_principal_check",
        definition: "CHECK (principal <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_principal_kind_check",
        definition: "CHECK (principal_kind = 'database-role'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_prior_run_status_check",
        definition: "CHECK (prior_run_status = 'effect-uncertain'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "operator_run_actions",
        name: "operator_run_actions_prior_node_check",
        definition: "CHECK (prior_started_node_frame_id IS NULL AND prior_started_node_local_node_id IS NULL AND prior_started_node_occurrence IS NULL AND prior_started_node_status IS NULL OR prior_started_node_frame_id >= 0 AND prior_started_node_local_node_id IS NOT NULL AND prior_started_node_local_node_id ~ '^[a-z0-9-]+$'::text AND prior_started_node_occurrence >= 0 AND prior_started_node_status = 'started'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "run_queue",
        name: "run_queue_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "run_queue",
        name: "run_queue_lease_generation_check",
        definition: "CHECK (lease_generation >= 0)",
        origin: CheckOrigin::Inline("lease_generation"),
    },
];

const GUARD_EVENT_LINEAGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_event_lineage_immutable()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF NEW.event_source_run_id IS DISTINCT FROM OLD.event_source_run_id\n       OR NEW.event_root_run_id IS DISTINCT FROM OLD.event_root_run_id\n       OR NEW.event_depth IS DISTINCT FROM OLD.event_depth THEN\n        RAISE EXCEPTION 'event causation lineage is immutable';\n    END IF;\n    RETURN NEW;\nEND\n$function$\n";

const GUARD_RUN_ADMISSION_PINS_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_run_admission_pins_immutable()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF NEW.flow_id IS DISTINCT FROM OLD.flow_id\n       OR NEW.flow_version IS DISTINCT FROM OLD.flow_version\n       OR NEW.package_id IS DISTINCT FROM OLD.package_id\n       OR NEW.effective_release_id IS DISTINCT FROM OLD.effective_release_id\n       OR NEW.environment IS DISTINCT FROM OLD.environment\n       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode\n       OR NEW.durability_class IS DISTINCT FROM OLD.durability_class\n       OR NEW.wiring_id IS DISTINCT FROM OLD.wiring_id\n       OR NEW.wiring_version IS DISTINCT FROM OLD.wiring_version\n       OR NEW.wiring_hash IS DISTINCT FROM OLD.wiring_hash\n       OR NEW.binding_world_json IS DISTINCT FROM OLD.binding_world_json THEN\n        RAISE EXCEPTION USING\n            ERRCODE = '55000',\n            MESSAGE = 'run-admission-pin-immutable';\n    END IF;\n    IF OLD.manifest_digest IS NOT NULL THEN\n        IF NEW.manifest_digest IS NULL THEN\n            IF NEW.status NOT IN ('dispatched', 'running')\n               OR EXISTS (SELECT 1 FROM wamn_run.effect_attempts AS effect\n                           WHERE effect.tenant_id = OLD.tenant_id\n                             AND effect.run_id = OLD.run_id\n                             AND OLD.durability_class = 'durable') THEN\n                RAISE EXCEPTION USING\n                    ERRCODE = '55000',\n                    MESSAGE = 'run-release-record-immutable';\n            END IF;\n        ELSIF NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest THEN\n            RAISE EXCEPTION USING\n                ERRCODE = '55000',\n                MESSAGE = 'run-release-record-immutable';\n        END IF;\n    END IF;\n    RETURN NEW;\nEND\n$function$\n";

const GUARD_TERMINAL_RUN_DELETE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_terminal_run_delete()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF OLD.status NOT IN ('completed', 'failed', 'infrastructure-failure') THEN\n        RAISE EXCEPTION USING\n            ERRCODE = '55000',\n            MESSAGE = 'run-delete-nonterminal';\n    END IF;\n    RETURN OLD;\nEND\n$function$\n";

const REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_effect_fact_change()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    RAISE EXCEPTION USING\n        ERRCODE = '55000',\n        MESSAGE = 'effect-fact-immutable';\nEND\n$function$\n";

const REJECT_IMMUTABLE_OPERATOR_RUN_ACTION_CHANGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_operator_run_action_change()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    RAISE EXCEPTION USING\n        ERRCODE = '55000',\n        MESSAGE = 'operator-run-action-immutable';\nEND\n$function$\n";

const RUNS_EVENT_LINEAGE_TRIGGER_DEF: &str = "CREATE TRIGGER runs_event_lineage_immutable BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_event_lineage_immutable()";
const RUNS_TERMINAL_DELETE_ONLY_TRIGGER_DEF: &str = "CREATE TRIGGER runs_terminal_delete_only BEFORE DELETE ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_terminal_run_delete()";

const GUARD_EVENT_LINEAGE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_event_lineage_immutable()
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
$$;"#;

const GUARD_RUN_ADMISSION_PINS_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_run_admission_pins_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.flow_id IS DISTINCT FROM OLD.flow_id
       OR NEW.flow_version IS DISTINCT FROM OLD.flow_version
       OR NEW.package_id IS DISTINCT FROM OLD.package_id
       OR NEW.effective_release_id IS DISTINCT FROM OLD.effective_release_id
       OR NEW.environment IS DISTINCT FROM OLD.environment
       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode
       OR NEW.durability_class IS DISTINCT FROM OLD.durability_class
       OR NEW.wiring_id IS DISTINCT FROM OLD.wiring_id
       OR NEW.wiring_version IS DISTINCT FROM OLD.wiring_version
       OR NEW.wiring_hash IS DISTINCT FROM OLD.wiring_hash
       OR NEW.binding_world_json IS DISTINCT FROM OLD.binding_world_json THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-admission-pin-immutable';
    END IF;
    IF OLD.manifest_digest IS NOT NULL THEN
        IF NEW.manifest_digest IS NULL THEN
            IF NEW.status NOT IN ('dispatched', 'running')
               OR EXISTS (SELECT 1 FROM wamn_run.effect_attempts AS effect
                           WHERE effect.tenant_id = OLD.tenant_id
                             AND effect.run_id = OLD.run_id
                             AND OLD.durability_class = 'durable') THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'run-release-record-immutable';
            END IF;
        ELSIF NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest THEN
            RAISE EXCEPTION USING
                ERRCODE = '55000',
                MESSAGE = 'run-release-record-immutable';
        END IF;
    END IF;
    RETURN NEW;
END
$$;"#;

const GUARD_TERMINAL_RUN_DELETE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_terminal_run_delete()
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
REVOKE ALL ON FUNCTION wamn_run.guard_terminal_run_delete() FROM PUBLIC;"#;

const REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_effect_fact_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'effect-fact-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_effect_fact_change() FROM PUBLIC;"#;

pub(super) const REJECT_IMMUTABLE_OPERATOR_RUN_ACTION_CHANGE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_operator_run_action_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'operator-run-action-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_operator_run_action_change()
    FROM PUBLIC;"#;

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
    BEFORE UPDATE OF flow_id, flow_version, package_id, effective_release_id, environment, \
    capture_mode, durability_class, wiring_id, wiring_version, wiring_hash, \
    binding_world_json, manifest_digest \
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
        borrowed_helper_spec(
            "reject_immutable_effect_fact_change",
            REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_DEF,
            REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_SQL,
        ),
        borrowed_helper_spec(
            "reject_immutable_operator_run_action_change",
            REJECT_IMMUTABLE_OPERATOR_RUN_ACTION_CHANGE_DEF,
            REJECT_IMMUTABLE_OPERATOR_RUN_ACTION_CHANGE_SQL,
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
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        for event in ["update", "delete"] {
            let name = format!("{table}_{event}_immutable");
            let event_sql = event.to_ascii_uppercase();
            specs.push(TriggerSpec {
                table: table.to_string(),
                name: name.clone(),
                definition: format!(
                    "CREATE TRIGGER {name} BEFORE {event_sql} ON wamn_run.{table} \
                     FOR EACH ROW EXECUTE FUNCTION \
                     wamn_run.reject_immutable_effect_fact_change()"
                ),
                sql: format!(
                    "CREATE TRIGGER {name} BEFORE {event_sql} ON wamn_run.{table} \
                     FOR EACH ROW EXECUTE FUNCTION \
                     wamn_run.reject_immutable_effect_fact_change();"
                ),
            });
        }
    }
    for event in ["update", "delete"] {
        let name = format!("operator_run_actions_{event}_immutable");
        let event_sql = event.to_ascii_uppercase();
        specs.push(TriggerSpec {
            table: "operator_run_actions".to_string(),
            name: name.clone(),
            definition: format!(
                "CREATE TRIGGER {name} BEFORE {event_sql} ON \
                 wamn_run.operator_run_actions FOR EACH ROW EXECUTE FUNCTION \
                 wamn_run.reject_immutable_operator_run_action_change()"
            ),
            sql: format!(
                "CREATE TRIGGER {name} BEFORE {event_sql} ON \
                 wamn_run.operator_run_actions FOR EACH ROW EXECUTE FUNCTION \
                 wamn_run.reject_immutable_operator_run_action_change();"
            ),
        });
    }
    specs
}

pub(super) const EFFECT_DISPATCH_ATTEMPT_FK_NAME: &str = "effect_attempt_dispatches_attempt_fk";
pub(super) const EFFECT_DISPATCH_ATTEMPT_FK_DEF: &str = "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence) REFERENCES wamn_run.effect_attempts(tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)";
pub(super) const EFFECT_DISPATCH_ATTEMPT_FK_SQL: &str = "ALTER TABLE wamn_run.effect_attempt_dispatches \
     ADD CONSTRAINT effect_attempt_dispatches_attempt_fk \
     FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, \
                  run_id, frame_id, local_node_id, occurrence) \
     REFERENCES wamn_run.effect_attempts \
         (tenant_id, attempt_id, attempt_started_at, \
          run_id, frame_id, local_node_id, occurrence)";
pub(super) const EFFECT_OUTCOME_DISPATCH_FK_NAME: &str = "effect_attempt_outcomes_dispatch_fk";
pub(super) const EFFECT_OUTCOME_DISPATCH_FK_DEF: &str = "FOREIGN KEY (tenant_id, attempt_id, dispatched_at) REFERENCES wamn_run.effect_attempt_dispatches(tenant_id, attempt_id, dispatched_at)";
pub(super) const EFFECT_OUTCOME_DISPATCH_FK_SQL: &str = "ALTER TABLE wamn_run.effect_attempt_outcomes \
     ADD CONSTRAINT effect_attempt_outcomes_dispatch_fk \
     FOREIGN KEY (tenant_id, attempt_id, dispatched_at) \
     REFERENCES wamn_run.effect_attempt_dispatches \
         (tenant_id, attempt_id, dispatched_at)";
pub(super) const RETIRED_EFFECT_ATTEMPT_COLUMNS: &[&str] = &[
    "attempt_key",
    "attempt_index",
    "predecessor_attempt_id",
    "legacy_imported",
    "selected_recovery_class",
    "recovery_class",
];

pub(super) const EFFECT_FRAME_COLUMNS: &[&str] = &[
    "root_plan_hash",
    "current_plan_hash",
    "frame_id",
    "parent_frame_id",
    "call_site_id",
    "local_node_id",
    "source_artifact_hash",
    "requirement_name",
];

pub(super) const EFFECT_ATTEMPTS_OCCURRENCE_KEY_DEF: &str = "CREATE UNIQUE INDEX \
effect_attempts_occurrence_key ON wamn_run.effect_attempts USING btree \
(tenant_id, run_id, frame_id, local_node_id, occurrence)";
pub(super) const EFFECT_ATTEMPTS_DISPATCH_IDENTITY_KEY_DEF: &str = "CREATE UNIQUE INDEX \
effect_attempts_dispatch_identity_key ON wamn_run.effect_attempts USING btree \
(tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)";
pub(super) const EFFECT_DISPATCHES_OCCURRENCE_KEY_DEF: &str = "CREATE UNIQUE INDEX \
effect_attempt_dispatches_occurrence_key ON wamn_run.effect_attempt_dispatches USING btree \
(tenant_id, run_id, frame_id, local_node_id, occurrence)";
pub(super) const EFFECT_FRAME_CHECKS: &[&str] = &[
    "effect_attempts_root_plan_hash_check",
    "effect_attempts_current_plan_hash_check",
    "effect_attempts_frame_check",
    "effect_attempts_frame_relation_check",
    "effect_attempts_local_node_check",
    "effect_attempts_source_artifact_check",
    "effect_attempts_requirement_check",
];

#[derive(Clone, Copy)]
pub(super) enum AuthoringTableSchema {
    Catalog,
    RunPlane,
}

pub(super) struct AuthoringPrivilegeSpec {
    pub(super) schema: AuthoringTableSchema,
    pub(super) table: &'static str,
    pub(super) app: &'static [&'static str],
    pub(super) author: &'static [&'static str],
}

pub(super) const AUTHORING_PRIVILEGE_SPECS: &[AuthoringPrivilegeSpec] = &[
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "packages",
        app: &["SELECT"],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "package_migrations",
        app: &[],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "effective_releases",
        app: &["SELECT"],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "effective_release_packages",
        app: &["SELECT"],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "effective_release_heads",
        app: &["SELECT"],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_requirements",
        app: &["SELECT"],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_instances",
        app: &["SELECT"],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_generations",
        app: &["SELECT"],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_bindings",
        app: &["SELECT"],
        author: &[],
    },
    // `runs` has a dedicated column-grant reconciler because capture_mode is
    // admission-owned while the remaining run columns retain app writes.
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "environment_policies",
        app: &["SELECT"],
        // wamn-0h0g.22.43 removes the last two dormant project-plane reads.
        // Keeping this empty is the converge half of the static DDL revoke.
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "runs",
        app: &["SELECT", "DELETE"],
        author: &[],
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

pub(super) const EFFECT_WRITER_RUN_READ_COLUMNS: [(&str, &[&str]); 2] = [
    ("runs", &["tenant_id", "run_id", "status"]),
    (
        "run_queue",
        &[
            "tenant_id",
            "run_id",
            "lease_owner",
            "lease_expires_at",
            "lease_generation",
        ],
    ),
];
