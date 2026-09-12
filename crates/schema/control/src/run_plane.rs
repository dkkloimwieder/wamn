//! The run-plane schema reconciler (E4/R14-migration, wamn-1wdq).
//!
//! `deploy/sql/run-state.sql` / `run-queue.sql` evolve, but a
//! schema instantiated from an older revision has NO migration path: the 2jkm.41
//! sweep found live demo schemas missing the E4 `stream_seq` column (runner
//! drains failed 42703), the whole queue table
//! (`poc_f1` predated per-project queue provisioning), and — after the ephemeral
//! fixture pod restarted — everything at once, including the `catalog` metadata
//! schema. This module is the PURE decision (the reconcile-replica-identity
//! precedent — no DB, clock, or wasm): given what the driver OBSERVED live
//! (tables + columns + indexes + CHECKs + user triggers + helper functions +
//! legacy outbox-era objects + the `catalog` schema state), it produces the
//! idempotent plan that
//! brings one project-env's run-plane schema to the schema of record. The
//! `wamn-ctl reconcile-run-plane` shell reads/executes; the throwaway-PG gate
//! shows the live transitions.
//!
//! **The schema of record is the deploy/sql source itself**, embedded at compile
//! time (`include_str!`) — the SAME files the wamn-gates `schema_drift` guard
//! (wamn-9mg8) pins — and sliced per table, so the plan can never drift from
//! what provisioning applies. Per-project schemas are the `wamn_run` → target
//! rewrite (`rewrite_schema`, the project-environment provisioning convention).
//!
//! What the plan covers (the wamn-1wdq manifestation set):
//!
//! 1. **Additive column drift** — a present table missing record columns gains
//!    `ALTER TABLE … ADD COLUMN <record definition>` (e.g. E4 `stream_seq
//!    bigint NOT NULL DEFAULT 0`).
//! 2. **Index drift** — a record index absent live is created; a present one
//!    whose live definition lacks a record column the record definition names
//!    (the pre-E4 `run_queue_claimable` without `stream_seq`) is dropped and
//!    recreated from record.
//! 3. **Wholly-missing tables** — created from their record section (DDL +
//!    indexes + RLS + policy + grants), in file order so FKs resolve.
//! 4. **The pre-l5i9.19 outbox era** — legacy `outbox`/`evt_shadow` tables, the
//!    constant-named `wamn_outbox_event` trigger (per entity table) and its
//!    function are DROPPED (trigger before function — the function drop is
//!    RESTRICT), and stored registrations carrying the legacy `state` or
//!    `partition-key` key are stripped (a legacy document fails parse after
//!    the owning surface is removed).
//! 5. **From-zero restore** — an empty database plans the full set, including
//!    `wamn_catalog::CATALOG_SCHEMA_SQL` (the `catalog` metadata schema the
//!    registration storage and the RI reconcile read).
//! 6. **Exact CHECK + trigger convergence** — every record-table CHECK is
//!    compared in PostgreSQL's canonical form; missing/drifted checks are added
//!    or replaced and non-record checks are removed. The run-state helper
//!    functions and lineage trigger are likewise repaired from record.
//!
//! **Retained-data preserving:** the plan never rewrites or deletes a retained
//! row or drops a retained table. Unknown live columns are SURFACED
//! (`extra_columns`) and preserved. Explicit cutovers physically remove only
//! named retired state after locked safety preflights. The partition-plane
//! cutover requires drained leases and refuses nonempty dead-letter history;
//! the effect-writer cutovers remove retired identity/recovery columns; the
//! rerun-lineage cutover removes only the two retired run columns and its
//! canonical index while preserving every run row; and the stored-test
//! cutover removes retired persistence. PostgreSQL
//! validates new CHECKs against existing rows and aborts on incompatible data.

mod observation;
mod observation_sql;
mod schema;

#[doc(inline)]
pub use observation::{
    EffectWriterRoleObservation, RowPolicyObservation, RowSecurityObservation, RunPlaneObservation,
    ScenarioAuthorRoleObservation,
};
#[doc(inline)]
pub use observation_sql::{
    catalog_schema_present_sql, count_retired_authored_ordering_rows_sql,
    count_stale_registration_keys_sql, select_app_run_queue_authority_sql,
    select_app_scenario_author_membership_sql, select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_dispatch_reader_schema_privileges_sql,
    select_dispatch_reader_table_privileges_sql,
    select_effect_ledger_effective_column_privileges_sql,
    select_effect_ledger_effective_privileges_sql, select_effect_ledger_table_privileges_sql,
    select_effect_writer_role_sql, select_effect_writer_run_column_privileges_sql,
    select_effect_writer_run_table_privileges_sql, select_effect_writer_schema_privileges_sql,
    select_environment_policy_policies_sql, select_environment_policy_row_security_sql,
    select_outbox_function_present_sql, select_outbox_trigger_tables_sql,
    select_run_capture_privileges_sql, select_scenario_author_role_sql,
    select_scenario_author_schema_usage_sql, select_schema_checks_sql, select_schema_columns_sql,
    select_schema_foreign_keys_sql, select_schema_indexes_sql, select_schema_triggers_sql,
};
#[doc(inline)]
pub use schema::{BareSchemaName, InvalidBareSchemaName, rewrite_schema};

#[cfg(test)]
use schema::header_section;
use schema::{
    index_definition_stale, index_statements, normalize_observed_schema, quote_ident,
    record_columns, record_table_names, record_tables, schema_header_section, table_section,
    table_section_carries_trigger,
};

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use wamn_catalog::CATALOG_SCHEMA_SQL;

/// The schema of record, compiled in — the same sources provisioning applies
/// (project-environment provisioning and the f1 provisioning Job) and the wamn-9mg8
/// stand-in drift guard pins.
const RUN_STATE_SQL: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../deploy/sql/run-queue.sql");

const RUNS_ADMISSION_SCOPE_CHECK_DEF: &str =
    "CHECK (package_id <> ''::text AND effective_release_id > 0 AND environment <> ''::text)";
const RUNS_WIRING_IDENTITY_CHECK_DEF: &str = "CHECK (wiring_id IS NULL AND wiring_version IS NULL OR wiring_id IS NOT NULL AND wiring_version IS NOT NULL AND wiring_id <> ''::text AND wiring_version > 0)";
const RUNS_EXECUTION_GRAIN_CHECK_DEF: &str = "CHECK (flow_id IS NOT NULL AND flow_version IS NOT NULL AND flow_id <> ''::text AND flow_version > 0 AND wiring_hash IS NULL AND binding_world_json IS NULL OR flow_id IS NULL AND flow_version IS NULL AND wiring_id IS NOT NULL AND wiring_version IS NOT NULL AND wiring_id <> ''::text AND wiring_version > 0 AND wiring_hash IS NOT NULL AND wiring_hash ~ '^sha256:[0-9a-f]{64}$'::text AND binding_world_json IS NOT NULL AND jsonb_typeof(binding_world_json) = 'array'::text)";
#[cfg(test)]
const RUNS_RELEASE_FK_DEF: &str = "FOREIGN KEY (tenant_id, effective_release_id) REFERENCES catalog.effective_releases(tenant_id, effective_release_id)";
#[cfg(test)]
const RUNS_RELEASE_INDEX_DEF: &str =
    "CREATE INDEX runs_release ON wamn_run.runs USING btree (tenant_id, effective_release_id)";
const RUNS_ROOT_INDEX_DEF: &str = "CREATE INDEX runs_root ON wamn_run.runs USING btree (tenant_id, root_run_id) WHERE (root_run_id IS NOT NULL)";
const RUNS_ADMISSION_PINS_TRIGGER_DEF: &str = "CREATE TRIGGER runs_admission_pins_immutable BEFORE UPDATE OF flow_id, flow_version, package_id, effective_release_id, environment, capture_mode, durability_class, wiring_id, wiring_version, wiring_hash, binding_world_json, manifest_digest ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable()";
/// The qual `wamn_run.environment_policies` must carry, as `pg_policy` renders
/// it. Re-keyed onto `current_user` with the rest of the guest-reachable floor
/// (`wamn-0h0g.22.6.3`); if this drifts from `deploy/sql/run-state.sql` the
/// reconciler REVERTS the sweep on every existing run-plane database.
const ENVIRONMENT_POLICY_TENANT_QUAL: &str =
    "wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()";

#[derive(Clone, Copy)]
enum CheckOrigin {
    Inline(&'static str),
    Table,
}

#[derive(Clone, Copy)]
struct CheckSpec {
    table: &'static str,
    name: &'static str,
    definition: &'static str,
    origin: CheckOrigin,
}

/// PostgreSQL 18's canonical CHECK constraint list for the four run-plane record
/// files. The live shell reads the same `pg_get_constraintdef(..., true)` form.
/// The throwaway-PG gate applies the deploy SQL and pins that this catalog is a
/// byte-for-byte projection of the schema of record.
const CHECK_SPECS: &[CheckSpec] = &[
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

const REJECT_IMMUTABLE_OPERATOR_RUN_ACTION_CHANGE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_operator_run_action_change()
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
const RUNS_ADMISSION_PINS_TRIGGER_SQL: &str = "CREATE TRIGGER runs_admission_pins_immutable \
    BEFORE UPDATE OF flow_id, flow_version, package_id, effective_release_id, environment, \
    capture_mode, durability_class, wiring_id, wiring_version, wiring_hash, \
    binding_world_json, manifest_digest \
    ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION \
    wamn_run.guard_run_admission_pins_immutable();";

struct HelperSpec {
    name: &'static str,
    definition: Cow<'static, str>,
    sql: Cow<'static, str>,
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

fn helper_specs() -> Vec<HelperSpec> {
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
struct TriggerSpec {
    table: String,
    name: String,
    definition: String,
    sql: String,
}

fn trigger_specs() -> Vec<TriggerSpec> {
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

const EFFECT_DISPATCH_ATTEMPT_FK_NAME: &str = "effect_attempt_dispatches_attempt_fk";
const EFFECT_DISPATCH_ATTEMPT_FK_DEF: &str = "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence) REFERENCES wamn_run.effect_attempts(tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)";
const EFFECT_DISPATCH_ATTEMPT_FK_SQL: &str = "ALTER TABLE wamn_run.effect_attempt_dispatches \
     ADD CONSTRAINT effect_attempt_dispatches_attempt_fk \
     FOREIGN KEY (tenant_id, attempt_id, attempt_started_at, \
                  run_id, frame_id, local_node_id, occurrence) \
     REFERENCES wamn_run.effect_attempts \
         (tenant_id, attempt_id, attempt_started_at, \
          run_id, frame_id, local_node_id, occurrence)";
const EFFECT_OUTCOME_DISPATCH_FK_NAME: &str = "effect_attempt_outcomes_dispatch_fk";
const EFFECT_OUTCOME_DISPATCH_FK_DEF: &str = "FOREIGN KEY (tenant_id, attempt_id, dispatched_at) REFERENCES wamn_run.effect_attempt_dispatches(tenant_id, attempt_id, dispatched_at)";
const EFFECT_OUTCOME_DISPATCH_FK_SQL: &str = "ALTER TABLE wamn_run.effect_attempt_outcomes \
     ADD CONSTRAINT effect_attempt_outcomes_dispatch_fk \
     FOREIGN KEY (tenant_id, attempt_id, dispatched_at) \
     REFERENCES wamn_run.effect_attempt_dispatches \
         (tenant_id, attempt_id, dispatched_at)";
const RETIRED_EFFECT_ATTEMPT_COLUMNS: &[&str] = &[
    "attempt_key",
    "attempt_index",
    "predecessor_attempt_id",
    "legacy_imported",
    "selected_recovery_class",
    "recovery_class",
];

const EFFECT_FRAME_COLUMNS: &[&str] = &[
    "root_plan_hash",
    "current_plan_hash",
    "frame_id",
    "parent_frame_id",
    "call_site_id",
    "local_node_id",
    "source_artifact_hash",
    "requirement_name",
];

const EFFECT_ATTEMPTS_OCCURRENCE_KEY_DEF: &str = "CREATE UNIQUE INDEX \
effect_attempts_occurrence_key ON wamn_run.effect_attempts USING btree \
(tenant_id, run_id, frame_id, local_node_id, occurrence)";
const EFFECT_ATTEMPTS_DISPATCH_IDENTITY_KEY_DEF: &str = "CREATE UNIQUE INDEX \
effect_attempts_dispatch_identity_key ON wamn_run.effect_attempts USING btree \
(tenant_id, attempt_id, attempt_started_at, run_id, frame_id, local_node_id, occurrence)";
const EFFECT_DISPATCHES_OCCURRENCE_KEY_DEF: &str = "CREATE UNIQUE INDEX \
effect_attempt_dispatches_occurrence_key ON wamn_run.effect_attempt_dispatches USING btree \
(tenant_id, run_id, frame_id, local_node_id, occurrence)";
const EFFECT_FRAME_CHECKS: &[&str] = &[
    "effect_attempts_root_plan_hash_check",
    "effect_attempts_current_plan_hash_check",
    "effect_attempts_frame_check",
    "effect_attempts_frame_relation_check",
    "effect_attempts_local_node_check",
    "effect_attempts_source_artifact_check",
    "effect_attempts_requirement_check",
];

fn effect_writer_cutover_owned_check(table: &str, name: &str) -> bool {
    table == "effect_attempts"
        && matches!(
            name,
            "effect_attempts_attempt_index_check"
                | "effect_attempts_lineage_check"
                | "effect_attempts_recovery_class_check"
                | "effect_attempts_key_check"
        )
}

fn frame_identity_column(table: &str, column: &str) -> bool {
    table == "effect_attempts" && (EFFECT_FRAME_COLUMNS.contains(&column) || column == "node_id")
}

fn frame_identity_check(table: &str, name: &str) -> bool {
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

fn postgres_visible_identifier(name: &str) -> &str {
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
struct FrameIdentityCutoverTargets {
    effect: bool,
    dispatch: bool,
    restore_dispatch_fk: bool,
}

impl FrameIdentityCutoverTargets {
    const fn needed(self) -> bool {
        self.effect
    }

    fn includes_table(self, table: &str) -> bool {
        table == "effect_attempts" && self.effect
    }
}

fn frame_identity_cutover_targets(
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

fn frame_identity_cutover_sql(
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

fn effect_writer_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
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

fn effect_writer_ledger_cutover_needed(schema: &BareSchemaName, obs: &RunPlaneObservation) -> bool {
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

/// The run-plane record files in APPLY ORDER: run-state first (schema header +
/// `runs`, which everything FKs), then the queue.
const RUN_PLANE_FILES: [&str; 2] = [RUN_STATE_SQL, RUN_QUEUE_SQL];

#[derive(Clone, Copy)]
enum AuthoringTableSchema {
    Catalog,
    RunPlane,
}

struct AuthoringPrivilegeSpec {
    schema: AuthoringTableSchema,
    table: &'static str,
    app: &'static [&'static str],
    author: &'static [&'static str],
}

const AUTHORING_PRIVILEGE_SPECS: &[AuthoringPrivilegeSpec] = &[
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

const TABLE_PRIVILEGE_TYPES: [&str; 7] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "TRUNCATE",
    "REFERENCES",
    "TRIGGER",
];

const RETIRED_PARTITION_COLUMNS: [&str; 2] = ["partition_key", "partition_policy"];
const RETIRED_PARTITION_TABLES: [&str; 2] = ["partition_owner", "run_dead_letters"];
const RETIRED_PARTITION_CHECK: &str = "run_queue_partition_policy_check";
const RETIRED_PARTITION_INDEX: &str = "run_queue_partition";
const RETIRED_AUTHORED_ORDERING_REFUSAL: &str =
    "retired-authored-ordering-requires-environment-reprovision";
/// Stable operator-facing refusal for retained history that cannot be cut over.
const RETIRED_DEAD_LETTER_REFUSAL: &str =
    "retired-run-dead-letter-history-requires-archive-or-environment-reprovision";
const RUN_QUEUE_CLAIMABLE_COLUMNS: [&str; 5] = [
    "tenant_id",
    "available_at",
    "stream_seq",
    "run_id",
    "lease_expires_at",
];

const RETIRED_CHILD_RUN_COLUMNS: [&str; 8] = [
    "parent_run_id",
    "parent_node_id",
    "parent_occurrence",
    "waiting_child_run_id",
    "waiting_child_occurrence",
    "wait_generation",
    "invoke_depth",
    "invoke_root_run_id",
];
const RETIRED_CHILD_RUN_INDEXES: [&str; 3] = [
    "runs_parent_occurrence",
    "runs_invoke_root",
    "runs_waiting_child",
];

fn child_run_cutover_needed(obs: &RunPlaneObservation) -> bool {
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

fn retired_child_run_check(definition: &str) -> bool {
    RETIRED_CHILD_RUN_COLUMNS
        .iter()
        .any(|column| definition.contains(column))
}

fn child_run_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
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

fn partition_plane_cutover_needed(obs: &RunPlaneObservation) -> bool {
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

fn run_queue_claim_index_ready(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("run_queue").is_some_and(|columns| {
        RUN_QUEUE_CLAIMABLE_COLUMNS
            .iter()
            .all(|column| columns.contains(*column))
    })
}

fn partition_plane_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
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
const RETIRED_STORED_SUITE_TABLES: [&str; 6] = [
    "authoring_suite_reports",
    "authoring_suite_case_facts",
    "authoring_report_reservations",
    "test_cases",
    "test_suites",
    "authoring_test_sets",
];

/// Helper functions retained only long enough for the cutovers above:
/// the first two by wamn-0h0g.8.10, the third by wamn-0h0g.15.27.
const RETIRED_STORED_SUITE_FUNCTIONS: [&str; 3] = [
    "guard_authoring_report_write",
    "reject_immutable_authoring_report_change",
    "reject_immutable_authoring_test_set_change",
];
const RETIRED_STORED_SUITE_CATALOG_TABLE: &str = "publish_gate_audit";

/// The RETAINED record tables that referenced `authoring_test_sets`. Their
/// `test_set_hash` column carries the FK, so the parent cannot be dropped while
/// it stands — and nothing else in the planner would ever remove it: the FK
/// reconciler repairs a fixed record list and has no drop-extra arm, and the
/// column is `NOT NULL` with no default, so leaving it would refuse every
/// reservation and report INSERT. `DROP COLUMN` takes the dependent FK with it.
const RETIRED_TEST_SET_REFERENCE_TABLES: [&str; 2] =
    ["authoring_test_run_reservations", "authoring_test_reports"];
const RETIRED_TEST_SET_REFERENCE_COLUMN: &str = "test_set_hash";

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

fn stored_suite_cutover_needed(obs: &RunPlaneObservation) -> bool {
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

fn stored_suite_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
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

/// The outbox-era tables the l5i9.19 teardown retired. A pre-teardown schema
/// (or one restored from a pre-teardown snapshot) still carries them.
pub const LEGACY_OUTBOX_TABLES: [&str; 2] = ["outbox", "evt_shadow"];

/// The constant trigger and function name the retired outbox emission used
/// (`CREATE OR REPLACE TRIGGER wamn_outbox_event … EXECUTE FUNCTION
/// wamn_outbox_event()`, one trigger per entity table, the function unqualified
/// so it landed in the apply-time schema).
pub const OUTBOX_TRIGGER_NAME: &str = "wamn_outbox_event";

/// Reserved non-login project-author identity. No production credential inherits
/// it, and the guest-visible `wamn_app` role is never a member.
pub const SCENARIO_AUTHOR_ROLE: &str = "wamn_scenario_author";
/// Stable NOLOGIN ACL role inherited only by scoped writer generations.
pub const EFFECT_WRITER_ROLE: &str = "wamn_effect_writer";
const EFFECT_WRITER_RUN_READ_COLUMNS: [(&str, &[&str]); 2] = [
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

fn environment_policy_row_security_at_record() -> RowSecurityObservation {
    RowSecurityObservation {
        enabled: true,
        forced: true,
        policies: BTreeMap::from([
            (
                "environment_policies_tenant".to_string(),
                RowPolicyObservation {
                    command: "select".to_string(),
                    permissive: true,
                    roles: BTreeSet::from(["wamn_app".to_string()]),
                    using_expression: Some(ENVIRONMENT_POLICY_TENANT_QUAL.to_string()),
                    check_expression: None,
                },
            ),
            (
                "environment_policies_platform".to_string(),
                RowPolicyObservation {
                    command: "select".to_string(),
                    permissive: true,
                    roles: BTreeSet::from(["wamn_platform".to_string()]),
                    using_expression: Some("true".to_string()),
                    check_expression: None,
                },
            ),
        ]),
    }
}

/// What one plan action does (for reporting; the SQL is on the action).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPlaneActionKind {
    /// Create or harden the host-only `wamn_scenario_author` NOLOGIN role.
    EnsureScenarioAuthorRole,
    /// `CREATE SCHEMA IF NOT EXISTS` + role usage grant (the run-state.sql
    /// header, rewritten) — emitted once when any run-plane table is missing.
    EnsureSchema,
    /// Create a missing run-plane table from its record section.
    CreateTable,
    /// Add a record column missing from a present table.
    AddColumn,
    /// Discard the retired mutable node projection and its database-local ACLs.
    RetireNodeRuns,
    /// Drop the retired plan digest columns, then the plan-byte table itself.
    RetireExecutionBundles,
    /// Strict empty-only conversion from legacy effect identity to frames.
    FrameIdentityCutover,
    /// Delete retired callable/event storage and its definer lock bridge.
    RetireLegacyAdmissionSurface,
    /// Delete the retired partition plane after a locked drain/evidence preflight.
    PartitionPlaneCutover,
    /// Delete retired durable child, wait, and invoke-depth run state.
    ChildRunCutover,
    /// Delete retired replay/root run lineage while preserving every run row.
    RerunLineageCutover,
    /// Delete retired per-node failure detail while preserving every run row.
    FailureDetailCutover,
    /// Delete retired stored-suite tables, audit relation, and helper functions.
    StoredSuiteCutover,
    /// Empty-only deletion of the retired effect-disposition request/outcome plane.
    RetiredEffectDispositionCutover,
    /// Strict empty-only installation of the coordinate-bound writer ledgers.
    EffectWriterCutover,
    /// Refuse a provisioning-owned stable writer role outside its frozen shape.
    VerifyEffectWriterRole,
    /// Converge exact stable-writer schema/table ACLs and deny other writers.
    RepairEffectWriterPrivilege,
    /// Drop/re-add a drifted record CHECK, or add it when absent.
    RepairConstraint,
    /// Drop/re-add a missing or drifted named record foreign key.
    RepairForeignKey,
    /// Enable + force RLS and replace the projected env-policy policy set.
    RepairRowSecurity,
    /// Remove a CHECK on a record table that is absent from the schema of record.
    DropExtraConstraint,
    /// Create or replace a missing/drifted run-state helper function.
    RepairHelperFunction,
    /// Drop/recreate a missing/drifted user trigger from the schema of record.
    RepairTrigger,
    /// Remove a user trigger on a record table that is absent from the record.
    DropExtraTrigger,
    /// Create a record index absent from a present table.
    CreateIndex,
    /// Drop + recreate a present index whose live definition lost a record
    /// column (the pre-E4 claimable index).
    RecreateIndex,
    /// Drop a legacy outbox-era table.
    DropLegacyTable,
    /// Drop a legacy `wamn_outbox_event` trigger from one table.
    DropLegacyTrigger,
    /// Drop the legacy `wamn_outbox_event()` function (after its triggers).
    DropLegacyFunction,
    /// Apply the complete catalog bootstrap when the `catalog` schema is absent.
    EnsureCatalogSchema,
    /// Create a missing `catalog` table from its record section.
    CreateCatalogTable,
    /// Converge authoring-state schema/table grants and remove guest write
    /// authority or membership in the host-only role.
    RepairAuthoringPrivilege,
    /// Replace broad application-role run grants with column grants that omit
    /// the admission-owned `runs.capture_mode` carrier.
    RepairRunCapturePrivilege,
    /// Converge the dispatcher read principal's in-database surface on exactly
    /// schema `USAGE` plus `SELECT` on the two relations it reads, narrowing a
    /// widened reader back (wamn-0h0g.12.123).
    ///
    /// This is the ONE run-plane privilege the pure planner does not build: its
    /// grant text comes from `wamn_control_provision`, and the effect shell
    /// appends the action. See `wamn_ctl::reconcile_run_plane`.
    RepairDispatchReaderPrivilege,
    /// Remove every guest-visible table and column privilege on `run_queue`.
    RemoveAppRunQueueAuthority,
    /// Strip retired keys from stored registrations.
    StripRetiredRegistrationKeys,
}

/// One reconcile action: the SQL to run and what it targets (for reporting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlaneAction {
    pub kind: RunPlaneActionKind,
    /// The table / index / object the action targets (reporting label).
    pub target: String,
    pub sql: String,
}

/// The reconcile plan: ordered actions, plus the record tables already fully at
/// target (reported, never executed) and unknown live columns the record does
/// not know (SURFACED and preserved). Named retired columns owned by an
/// explicit cutover are not reported as extras. Idempotent: planning against
/// the post-apply state yields no actions.
#[derive(Debug, Clone, Default)]
pub struct RunPlanePlan {
    pub actions: Vec<RunPlaneAction>,
    /// Run-plane record tables present live with full column + index parity.
    pub at_target: Vec<String>,
    /// `(table, column)` unknown live columns not in the record — untouched.
    pub extra_columns: Vec<(String, String)>,
}

const RETIRED_RERUN_LINEAGE_COLUMNS: &[&str] = &["replay_of", "root_run_id"];
const RETIRED_FAILURE_DETAIL_COLUMNS: &[&str] = &["fail_node", "fail_reason"];
const RETIRED_EXECUTION_BUNDLE_COLUMN: &str = "execution_bundle_hash";
const RETIRED_EFFECT_DISPOSITION_TABLES: [&str; 2] =
    ["effect_disposition_requests", "effect_dispositions"];
const RETIRED_EFFECT_DISPOSITION_HELPER: &str = "guard_effect_disposition_append";

fn execution_bundle_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.catalog_tables.contains("execution_bundles")
        || obs
            .tables
            .get("runs")
            .is_some_and(|columns| columns.contains(RETIRED_EXECUTION_BUNDLE_COLUMN))
}

fn execution_bundle_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
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

fn retired_effect_disposition_cutover_needed(obs: &RunPlaneObservation) -> bool {
    RETIRED_EFFECT_DISPOSITION_TABLES
        .iter()
        .any(|table| obs.tables.contains_key(*table))
        || obs
            .helper_functions
            .contains_key(RETIRED_EFFECT_DISPOSITION_HELPER)
}

fn retired_effect_disposition_cutover_sql(
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

fn rerun_lineage_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("runs").is_some_and(|columns| {
        RETIRED_RERUN_LINEAGE_COLUMNS
            .iter()
            .any(|column| columns.contains(*column))
    }) || obs.indexes.contains_key("runs_root")
}

fn rerun_lineage_cutover_sql(schema: &BareSchemaName) -> String {
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

fn failure_detail_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("runs").is_some_and(|columns| {
        RETIRED_FAILURE_DETAIL_COLUMNS
            .iter()
            .any(|column| columns.contains(*column))
    })
}

fn failure_detail_cutover_sql(schema: &BareSchemaName) -> String {
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

impl RunPlanePlan {
    /// Whether there is anything to apply (a no-op reconcile is the expected
    /// steady state and worth reporting as such).
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }
}

fn run_wiring_identity_contract_complete(obs: &RunPlaneObservation) -> bool {
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

fn wiring_identity_cutover_sql(schema: &BareSchemaName) -> String {
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

/// The convergent mirror of `deploy/sql/run-state.sql`'s `runs` ACL block.
///
/// The `wamn_run_retention` arm (`wamn-0h0g.12.69`) is EXISTENCE-GUARDED rather
/// than emitted bare: the stable retention ACL role is created by the schema
/// header section and by provisioning, and a repair that names a role the target
/// cluster has not been bootstrapped with would fail the whole batch instead of
/// converging the privileges it CAN. Its `REVOKE`s precede its `GRANT` for the
/// same reason every other arm's do — the batch must narrow a widened grant, not
/// merely add the intended one — and BOTH a column-wide and a table-wide
/// `REVOKE` are issued, because a table-level `GRANT SELECT` and a per-column
/// one are separate ACL entries and revoking either alone leaves the other
/// standing. The re-grant is column-scoped to exactly the three columns the
/// prune statement's `WHERE` clause reads: retention is a `wamn_platform`
/// member, that group's floor arm is `USING (true)`, and a table-level `SELECT`
/// would therefore expose every tenant's run payload columns.
fn repair_run_capture_privilege_sql(
    schema: &BareSchemaName,
    available_columns: impl IntoIterator<Item = String>,
) -> String {
    let available_columns = available_columns.into_iter().collect::<BTreeSet<_>>();
    debug_assert!(available_columns.contains("capture_mode"));
    let all_columns = available_columns
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    let qualified = format!("{}.runs", schema.quoted());
    format!(
        "LOCK TABLE {qualified} IN ACCESS EXCLUSIVE MODE; \
         REVOKE SELECT ({all_columns}), INSERT ({all_columns}), \
                UPDATE ({all_columns}), REFERENCES ({all_columns}) \
           ON TABLE {qualified} FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}; \
         REVOKE ALL PRIVILEGES ON TABLE {qualified} \
           FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}; \
         GRANT SELECT, DELETE ON TABLE {qualified} TO wamn_app; \
         DO $run_retention_acl$ BEGIN \
           IF EXISTS (SELECT FROM pg_catalog.pg_roles \
                       WHERE rolname = 'wamn_run_retention') THEN \
             EXECUTE 'REVOKE ALL PRIVILEGES ({all_columns}) ON TABLE {qualified} \
                        FROM wamn_run_retention'; \
             EXECUTE 'REVOKE ALL PRIVILEGES ON TABLE {qualified} FROM wamn_run_retention'; \
             EXECUTE 'GRANT SELECT (\"tenant_id\", \"status\", \"created_at\"), DELETE \
                        ON TABLE {qualified} TO wamn_run_retention'; \
           END IF; \
         END $run_retention_acl$; \
         DO $run_capture_acl$ BEGIN \
           IF EXISTS ( \
                SELECT 1 \
                  FROM unnest(ARRAY['wamn_app','{SCENARIO_AUTHOR_ROLE}']) actor, \
                       unnest(ARRAY['INSERT','UPDATE']) privilege \
                 WHERE pg_catalog.has_any_column_privilege( \
                   actor, '{qualified}', privilege)) \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM pg_catalog.pg_class relation \
                     JOIN pg_catalog.pg_namespace namespace \
                       ON namespace.oid = relation.relnamespace \
                     JOIN pg_catalog.pg_attribute attribute \
                       ON attribute.attrelid = relation.oid \
                      AND attribute.attname = 'capture_mode' \
                     CROSS JOIN LATERAL \
                       pg_catalog.aclexplode(attribute.attacl) acl \
                    WHERE relation.oid = pg_catalog.to_regclass('{qualified}') \
                      AND acl.grantee = 0 \
                      AND acl.privilege_type IN ('INSERT','UPDATE')) \
              OR NOT pg_catalog.has_table_privilege( \
                   'wamn_app', '{qualified}', 'SELECT') \
              OR NOT pg_catalog.has_table_privilege( \
                   'wamn_app', '{qualified}', 'DELETE') \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM unnest(ARRAY[ \
                       'INSERT','UPDATE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                    WHERE pg_catalog.has_table_privilege( \
                      'wamn_app', '{qualified}', privilege)) \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM unnest(ARRAY[ \
                       'SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                    WHERE pg_catalog.has_table_privilege( \
                      '{SCENARIO_AUTHOR_ROLE}', '{qualified}', privilege)) \
              OR pg_catalog.has_any_column_privilege( \
                   '{SCENARIO_AUTHOR_ROLE}', '{qualified}', \
                   'SELECT,INSERT,UPDATE,REFERENCES') \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM pg_catalog.pg_class relation \
                     CROSS JOIN LATERAL pg_catalog.aclexplode( \
                       COALESCE(relation.relacl, \
                                pg_catalog.acldefault('r', relation.relowner))) acl \
                    WHERE relation.oid = pg_catalog.to_regclass('{qualified}') \
                      AND acl.grantee = 0) \
              OR (SELECT owner.rolname \
                    FROM pg_catalog.pg_class relation \
                    JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner \
                   WHERE relation.oid = pg_catalog.to_regclass('{qualified}')) \
                   IN ('wamn_app', '{SCENARIO_AUTHOR_ROLE}') \
           THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                MESSAGE = 'run-capture-author-sql-write-authority'; \
           END IF; \
         END $run_capture_acl$"
    )
}

fn run_capture_privileges_drifted(schema: &BareSchemaName, obs: &RunPlaneObservation) -> bool {
    if !obs.tables.contains_key("runs") {
        return false;
    }

    let expected = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<BTreeSet<_>>()
    };
    let key = |grantee: &str| {
        (
            schema.as_str().to_string(),
            "runs".to_string(),
            grantee.to_string(),
        )
    };
    let observed =
        |map: &BTreeMap<_, _>, grantee: &str| map.get(&key(grantee)).cloned().unwrap_or_default();

    observed(&obs.authoring_table_privileges, "PUBLIC") != BTreeSet::new()
        || observed(&obs.authoring_table_privileges, "wamn_app") != expected(&["SELECT", "DELETE"])
        || observed(&obs.authoring_table_privileges, SCENARIO_AUTHOR_ROLE) != BTreeSet::new()
        || observed(&obs.authoring_effective_table_privileges, "wamn_app")
            != expected(&["SELECT", "DELETE"])
        || observed(
            &obs.authoring_effective_table_privileges,
            SCENARIO_AUTHOR_ROLE,
        ) != BTreeSet::new()
        || observed(&obs.authoring_effective_column_privileges, "wamn_app") != expected(&["SELECT"])
        || observed(
            &obs.authoring_effective_column_privileges,
            SCENARIO_AUTHOR_ROLE,
        ) != BTreeSet::new()
        || obs
            .authoring_table_owners
            .get(&(schema.as_str().to_string(), "runs".to_string()))
            .is_some_and(|owner| owner == "wamn_app" || owner == SCENARIO_AUTHOR_ROLE)
        || obs.app_run_capture_privileges.0
        || obs.app_run_capture_privileges.1
        || !obs.app_run_capture_privileges.2
}

/// The generation-role floor the PROVISIONER mints, restated as a refusal.
///
/// The membership edge term is `SET FALSE`, not `SET TRUE`: `INHERIT TRUE, SET
/// FALSE` gives the login generation the stable ACL role's privileges while
/// denying it `SET ROLE` to BECOME that role — the "rotating login generations
/// with no `SET ROLE` escape" of `docs/exe-model.md`, and the posture that keeps
/// `current_user` an honest RLS input. `wamn-0h0g.12.178`: this predicate was
/// authored against the `SET TRUE` grant of `f0d18024`; `358f6792` tightened the
/// provisioner's grant, its state probe, and its own violation check to `SET
/// FALSE` without reaching here, leaving the reconciler demanding the LOOSER
/// shape and refusing every environment `provision-project-env --prepare`
/// actually mints. `provisioner_minted_generation_leg` is the arm that meets
/// the real prepared shape.
fn generation_role_contract_violation_sql() -> &'static str {
    "EXISTS ( \
         SELECT 1 FROM pg_catalog.pg_roles AS generation \
          WHERE generation.rolname ~ '^wamn_effect_writer_[0-9a-f]{40}_[ab]$' \
            AND (pg_catalog.has_database_privilege( \
                   generation.oid, current_database(), 'CONNECT') \
                 OR EXISTS ( \
                      SELECT 1 FROM pg_catalog.pg_auth_members AS edge \
                      JOIN pg_catalog.pg_roles AS parent ON parent.oid = edge.roleid \
                     WHERE edge.member = generation.oid \
                       AND parent.rolname = 'wamn_effect_writer')) \
            AND (NOT generation.rolcanlogin OR generation.rolsuper \
                 OR generation.rolcreatedb OR generation.rolcreaterole \
                 OR NOT generation.rolinherit OR generation.rolreplication \
                 OR generation.rolbypassrls \
                 OR NOT EXISTS ( \
                      SELECT 1 FROM pg_catalog.pg_auth_members AS edge \
                      JOIN pg_catalog.pg_roles AS parent ON parent.oid = edge.roleid \
                     WHERE edge.member = generation.oid \
                       AND parent.rolname = 'wamn_effect_writer') \
                 OR EXISTS ( \
                      SELECT 1 FROM pg_catalog.pg_auth_members AS edge \
                      JOIN pg_catalog.pg_roles AS parent ON parent.oid = edge.roleid \
                     WHERE edge.member = generation.oid \
                       AND parent.rolname NOT IN ( \
                             'wamn_effect_writer', 'wamn_run_projection_writer')) \
                 OR EXISTS ( \
                      SELECT 1 FROM pg_catalog.pg_auth_members AS edge \
                       WHERE edge.member = generation.oid \
                         AND (edge.admin_option OR NOT edge.inherit_option \
                              OR edge.set_option)) \
                 OR EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members AS edge \
                             WHERE edge.roleid = generation.oid) \
                 OR EXISTS (SELECT 1 FROM pg_catalog.pg_shdepend AS dependency \
                             WHERE dependency.refclassid = 'pg_authid'::regclass \
                               AND dependency.refobjid = generation.oid \
                               AND dependency.deptype = 'o'))) \
       OR (SELECT count(*) FROM pg_catalog.pg_roles AS generation \
            WHERE generation.rolname ~ '^wamn_effect_writer_[0-9a-f]{40}_[ab]$' \
              AND pg_catalog.has_database_privilege( \
                    generation.oid, current_database(), 'CONNECT')) > 2 \
       OR (SELECT count(DISTINCT substring( \
                    generation.rolname FROM \
                    '^wamn_effect_writer_([0-9a-f]{40})_[ab]$')) \
             FROM pg_catalog.pg_roles AS generation \
            WHERE generation.rolname ~ '^wamn_effect_writer_[0-9a-f]{40}_[ab]$' \
              AND pg_catalog.has_database_privilege( \
                    generation.oid, current_database(), 'CONNECT')) > 1"
}

/// Reconcile one project-env's run-plane schema (+ the per-database `catalog`
/// metadata schema) against the schema of record. Pure: `obs` is what the
/// driver read; the returned plan is what it should execute, in order.
pub fn plan_run_plane(schema: &BareSchemaName, obs: &RunPlaneObservation) -> RunPlanePlan {
    let mut plan = RunPlanePlan::default();
    if obs.tables.contains_key("node_runs") {
        let target = schema.quoted();
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RetireNodeRuns,
            target: format!("{}.node_runs", schema.as_str()),
            sql: format!(
                r#"LOCK TABLE {target}.node_runs IN ACCESS EXCLUSIVE MODE;
-- Populated rows are deliberately discarded without archive: node_runs held
-- only dead mutable projection coordinates, while runs retains run history.
DROP TABLE IF EXISTS {target}.node_runs RESTRICT;
DO $retire_run_projection_authority$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_roles
                WHERE rolname = 'wamn_run_projection_writer') THEN
        EXECUTE pg_catalog.format(
            'REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA %I FROM wamn_run_projection_writer',
            '{schema}'
        );
        EXECUTE pg_catalog.format(
            'REVOKE ALL PRIVILEGES ON SCHEMA %I FROM wamn_run_projection_writer',
            '{schema}'
        );
    END IF;
END
$retire_run_projection_authority$;"#,
                schema = schema.as_str(),
            ),
        });
        return plan;
    }
    if execution_bundle_cutover_needed(obs) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RetireExecutionBundles,
            target: "catalog.execution-bundles".to_string(),
            sql: execution_bundle_cutover_sql(schema, obs),
        });
        return plan;
    }
    let has_runs = obs.tables.contains_key("runs");
    let capture_mode_present = obs
        .tables
        .get("runs")
        .is_some_and(|columns| columns.contains("capture_mode"));
    let run_capture_privileges_drifted = run_capture_privileges_drifted(schema, obs);
    let wiring_identity_cutover_needed = has_runs && !run_wiring_identity_contract_complete(obs);

    let effect_writer_ledger_cutover_needed = effect_writer_ledger_cutover_needed(schema, obs);
    if effect_writer_ledger_cutover_needed && obs.effect_ledger_rows != 0 {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EffectWriterCutover,
            target: "effect-ledgers.coordinate-writer-boundary".to_string(),
            sql: effect_writer_cutover_sql(schema, obs),
        });
        return plan;
    }

    let failure_detail_cutover_needed = failure_detail_cutover_needed(obs);
    let partition_plane_cutover_needed = partition_plane_cutover_needed(obs);
    if partition_plane_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::PartitionPlaneCutover,
            target: "run_queue.partition-plane".to_string(),
            sql: partition_plane_cutover_sql(schema, obs),
        });
    }

    let child_run_cutover_needed = child_run_cutover_needed(obs);
    if child_run_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::ChildRunCutover,
            target: "runs.durable-child-state".to_string(),
            sql: child_run_cutover_sql(schema, obs),
        });
    }

    let rerun_lineage_cutover_needed = rerun_lineage_cutover_needed(obs);
    if rerun_lineage_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RerunLineageCutover,
            target: "runs.rerun-lineage".to_string(),
            sql: rerun_lineage_cutover_sql(schema),
        });
    }

    let stored_suite_cutover_needed = stored_suite_cutover_needed(obs);
    if stored_suite_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::StoredSuiteCutover,
            target: "stored-suite-persistence".to_string(),
            sql: stored_suite_cutover_sql(schema, obs),
        });
    }

    if retired_effect_disposition_cutover_needed(obs) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RetiredEffectDispositionCutover,
            target: "effect-disposition-persistence".to_string(),
            sql: retired_effect_disposition_cutover_sql(schema, obs),
        });
    }

    if obs
        .effect_writer_role
        .is_none_or(|role| !role.is_acl_only())
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::VerifyEffectWriterRole,
            target: EFFECT_WRITER_ROLE.to_string(),
            sql: format!("DO $effect_writer_role$ \
                  DECLARE role_oid oid; \
                  BEGIN \
                    SELECT oid INTO role_oid FROM pg_catalog.pg_roles \
                     WHERE rolname = 'wamn_effect_writer' AND NOT rolcanlogin \
                       AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole \
                       AND NOT rolinherit AND NOT rolreplication AND NOT rolbypassrls; \
                    IF role_oid IS NULL \
                       OR pg_catalog.has_database_privilege(role_oid, current_database(), 'CONNECT') \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_class WHERE relowner = role_oid) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspowner = role_oid) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_proc WHERE proowner = role_oid) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datdba = role_oid) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members WHERE member = role_oid) \
                       OR EXISTS ( \
                            SELECT 1 FROM pg_catalog.pg_auth_members AS membership \
                            JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member \
                            WHERE membership.roleid = role_oid \
                              AND (member.rolname !~ '^wamn_effect_writer_[0-9a-f]{{40}}_[ab]$' \
                                   OR NOT member.rolcanlogin OR member.rolsuper \
                                   OR member.rolcreatedb OR member.rolcreaterole \
                                   OR NOT member.rolinherit OR member.rolreplication \
                                   OR member.rolbypassrls)) \
                       OR {generation_contract} \
                    THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                         MESSAGE = 'effect-writer-role-out-of-bounds'; \
                    END IF; \
                  END $effect_writer_role$",
                generation_contract = generation_role_contract_violation_sql(),
            ),
        });
    }
    let frame_cutover_targets = frame_identity_cutover_targets(obs, schema);
    if frame_cutover_targets.needed() {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::FrameIdentityCutover,
            target: "effect_attempts.frame-identity".to_string(),
            sql: frame_identity_cutover_sql(schema, frame_cutover_targets),
        });
    }
    if effect_writer_ledger_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EffectWriterCutover,
            target: "effect-ledgers.coordinate-writer-boundary".to_string(),
            sql: effect_writer_cutover_sql(schema, obs),
        });
    }
    // This is the final pre-role-bootstrap migration. Keeping it behind the
    // older guarded cutovers preserves their refusal ordering on compound
    // legacy schemas, while `RESTRICT` dependency failures still happen before
    // the shell creates/hardens roles or repairs membership.
    if failure_detail_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::FailureDetailCutover,
            target: "runs.failure-detail".to_string(),
            sql: failure_detail_cutover_sql(schema),
        });
    }
    let retire_invocation_admissions = obs.tables.contains_key("invocation_admissions");
    let retire_catalog_head_lock = obs.helper_functions.contains_key("lock_catalog_head");
    if retire_invocation_admissions || retire_catalog_head_lock {
        let mut statements = Vec::new();
        if retire_invocation_admissions {
            statements.push(format!(
                "DROP TABLE {}.invocation_admissions RESTRICT",
                schema.quoted()
            ));
        }
        if retire_catalog_head_lock {
            statements.push(format!(
                "DROP FUNCTION {}.lock_catalog_head(text, text, text) RESTRICT",
                schema.quoted()
            ));
        }
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RetireLegacyAdmissionSurface,
            target: "legacy-admission-surface".to_string(),
            sql: statements.join("; "),
        });
    }

    // The standalone record files grant schema visibility to this reserved
    // role, so it must exist (and remain non-login/non-bypass) before a missing
    // schema header executes. It intentionally owns no project-plane table read.
    if obs
        .scenario_author_role
        .is_none_or(|role| !role.is_host_only())
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EnsureScenarioAuthorRole,
            target: SCENARIO_AUTHOR_ROLE.to_string(),
            sql: ensure_scenario_author_role_sql().to_string(),
        });
    }
    if obs.app_is_scenario_author_member {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: "wamn_app-membership".to_string(),
            sql: format!("REVOKE {SCENARIO_AUTHOR_ROLE} FROM wamn_app"),
        });
    }
    if capture_mode_present && run_capture_privileges_drifted {
        let available_columns = obs
            .tables
            .get("runs")
            .expect("capture_mode is present only on a present runs table")
            .iter()
            .filter(|column| {
                (!child_run_cutover_needed || !RETIRED_CHILD_RUN_COLUMNS.contains(&column.as_str()))
                    && (!rerun_lineage_cutover_needed
                        || !RETIRED_RERUN_LINEAGE_COLUMNS.contains(&column.as_str()))
                    && (!failure_detail_cutover_needed
                        || !RETIRED_FAILURE_DETAIL_COLUMNS.contains(&column.as_str()))
            })
            .cloned();
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairRunCapturePrivilege,
            target: "runs.capture_mode".to_string(),
            sql: repair_run_capture_privilege_sql(schema, available_columns),
        });
    }

    // 1. Missing run-plane tables → EnsureSchema once, then per-table sections
    //    in file order so retained foreign keys resolve.
    let mut any_missing = false;
    let mut creates = Vec::new();
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            if obs.tables.contains_key(&table) {
                continue;
            }
            any_missing = true;
            creates.push(RunPlaneAction {
                kind: RunPlaneActionKind::CreateTable,
                target: table.clone(),
                sql: rewrite_schema(&table_section(file, "wamn_run", &table), schema),
            });
        }
    }
    if any_missing {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EnsureSchema,
            target: schema.to_string(),
            sql: rewrite_schema(&schema_header_section(RUN_STATE_SQL, "wamn_run"), schema),
        });
    }
    // Helpers precede table sections: missing-table sections carry triggers,
    // and those triggers must resolve their functions at CREATE time.
    let helper_specs = helper_specs();
    for spec in &helper_specs {
        if obs
            .helper_functions
            .get(spec.name)
            .is_none_or(|definition| {
                normalize_observed_schema(definition, schema) != spec.definition.as_ref()
            })
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairHelperFunction,
                target: spec.name.to_string(),
                sql: rewrite_schema(&spec.sql, schema),
            });
        }
    }
    // Catalog storage converges before run-plane constraint reconciliation:
    // attested rows derive their portable connection name from the pinned flow
    // graph, and the next runtime child reads the nullable provenance columns.
    if !obs.catalog_schema_present {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EnsureCatalogSchema,
            target: "catalog".to_string(),
            sql: CATALOG_SCHEMA_SQL.to_string(),
        });
    } else {
        for table in record_tables(CATALOG_SCHEMA_SQL, "catalog") {
            if !obs.catalog_tables.contains(&table) {
                plan.actions.push(RunPlaneAction {
                    kind: RunPlaneActionKind::CreateCatalogTable,
                    target: table.clone(),
                    sql: table_section(CATALOG_SCHEMA_SQL, "catalog", &table),
                });
            }
        }
    }

    // `runs` now has immediate catalog FKs, so catalog creation must precede
    // every missing run-table section.
    plan.actions.extend(creates);

    if obs.app_run_queue_authority {
        let columns = obs
            .tables
            .get("run_queue")
            .into_iter()
            .flatten()
            .filter(|column| {
                !partition_plane_cutover_needed
                    || !RETIRED_PARTITION_COLUMNS.contains(&column.as_str())
            })
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        let column_revoke = if columns.is_empty() {
            String::new()
        } else {
            format!(
                "; REVOKE SELECT ({columns}), INSERT ({columns}), UPDATE ({columns}), \
                 REFERENCES ({columns}) ON TABLE {}.run_queue FROM PUBLIC, wamn_app",
                schema.quoted()
            )
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RemoveAppRunQueueAuthority,
            target: format!("{}.run_queue.app-authority", schema.as_str()),
            sql: format!(
                "REVOKE ALL PRIVILEGES ON TABLE {}.run_queue FROM PUBLIC, wamn_app{column_revoke}",
                schema.quoted()
            ),
        });
    }

    if obs.effect_writer_schema_privileges != (true, false) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairEffectWriterPrivilege,
            target: format!("{}.usage", schema.as_str()),
            sql: format!(
                "REVOKE ALL PRIVILEGES ON SCHEMA {} FROM PUBLIC, {EFFECT_WRITER_ROLE}; \
                 GRANT USAGE ON SCHEMA {} TO {EFFECT_WRITER_ROLE}; \
                 DO $effect_writer_schema_acl$ BEGIN \
                   IF NOT pg_catalog.has_schema_privilege('{EFFECT_WRITER_ROLE}', '{}', 'USAGE') \
                      OR pg_catalog.has_schema_privilege('{EFFECT_WRITER_ROLE}', '{}', 'CREATE') \
                   THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                        MESSAGE = 'effect-writer-schema-privilege-out-of-bounds'; \
                   END IF; \
                 END $effect_writer_schema_acl$",
                schema.quoted(),
                schema.quoted(),
                schema.as_str(),
                schema.as_str(),
            ),
        });
    }
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        if !obs.tables.contains_key(table) {
            continue;
        }
        // BORN PARKED (owner ruling on wamn-0h0g.20.28, widened to the two sibling
        // ledgers by wamn-0h0g.20.32). NO effect ledger's APPEND authority is part
        // of the record: the writer primitive is unwired, and every generation
        // login inherits this role with INHERIT TRUE. So a live INSERT on any of
        // the three is DRIFT, and this convergent step REMOVES it rather than
        // re-granting it. Whoever wires the writer grants those INSERTs here.
        let writer_privileges: &[&str] = &["SELECT"];
        let expected = |grantee: &str| -> BTreeSet<String> {
            match grantee {
                "wamn_app" => ["SELECT"].into_iter().map(str::to_string).collect(),
                EFFECT_WRITER_ROLE => writer_privileges
                    .iter()
                    .copied()
                    .map(str::to_string)
                    .collect(),
                "PUBLIC" | SCENARIO_AUTHOR_ROLE => BTreeSet::new(),
                _ => unreachable!("closed effect-ledger grantee set"),
            }
        };
        let direct_drifted = [
            "PUBLIC",
            "wamn_app",
            SCENARIO_AUTHOR_ROLE,
            EFFECT_WRITER_ROLE,
        ]
        .into_iter()
        .any(|grantee| {
            obs.effect_ledger_table_privileges
                .get(&(table.to_string(), grantee.to_string()))
                .cloned()
                .unwrap_or_default()
                != expected(grantee)
        });
        let effective_drifted = ["wamn_app", SCENARIO_AUTHOR_ROLE, EFFECT_WRITER_ROLE]
            .into_iter()
            .any(|grantee| {
                obs.effect_ledger_effective_privileges
                    .get(&(table.to_string(), grantee.to_string()))
                    .cloned()
                    .unwrap_or_default()
                    != expected(grantee)
            });
        let effective_column_drifted = ["wamn_app", SCENARIO_AUTHOR_ROLE, EFFECT_WRITER_ROLE]
            .into_iter()
            .any(|grantee| {
                let expected_columns: BTreeSet<String> = expected(grantee)
                    .into_iter()
                    .filter(|privilege| {
                        ["SELECT", "INSERT", "UPDATE", "REFERENCES"].contains(&privilege.as_str())
                    })
                    .collect();
                obs.effect_ledger_effective_column_privileges
                    .get(&(table.to_string(), grantee.to_string()))
                    .cloned()
                    .unwrap_or_default()
                    != expected_columns
            });
        let boundary_owned = obs.effect_ledger_owners.get(table).is_some_and(|owner| {
            matches!(
                owner.as_str(),
                "wamn_app" | SCENARIO_AUTHOR_ROLE | EFFECT_WRITER_ROLE
            )
        });
        if !direct_drifted && !effective_drifted && !effective_column_drifted && !boundary_owned {
            continue;
        }
        let qualified = format!("{}.{}", schema.quoted(), quote_ident(table));
        let columns = obs
            .tables
            .get(table)
            .expect("present effect ledger")
            .iter()
            .filter(|column| {
                if table != "effect_attempts" {
                    return true;
                }
                let frame_owned = frame_cutover_targets.effect
                    && (column.as_str() == "node_id"
                        || EFFECT_FRAME_COLUMNS.contains(&column.as_str()));
                let writer_owned = effect_writer_ledger_cutover_needed
                    && RETIRED_EFFECT_ATTEMPT_COLUMNS.contains(&column.as_str());
                !frame_owned && !writer_owned
            })
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        // The grant and the self-check move together: whatever APPEND authority
        // this ledger does not carry becomes a privilege the block REFUSES to see
        // the server still report, so a parked table shows its own denial.
        let writer_grant = writer_privileges.join(", ");
        let writer_forbidden_table = "'INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'";
        let writer_forbidden_columns = "INSERT,UPDATE,REFERENCES";
        plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairEffectWriterPrivilege,
                target: format!("{}.{}", schema.as_str(), table),
                sql: format!(
                    "REVOKE SELECT ({columns}), INSERT ({columns}), UPDATE ({columns}), \
                            REFERENCES ({columns}) ON TABLE {qualified} \
                       FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}, {EFFECT_WRITER_ROLE}; \
                     REVOKE ALL PRIVILEGES ON TABLE {qualified} \
                       FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}, {EFFECT_WRITER_ROLE}; \
                     GRANT SELECT ON TABLE {qualified} TO wamn_app; \
                     GRANT {writer_grant} ON TABLE {qualified} TO {EFFECT_WRITER_ROLE}; \
                     DO $effect_ledger_acl$ BEGIN \
                       IF EXISTS (SELECT 1 FROM unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('wamn_app', '{qualified}', privilege)) \
                          OR EXISTS (SELECT 1 FROM unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('{SCENARIO_AUTHOR_ROLE}', '{qualified}', privilege)) \
                          OR EXISTS (SELECT 1 FROM unnest(ARRAY[{writer_forbidden_table}]) privilege \
                                   WHERE pg_catalog.has_table_privilege('{EFFECT_WRITER_ROLE}', '{qualified}', privilege)) \
                          OR pg_catalog.has_any_column_privilege('wamn_app', '{qualified}', 'INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{SCENARIO_AUTHOR_ROLE}', '{qualified}', 'SELECT,INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{EFFECT_WRITER_ROLE}', '{qualified}', '{writer_forbidden_columns}') \
                          OR (SELECT owner.rolname FROM pg_catalog.pg_class relation \
                              JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner \
                             WHERE relation.oid = pg_catalog.to_regclass('{qualified}')) \
                             IN ('wamn_app', '{SCENARIO_AUTHOR_ROLE}', '{EFFECT_WRITER_ROLE}') \
                       THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                            MESSAGE = 'effect-ledger-effective-privilege-out-of-bounds:{table}'; \
                       END IF; \
                     END $effect_ledger_acl$"
                ),
            });
    }

    let mut effect_writer_run_read_repairs = Vec::new();
    for (table, allowed) in EFFECT_WRITER_RUN_READ_COLUMNS {
        let Some(live_columns) = obs.tables.get(table) else {
            continue;
        };
        let table_drifted = obs
            .effect_writer_run_table_privileges
            .get(table)
            .is_some_and(|privileges| !privileges.is_empty());
        let column_drifted = allowed.iter().any(|column| !live_columns.contains(*column))
            || live_columns.iter().any(|column| {
                let actual = obs
                    .effect_writer_run_column_privileges
                    .get(&(table.to_string(), column.clone()))
                    .cloned()
                    .unwrap_or_default();
                let expected: BTreeSet<String> = if allowed.contains(&column.as_str()) {
                    ["SELECT".to_string()].into_iter().collect()
                } else {
                    BTreeSet::new()
                };
                actual != expected
            });
        if !table_drifted && !column_drifted {
            continue;
        }

        let qualified = format!("{}.{}", schema.quoted(), quote_ident(table));
        let all_columns = live_columns
            .iter()
            .filter(|column| {
                !(table == "run_queue"
                    && partition_plane_cutover_needed
                    && RETIRED_PARTITION_COLUMNS.contains(&column.as_str()))
                    && !(table == "runs"
                        && rerun_lineage_cutover_needed
                        && RETIRED_RERUN_LINEAGE_COLUMNS.contains(&column.as_str()))
                    && !(table == "runs"
                        && failure_detail_cutover_needed
                        && RETIRED_FAILURE_DETAIL_COLUMNS.contains(&column.as_str()))
            })
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        let allowed_columns = allowed
            .iter()
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        let allowed_literals = allowed
            .iter()
            .map(|column| format!("'{}'", column.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        effect_writer_run_read_repairs.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairEffectWriterPrivilege,
            target: format!("{}.{}.effect-read", schema.as_str(), table),
            sql: format!(
                "REVOKE SELECT ({all_columns}), INSERT ({all_columns}), \
                        UPDATE ({all_columns}), REFERENCES ({all_columns}) \
                   ON TABLE {qualified} FROM PUBLIC, {EFFECT_WRITER_ROLE}; \
                 REVOKE ALL PRIVILEGES ON TABLE {qualified} \
                   FROM PUBLIC, {EFFECT_WRITER_ROLE}; \
                 GRANT SELECT ({allowed_columns}) ON TABLE {qualified} \
                   TO {EFFECT_WRITER_ROLE}; \
                 DO $effect_writer_run_read_acl$ BEGIN \
                   IF EXISTS ( \
                        SELECT 1 FROM unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE', \
                                                   'TRUNCATE','REFERENCES','TRIGGER']) privilege \
                         WHERE pg_catalog.has_table_privilege( \
                               '{EFFECT_WRITER_ROLE}', '{qualified}', privilege)) \
                      OR EXISTS ( \
                        SELECT 1 FROM pg_catalog.pg_attribute AS attribute \
                        CROSS JOIN unnest(ARRAY['INSERT','UPDATE','REFERENCES']) privilege \
                         WHERE attribute.attrelid=pg_catalog.to_regclass('{qualified}') \
                           AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                           AND pg_catalog.has_column_privilege( \
                               '{EFFECT_WRITER_ROLE}', '{qualified}', \
                               attribute.attname, privilege)) \
                      OR EXISTS ( \
                        SELECT 1 FROM pg_catalog.pg_attribute AS attribute \
                         WHERE attribute.attrelid=pg_catalog.to_regclass('{qualified}') \
                           AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                           AND NOT (attribute.attname = ANY (ARRAY[{allowed_literals}])) \
                           AND pg_catalog.has_column_privilege( \
                               '{EFFECT_WRITER_ROLE}', '{qualified}', \
                               attribute.attname, 'SELECT')) \
                      OR EXISTS ( \
                        SELECT 1 FROM unnest(ARRAY[{allowed_literals}]) column_name \
                         WHERE NOT pg_catalog.has_column_privilege( \
                               '{EFFECT_WRITER_ROLE}', '{qualified}', \
                               column_name, 'SELECT')) \
                   THEN RAISE EXCEPTION USING ERRCODE='42501', \
                        MESSAGE='effect-writer-run-read-privilege-out-of-bounds:{table}'; \
                   END IF; \
                 END $effect_writer_run_read_acl$"
            ),
        });
    }

    if wiring_identity_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::AddColumn,
            target: "runs.wiring-identity".to_string(),
            sql: wiring_identity_cutover_sql(schema),
        });
    }

    // The host-only role needs schema visibility even when an existing schema
    // receives only a newly missing table section (record table sections do
    // not replay file headers). Catalog from-zero carries its header grant;
    // an existing catalog is repaired explicitly.
    if !obs.scenario_author_schema_usage.contains(schema.as_str()) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: format!("{}.usage", schema.as_str()),
            sql: format!(
                "GRANT USAGE ON SCHEMA {} TO {SCENARIO_AUTHOR_ROLE}",
                schema.quoted()
            ),
        });
    }
    if obs.catalog_schema_present && !obs.scenario_author_schema_usage.contains("catalog") {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: "catalog.usage".to_string(),
            sql: format!("GRANT USAGE ON SCHEMA catalog TO {SCENARIO_AUTHOR_ROLE}"),
        });
    }

    // Converge direct grants exactly on the narrow authoring surface. PUBLIC
    // and the guest-visible role are part of the expected map so a stale grant
    // cannot survive an otherwise current schema.
    for spec in AUTHORING_PRIVILEGE_SPECS {
        if matches!(spec.schema, AuthoringTableSchema::RunPlane) && spec.table == "runs" {
            continue;
        }
        let (schema_name, present) = match spec.schema {
            AuthoringTableSchema::Catalog => ("catalog", obs.catalog_tables.contains(spec.table)),
            AuthoringTableSchema::RunPlane => {
                (schema.as_str(), obs.tables.contains_key(spec.table))
            }
        };
        if !present {
            continue;
        }
        let is_environment_policy = matches!(spec.schema, AuthoringTableSchema::RunPlane)
            && spec.table == "environment_policies";
        let direct_grantees: &[&str] = if is_environment_policy {
            &[
                "PUBLIC",
                "wamn_app",
                SCENARIO_AUTHOR_ROLE,
                EFFECT_WRITER_ROLE,
            ]
        } else {
            &["PUBLIC", "wamn_app", SCENARIO_AUTHOR_ROLE]
        };
        let effective_grantees: &[&str] = if is_environment_policy {
            &["wamn_app", SCENARIO_AUTHOR_ROLE, EFFECT_WRITER_ROLE]
        } else {
            &["wamn_app", SCENARIO_AUTHOR_ROLE]
        };
        let expected_for = |grantee: &str| -> BTreeSet<String> {
            let privileges = match grantee {
                "wamn_app" => spec.app,
                SCENARIO_AUTHOR_ROLE => spec.author,
                "PUBLIC" | EFFECT_WRITER_ROLE => &[],
                _ => unreachable!("closed authoring grantee set"),
            };
            privileges
                .iter()
                .map(|value| (*value).to_string())
                .collect()
        };
        let direct_drifted = direct_grantees.iter().copied().any(|grantee| {
            obs.authoring_table_privileges
                .get(&(
                    schema_name.to_string(),
                    spec.table.to_string(),
                    grantee.to_string(),
                ))
                .cloned()
                .unwrap_or_default()
                != expected_for(grantee)
        });
        let effective_drifted = effective_grantees.iter().copied().any(|grantee| {
            obs.authoring_effective_table_privileges
                .get(&(
                    schema_name.to_string(),
                    spec.table.to_string(),
                    grantee.to_string(),
                ))
                .cloned()
                .unwrap_or_default()
                != expected_for(grantee)
        });
        let effective_column_drifted = effective_grantees.iter().copied().any(|grantee| {
            let expected_columns: BTreeSet<String> = expected_for(grantee)
                .into_iter()
                .filter(|privilege| {
                    ["SELECT", "INSERT", "UPDATE", "REFERENCES"].contains(&privilege.as_str())
                })
                .collect();
            obs.authoring_effective_column_privileges
                .get(&(
                    schema_name.to_string(),
                    spec.table.to_string(),
                    grantee.to_string(),
                ))
                .cloned()
                .unwrap_or_default()
                != expected_columns
        });
        let boundary_owned = obs
            .authoring_table_owners
            .get(&(schema_name.to_string(), spec.table.to_string()))
            .is_some_and(|owner| effective_grantees.contains(&owner.as_str()));
        if !direct_drifted && !effective_drifted && !effective_column_drifted && !boundary_owned {
            continue;
        }

        let qualified = format!("{}.{}", quote_ident(schema_name), quote_ident(spec.table));
        let mut sql = String::new();
        if direct_drifted {
            sql = direct_grantees
                .iter()
                .map(|grantee| format!("REVOKE ALL PRIVILEGES ON TABLE {qualified} FROM {grantee}"))
                .collect::<Vec<_>>()
                .join("; ");
            for (grantee, privileges) in
                [("wamn_app", spec.app), (SCENARIO_AUTHOR_ROLE, spec.author)]
            {
                if !privileges.is_empty() {
                    sql.push_str(&format!(
                        "; GRANT {} ON TABLE {qualified} TO {grantee}",
                        privileges.join(", ")
                    ));
                }
            }
        }
        // Direct REVOKEs cannot safely repair an unrelated inherited group or
        // table ownership. Verify the effective postcondition and fail loudly
        // instead of claiming convergence while either boundary role retains
        // authority outside its spec.
        let mut forbidden_checks = Vec::new();
        for grantee in effective_grantees.iter().copied() {
            let expected = match grantee {
                "wamn_app" => spec.app,
                SCENARIO_AUTHOR_ROLE => spec.author,
                EFFECT_WRITER_ROLE => &[],
                _ => unreachable!("closed effective grantee set"),
            };
            for privilege in TABLE_PRIVILEGE_TYPES {
                if !expected.contains(&privilege) {
                    forbidden_checks.push(format!(
                        "pg_catalog.has_table_privilege(\
                         '{grantee}', '{qualified}', '{privilege}')"
                    ));
                }
            }
            for privilege in ["SELECT", "INSERT", "UPDATE", "REFERENCES"] {
                if !expected.contains(&privilege) {
                    forbidden_checks.push(format!(
                        "pg_catalog.has_any_column_privilege(\
                         '{grantee}', '{qualified}', '{privilege}')"
                    ));
                }
            }
        }
        forbidden_checks.push(format!(
            "(SELECT owner.rolname \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner \
              WHERE relation.oid = pg_catalog.to_regclass('{qualified}')) \
             IN ({})",
            effective_grantees
                .iter()
                .map(|grantee| format!("'{grantee}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if !sql.is_empty() {
            sql.push_str("; ");
        }
        sql.push_str(&format!(
            "DO $effective_acl$ BEGIN IF {} THEN RAISE EXCEPTION USING \
             ERRCODE = '42501', MESSAGE = \
             'authoring-effective-privilege-out-of-bounds:{schema_name}.{}'; \
             END IF; END $effective_acl$",
            forbidden_checks.join(" OR "),
            spec.table,
        ));
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: format!("{schema_name}.{}", spec.table),
            sql,
        });
    }

    // 2. Column drift on PRESENT record tables: add what the record has and the
    //    live table lacks (record order); surface unknown extras, never drop
    //    them. Explicit cutover-owned retired columns are handled above.
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            let Some(live_cols) = obs.tables.get(&table) else {
                continue;
            };
            let record_cols = record_columns(file, "wamn_run", &table);
            for (record_column_index, (col, def)) in record_cols.iter().enumerate() {
                if wiring_identity_cutover_needed
                    && table == "runs"
                    && matches!(
                        col.as_str(),
                        "flow_id"
                            | "flow_version"
                            | "wiring_id"
                            | "wiring_version"
                            | "wiring_hash"
                            | "binding_world_json"
                    )
                {
                    continue;
                }
                if frame_cutover_targets.needed() && frame_identity_column(&table, col) {
                    continue;
                }
                if effect_writer_ledger_cutover_needed
                    && table == "effect_attempt_dispatches"
                    && matches!(
                        col.as_str(),
                        "run_id" | "frame_id" | "local_node_id" | "occurrence"
                    )
                {
                    continue;
                }
                if !live_cols.contains(col) {
                    let add_column_sql = format!(
                        "ALTER TABLE {}.{} ADD COLUMN {def}",
                        schema.quoted(),
                        quote_ident(&table),
                    );
                    let sql = if table == "runs"
                        && col == "capture_mode"
                        && obs.app_run_capture_privileges.0
                    {
                        let available_columns = live_cols
                            .iter()
                            .cloned()
                            .chain(
                                record_cols[..=record_column_index]
                                    .iter()
                                    .map(|(column, _)| column.clone()),
                            )
                            .filter(|column| {
                                (!child_run_cutover_needed
                                    || !RETIRED_CHILD_RUN_COLUMNS.contains(&column.as_str()))
                                    && (!failure_detail_cutover_needed
                                        || !RETIRED_FAILURE_DETAIL_COLUMNS
                                            .contains(&column.as_str()))
                            });
                        format!(
                            "LOCK TABLE {}.runs IN ACCESS EXCLUSIVE MODE; {add_column_sql}; {}",
                            schema.quoted(),
                            repair_run_capture_privilege_sql(schema, available_columns),
                        )
                    } else {
                        add_column_sql
                    };
                    plan.actions.push(RunPlaneAction {
                        kind: RunPlaneActionKind::AddColumn,
                        target: format!("{table}.{col}"),
                        sql,
                    });
                }
            }
            let known: BTreeSet<&str> = record_cols.iter().map(|(c, _)| c.as_str()).collect();
            for col in live_cols {
                if partition_plane_cutover_needed
                    && table == "run_queue"
                    && RETIRED_PARTITION_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if frame_cutover_targets.includes_table(&table)
                    && frame_identity_column(&table, col)
                {
                    continue;
                }
                if effect_writer_ledger_cutover_needed
                    && table == "effect_attempts"
                    && RETIRED_EFFECT_ATTEMPT_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if child_run_cutover_needed
                    && table == "runs"
                    && RETIRED_CHILD_RUN_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if rerun_lineage_cutover_needed
                    && table == "runs"
                    && RETIRED_RERUN_LINEAGE_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if failure_detail_cutover_needed
                    && table == "runs"
                    && RETIRED_FAILURE_DETAIL_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if stored_suite_cutover_needed
                    && RETIRED_TEST_SET_REFERENCE_TABLES.contains(&table.as_str())
                    && col == RETIRED_TEST_SET_REFERENCE_COLUMN
                {
                    continue;
                }
                if !known.contains(col.as_str()) {
                    plan.extra_columns.push((table.clone(), col.clone()));
                }
            }
        }
    }

    // A missing table's record section carries its complete RLS apparatus.
    // Existing tables are compared at the PostgreSQL catalog grain: both
    // relation flags and the sole tenant SELECT policy must match exactly.
    if obs.tables.contains_key("environment_policies")
        && obs.environment_policy_row_security.as_ref()
            != Some(&environment_policy_row_security_at_record())
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairRowSecurity,
            target: "environment_policies.row-security".to_string(),
            sql: repair_environment_policy_row_security_sql(schema),
        });
    }

    // Required columns are added before the exact writer read boundary names
    // them. This also lets one reconcile turn converge a partial queue shape.
    plan.actions.extend(effect_writer_run_read_repairs);

    // A broad legacy grant is narrowed after the record column exists.
    if !capture_mode_present && run_capture_privileges_drifted && !obs.app_run_capture_privileges.0
    {
        let record_columns = record_columns(RUN_STATE_SQL, "wamn_run", "runs")
            .into_iter()
            .map(|(column, _)| column);
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairRunCapturePrivilege,
            target: "runs.capture_mode".to_string(),
            sql: repair_run_capture_privilege_sql(schema, record_columns),
        });
    }

    // 2b. Exact CHECK convergence. AddColumn carries its own inline CHECK, so
    // skip that spec when its column is absent in the observation. Table-level
    // checks run after AddColumn and therefore may safely name newly-added
    // columns. PostgreSQL validates every ADD against existing rows; a legacy
    // row that violates the canonical contract aborts reconciliation rather
    // than being rewritten or deleted.
    let expected_checks: BTreeSet<(&str, &str)> = CHECK_SPECS
        .iter()
        .map(|spec| (spec.table, spec.name))
        .collect();
    for spec in CHECK_SPECS {
        if spec.table == "runs" && spec.name == "runs_check" {
            continue;
        }
        if wiring_identity_cutover_needed
            && spec.table == "runs"
            && matches!(
                spec.name,
                "runs_wiring_identity_check" | "runs_execution_grain_check"
            )
        {
            continue;
        }
        if frame_cutover_targets.includes_table(spec.table)
            && frame_identity_check(spec.table, spec.name)
        {
            continue;
        }
        if effect_writer_ledger_cutover_needed
            && spec.table == "effect_attempt_dispatches"
            && matches!(
                spec.name,
                "effect_attempt_dispatches_frame_check"
                    | "effect_attempt_dispatches_local_node_check"
                    | "effect_attempt_dispatches_occurrence_check"
            )
        {
            continue;
        }
        let Some(columns) = obs.tables.get(spec.table) else {
            continue;
        };
        if matches!(spec.origin, CheckOrigin::Inline(column) if !columns.contains(column)) {
            continue;
        }
        let key = (spec.table.to_string(), spec.name.to_string());
        if obs
            .checks
            .get(&key)
            .is_some_and(|def| def == spec.definition)
        {
            continue;
        }
        let drop = if obs.checks.contains_key(&key) {
            format!("DROP CONSTRAINT {}, ", quote_ident(spec.name))
        } else {
            String::new()
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairConstraint,
            target: format!("{}.{}", spec.table, spec.name),
            sql: format!(
                "ALTER TABLE {}.{} {drop}ADD CONSTRAINT {} {}",
                schema.quoted(),
                quote_ident(spec.table),
                quote_ident(spec.name),
                spec.definition,
            ),
        });
    }
    for ((table, name), definition) in &obs.checks {
        if obs.tables.contains_key(table)
            && record_table_names().contains(table.as_str())
            && !expected_checks.contains(&(table.as_str(), name.as_str()))
            && !(effect_writer_ledger_cutover_needed
                && effect_writer_cutover_owned_check(table, name))
            && !(frame_cutover_targets.includes_table(table) && frame_identity_check(table, name))
            && !(partition_plane_cutover_needed
                && table == "run_queue"
                && name == RETIRED_PARTITION_CHECK)
            && !(child_run_cutover_needed && table == "runs" && retired_child_run_check(definition))
            && !(table == "runs" && name == "runs_environment_check")
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::DropExtraConstraint,
                target: format!("{table}.{name}"),
                sql: format!(
                    "ALTER TABLE {}.{} DROP CONSTRAINT {}",
                    schema.quoted(),
                    quote_ident(table),
                    quote_ident(name),
                ),
            });
        }
    }

    // 2d. The effect-ledger FKs remain exact. A missing table's canonical
    // CREATE section carries these, so repair only observed tables.
    for (table, name, definition, sql) in [
        (
            "effect_attempt_dispatches",
            EFFECT_DISPATCH_ATTEMPT_FK_NAME,
            EFFECT_DISPATCH_ATTEMPT_FK_DEF,
            EFFECT_DISPATCH_ATTEMPT_FK_SQL,
        ),
        (
            "effect_attempt_outcomes",
            EFFECT_OUTCOME_DISPATCH_FK_NAME,
            EFFECT_OUTCOME_DISPATCH_FK_DEF,
            EFFECT_OUTCOME_DISPATCH_FK_SQL,
        ),
    ] {
        if effect_writer_ledger_cutover_needed
            && table == "effect_attempt_dispatches"
            && obs.tables.contains_key("effect_attempts")
        {
            continue;
        }
        if !obs.tables.contains_key(table) {
            continue;
        }
        let key = (table.to_string(), name.to_string());
        if obs
            .foreign_keys
            .get(&key)
            .is_some_and(|observed| normalize_observed_schema(observed, schema) == definition)
        {
            continue;
        }
        let drop = if obs.foreign_keys.contains_key(&key) {
            format!(
                "ALTER TABLE {}.{} DROP CONSTRAINT {}; ",
                schema.quoted(),
                quote_ident(table),
                quote_ident(name),
            )
        } else {
            String::new()
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairForeignKey,
            target: format!("{table}.{name}"),
            sql: format!("{drop}{}", rewrite_schema(sql, schema)),
        });
    }

    // 2e. User triggers are explicit record objects. A missing table's section
    // may carry its trigger; triggers placed after a shared helper are separate
    // actions because the section parser deliberately stops at that helper.
    // Present tables are repaired exactly, and immutable-ledger triggers are
    // never mistaken for extras.
    let trigger_specs = trigger_specs();
    let expected_triggers: BTreeSet<(&str, &str)> = trigger_specs
        .iter()
        .map(|spec| (spec.table.as_str(), spec.name.as_str()))
        .collect();
    for spec in &trigger_specs {
        if !obs.tables.contains_key(&spec.table)
            && table_section_carries_trigger(&spec.table, &spec.name)
        {
            continue;
        }
        let key = (spec.table.clone(), spec.name.clone());
        if obs.triggers.get(&key).is_some_and(|definition| {
            normalize_observed_schema(definition, schema) == spec.definition
        }) {
            continue;
        }
        let drop = if obs.triggers.contains_key(&key) {
            format!(
                "DROP TRIGGER {} ON {}.{}; ",
                quote_ident(&spec.name),
                schema.quoted(),
                quote_ident(&spec.table),
            )
        } else {
            String::new()
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairTrigger,
            target: format!("{}.{}", spec.table, spec.name),
            sql: format!("{drop}{}", rewrite_schema(&spec.sql, schema)),
        });
    }
    for (table, name) in obs.triggers.keys() {
        if record_table_names().contains(table.as_str())
            && !expected_triggers.contains(&(table.as_str(), name.as_str()))
            && name != OUTBOX_TRIGGER_NAME
            && !(effect_writer_ledger_cutover_needed
                && matches!(
                    (table.as_str(), name.as_str()),
                    ("effect_attempts", "effect_attempts_insert_guard")
                        | (
                            "effect_attempt_dispatches",
                            "effect_attempt_dispatches_insert_guard"
                        )
                        | (
                            "effect_attempt_outcomes",
                            "effect_attempt_outcomes_insert_guard"
                        )
                ))
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::DropExtraTrigger,
                target: format!("{table}.{name}"),
                sql: format!(
                    "DROP TRIGGER {} ON {}.{}",
                    quote_ident(name),
                    schema.quoted(),
                    quote_ident(table),
                ),
            });
        }
    }

    // 3. Index drift on PRESENT tables only (a created section carries its own
    //    indexes): absent → create from record; present but the live definition
    //    lost a record column the record definition names → drop + recreate.
    for file in RUN_PLANE_FILES {
        for (name, table, stmt) in index_statements(file, "wamn_run") {
            if !obs.tables.contains_key(&table) {
                continue;
            }
            if matches!(name.as_str(), "runs_release" | "runs_execution_bundle") {
                continue;
            }
            if effect_writer_ledger_cutover_needed
                && matches!(
                    name.as_str(),
                    "effect_attempts_dispatch_identity_key"
                        | "effect_attempt_dispatches_occurrence_key"
                )
            {
                continue;
            }
            if partition_plane_cutover_needed
                && run_queue_claim_index_ready(obs)
                && name == "run_queue_claimable"
            {
                continue;
            }
            match obs.indexes.get(&name) {
                None => plan.actions.push(RunPlaneAction {
                    kind: RunPlaneActionKind::CreateIndex,
                    target: name.clone(),
                    sql: rewrite_schema(&stmt, schema),
                }),
                Some(live_def) if index_definition_stale(file, &table, &stmt, live_def) => {
                    plan.actions.push(RunPlaneAction {
                        kind: RunPlaneActionKind::RecreateIndex,
                        target: name.clone(),
                        sql: format!(
                            "DROP INDEX {}.{}; {}",
                            schema.quoted(),
                            quote_ident(&name),
                            rewrite_schema(&stmt, schema),
                        ),
                    });
                }
                Some(_) => {}
            }
        }
    }

    // 4. Legacy outbox-era teardown: tables, then triggers BEFORE the function
    //    (DROP FUNCTION is RESTRICT while a trigger still references it).
    for legacy in LEGACY_OUTBOX_TABLES {
        if obs.tables.contains_key(legacy) {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::DropLegacyTable,
                target: legacy.to_string(),
                sql: format!(
                    "DROP TABLE IF EXISTS {}.{}",
                    schema.quoted(),
                    quote_ident(legacy),
                ),
            });
        }
    }
    for table in &obs.outbox_trigger_tables {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::DropLegacyTrigger,
            target: table.clone(),
            sql: format!(
                "DROP TRIGGER IF EXISTS {OUTBOX_TRIGGER_NAME} ON {}.{}",
                schema.quoted(),
                quote_ident(table),
            ),
        });
    }
    if obs.outbox_function_present {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::DropLegacyFunction,
            target: OUTBOX_TRIGGER_NAME.to_string(),
            sql: format!(
                "DROP FUNCTION IF EXISTS {}.{OUTBOX_TRIGGER_NAME}()",
                schema.quoted(),
            ),
        });
    }

    // 5. Registration payload cleanup follows structural convergence.
    if obs.stale_registration_key_rows > 0 {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::StripRetiredRegistrationKeys,
            target: format!("{} registrations", obs.stale_registration_key_rows),
            sql: strip_retired_registration_keys_sql().to_string(),
        });
    }

    // Report the run-plane tables that needed nothing at all.
    let touched: BTreeSet<&str> = plan
        .actions
        .iter()
        .map(|a| a.target.split('.').next().unwrap_or(&a.target))
        .collect();
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            let index_touched = index_statements(file, "wamn_run")
                .iter()
                .any(|(name, t, _)| *t == table && touched.contains(name.as_str()));
            if obs.tables.contains_key(&table)
                && !touched.contains(table.as_str())
                && !index_touched
            {
                plan.at_target.push(table);
            }
        }
    }

    plan
}

fn repair_environment_policy_row_security_sql(schema: &BareSchemaName) -> String {
    let qualified = format!("{}.environment_policies", schema.quoted());
    format!(
        "ALTER TABLE {qualified} ENABLE ROW LEVEL SECURITY; \
         ALTER TABLE {qualified} FORCE ROW LEVEL SECURITY; \
         DO $environment_policy_rows$ DECLARE policy_name text; BEGIN \
           FOR policy_name IN \
             SELECT policy.polname FROM pg_catalog.pg_policy AS policy \
              WHERE policy.polrelid = pg_catalog.to_regclass('{qualified}') \
           LOOP \
             EXECUTE pg_catalog.format( \
               'DROP POLICY %I ON {qualified}', policy_name); \
           END LOOP; \
         END $environment_policy_rows$; \
         CREATE POLICY environment_policies_tenant ON {qualified} \
           FOR SELECT TO wamn_app USING (wamn_authority.tenant_key(tenant_id) \
             = wamn_authority.current_tenant_key()); \
         CREATE POLICY environment_policies_platform ON {qualified} \
           AS PERMISSIVE FOR SELECT TO wamn_platform USING (true)"
    )
}

/// Strip retired registration keys that fail the current declaration parser.
///
/// Runs as the superuser across all tenants and preserves every retained key.
pub fn strip_retired_registration_keys_sql() -> &'static str {
    "UPDATE catalog.event_registrations \
     SET registration = registration - 'state' - 'partition-key' \
     WHERE registration ?| ARRAY['state', 'partition-key']"
}

// ---------------------------------------------------------------------------
// Observation SQL (the shell binds these; pinned by tests like the RI module's
// `select_replica_identity_sql`). SR12: the pure decision has no pg_catalog —
// the throwaway-PG gate covers that these really observe the live state.
// ---------------------------------------------------------------------------

/// Create or harden the host-only scenario-author group role.
pub fn ensure_scenario_author_role_sql() -> &'static str {
    "DO $scenario_author$ BEGIN \
       PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles \
                      WHERE rolname = 'wamn_scenario_author') THEN \
         CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
           NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
       ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles \
                     WHERE rolname = 'wamn_scenario_author' \
                       AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                            OR rolinherit OR rolreplication OR rolbypassrls)) THEN \
         ALTER ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
           NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
       END IF; \
     END $scenario_author$;"
}

/// Retained helper definitions plus retired names needed to observe cutovers
/// in `$1`. A retired helper the observation cannot name is a helper the
/// cutover can never be planned for.
pub fn select_run_plane_helper_functions_sql() -> &'static str {
    "SELECT p.proname, pg_get_functiondef(p.oid) \
     FROM pg_proc p \
     JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = $1 \
       AND p.proname IN ('lock_catalog_head', \
                         'guard_event_lineage_immutable', \
                         'reject_immutable_effect_fact_change', \
                         'reject_immutable_operator_run_action_change', \
                         'reject_immutable_authoring_report_change', \
                         'guard_authoring_report_write', \
                         'reject_immutable_authoring_test_set_change', \
                         'guard_effect_disposition_append', \
                         'guard_run_admission_pins_immutable', \
                         'guard_terminal_run_delete') \
     ORDER BY p.proname"
}

#[cfg(test)]
mod tests;
