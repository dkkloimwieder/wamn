//! The run-plane schema reconciler (E4/R14-migration, wamn-1wdq).
//!
//! `deploy/sql/run-state.sql` / `flows.sql` / `run-queue.sql` evolve, but a
//! schema instantiated from an older revision has NO migration path: the 2jkm.41
//! sweep found live demo schemas missing the E4 `stream_seq` column (runner
//! drains failed 42703), the fqg.20/D20 `partition_policy`, whole queue tables
//! (`poc_f1` predated per-project queue provisioning), and — after the ephemeral
//! fixture pod restarted — everything at once, including the `catalog` metadata
//! schema. This module is the PURE decision (the reconcile-replica-identity
//! precedent — no DB, clock, or wasm): given what the driver OBSERVED live
//! (tables + columns + indexes + CHECKs + user triggers + helper functions +
//! legacy outbox-era objects + the `catalog` schema state), it produces the
//! idempotent plan that
//! brings one project-env's run-plane schema to the schema of record. The
//! `wamn-ctl reconcile-run-plane` shell reads/executes; the throwaway-PG gate
//! proves the live transitions.
//!
//! **The schema of record is the deploy/sql source itself**, embedded at compile
//! time (`include_str!`) — the SAME files the wamn-gates `schema_drift` guard
//! (wamn-9mg8) pins — and sliced per table, so the plan can never drift from
//! what provisioning applies. Per-project schemas are the `wamn_run` → target
//! rewrite (`rewrite_schema`, the `publish-catalog --runstate` convention).
//!
//! What the plan covers (the wamn-1wdq manifestation set):
//!
//! 1. **Additive column drift** — a present table missing record columns gains
//!    `ALTER TABLE … ADD COLUMN <record definition>` (e.g. E4 `stream_seq
//!    bigint NOT NULL DEFAULT 0`, D20 `partition_policy … CHECK …`).
//! 2. **Index drift** — a record index absent live is created; a present one
//!    whose live definition lacks a record column the record definition names
//!    (the pre-E4 `run_queue_claimable` without `stream_seq`) is dropped and
//!    recreated from record.
//! 3. **Wholly-missing tables** — created from their record section (DDL +
//!    indexes + RLS + policy + grants), in file order so FKs resolve.
//! 4. **The pre-l5i9.19 outbox era** — legacy `outbox`/`evt_shadow` tables, the
//!    constant-named `wamn_outbox_event` trigger (per entity table) and its
//!    function are DROPPED (trigger before function — the function drop is
//!    RESTRICT), and stored registrations carrying the legacy `state` key are
//!    stripped (a state-carrying document fails parse post-teardown → HELD).
//! 5. **From-zero restore** — an empty database plans the full set, including
//!    `deploy/sql/catalog-schema.sql` (the `catalog` metadata schema the
//!    registration storage and the RI reconcile read).
//! 6. **Exact CHECK + trigger convergence** — every record-table CHECK is
//!    compared in PostgreSQL's canonical form; missing/drifted checks are added
//!    or replaced and non-record checks are removed. The run-state helper
//!    functions and lineage trigger are likewise repaired from record.
//!
//! **Data preserving:** the plan never
//! drops a live column, table, or index other than the named legacy outbox-era
//! objects and a stale-definition record index; live columns not in the record
//! are SURFACED (`extra_columns`), never touched. CHECK/trigger definitions may
//! be replaced to converge with record, but rows are never rewritten or
//! deleted: PostgreSQL validates new CHECKs against existing rows and aborts on
//! incompatible legacy data.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use wamn_pg_core::{Identifier, InvalidIdentifier};

/// The schema of record, compiled in — the same sources provisioning applies
/// (`publish-catalog --runstate`, the f1 provisioning Job) and the wamn-9mg8
/// stand-in drift guard pins.
const RUN_STATE_SQL: &str = include_str!("../../../../deploy/sql/run-state.sql");
const FLOWS_SQL: &str = include_str!("../../../../deploy/sql/flows.sql");
const FLOW_TESTS_SQL: &str = include_str!("../../../../deploy/sql/flow-tests.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");

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

/// PostgreSQL 18's canonical CHECK inventory for the four run-plane record
/// files. The live shell reads the same `pg_get_constraintdef(..., true)` form.
/// The throwaway-PG gate applies the deploy SQL and pins that this catalog is a
/// byte-for-byte projection of the schema of record.
const CHECK_SPECS: &[CheckSpec] = &[
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
        definition: "CHECK (status = ANY (ARRAY['dispatched'::text, 'running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text, 'infrastructure-failure'::text, 'effect-uncertain'::text]))",
        origin: CheckOrigin::Inline("status"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_admission_context_version_check",
        definition: "CHECK (admission_context_version > 0)",
        origin: CheckOrigin::Inline("admission_context_version"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_invoke_depth_check",
        definition: "CHECK (invoke_depth >= 0)",
        origin: CheckOrigin::Inline("invoke_depth"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_caller_outcome_kind_check",
        definition: "CHECK (caller_outcome_kind = ANY (ARRAY['responded'::text, 'failed'::text, 'cancelled'::text]))",
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
        definition: "CHECK (fail_kind = ANY (ARRAY['terminal'::text, 'retry-exhausted'::text, 'invalid-input'::text, 'runaway-budget'::text, 'effect-uncertain'::text]))",
        origin: CheckOrigin::Inline("fail_kind"),
    },
    CheckSpec {
        table: "runs",
        name: "runs_check",
        definition: "CHECK ((catalog_id IS NULL) = (catalog_version IS NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_environment_check",
        definition: "CHECK (environment IS NULL OR environment <> ''::text)",
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
        name: "runs_check3",
        definition: "CHECK ((parent_run_id IS NULL) = (parent_node_id IS NULL) AND (parent_run_id IS NULL) = (parent_occurrence IS NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check4",
        definition: "CHECK ((parent_run_id IS NULL) = (invoke_root_run_id IS NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check5",
        definition: "CHECK ((waiting_child_run_id IS NULL) = (waiting_child_occurrence IS NULL) AND (waiting_child_run_id IS NULL) = (wait_generation IS NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check6",
        definition: "CHECK ((cancel_requested_kind IS NULL) = (cancel_requested_at IS NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check7",
        definition: "CHECK ((caller_released_at IS NULL) = (caller_outcome_kind IS NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check8",
        definition: "CHECK (caller_outcome_kind IS NULL OR caller_outcome_json IS NOT NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check9",
        definition: "CHECK (caller_outcome_kind <> 'responded'::text OR caller_release_node_id IS NOT NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "runs",
        name: "runs_check10",
        definition: "CHECK (response_deadline_at IS NULL OR run_deadline_at IS NULL OR response_deadline_at <= run_deadline_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "invocation_admissions",
        name: "invocation_admissions_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_status_check",
        definition: "CHECK (status = ANY (ARRAY['started'::text, 'success'::text, 'error'::text]))",
        origin: CheckOrigin::Inline("status"),
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_selected_recovery_class_check",
        definition: "CHECK (selected_recovery_class = ANY (ARRAY['replay'::text, 'idempotent-with-key'::text, 'never-replay'::text]))",
        origin: CheckOrigin::Inline("selected_recovery_class"),
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_recovery_class_check",
        definition: "CHECK (recovery_class = ANY (ARRAY['replay'::text, 'idempotent-with-key'::text, 'never-replay'::text]))",
        origin: CheckOrigin::Inline("recovery_class"),
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_generation_fact_kind_check",
        definition: "CHECK (generation_fact_kind = ANY (ARRAY['not-required'::text, 'attested'::text]))",
        origin: CheckOrigin::Inline("generation_fact_kind"),
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_error_kind_check",
        definition: "CHECK (error_kind = ANY (ARRAY['retryable'::text, 'rate-limited'::text, 'terminal'::text, 'invalid-input'::text, 'cancelled'::text]))",
        origin: CheckOrigin::Inline("error_kind"),
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_check",
        definition: "CHECK (status <> 'started'::text OR selected_recovery_class IS NOT NULL AND recovery_class IS NOT NULL AND selected_recovery_class = recovery_class AND generation_fact_kind IS NOT NULL AND attempt_started_at IS NOT NULL AND attempt_deadline_at IS NOT NULL AND attempt_input_ref IS NOT NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_check1",
        definition: "CHECK (generation_fact_kind = 'not-required'::text AND connection_generation IS NULL AND credential_generation IS NULL OR generation_fact_kind = 'attested'::text AND connection_generation IS NOT NULL AND connection_generation <> ''::text AND credential_generation IS NOT NULL AND credential_generation <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_check2",
        definition: "CHECK (attempt_deadline_at IS NULL OR attempt_started_at IS NULL OR attempt_started_at <= attempt_deadline_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_check3",
        definition: "CHECK (attempt_dispatched_at IS NULL OR attempt_started_at IS NULL OR attempt_started_at <= attempt_dispatched_at)",
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
        name: "effect_attempts_attempt_index_check",
        definition: "CHECK (attempt_index >= 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_lineage_check",
        definition: "CHECK (legacy_imported AND predecessor_attempt_id IS NULL OR NOT legacy_imported AND (attempt_index = 0 AND predecessor_attempt_id IS NULL OR attempt_index > 0 AND predecessor_attempt_id IS NOT NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_attempts",
        name: "effect_attempts_recovery_class_check",
        definition: "CHECK ((selected_recovery_class = ANY (ARRAY['replay'::text, 'idempotent-with-key'::text, 'never-replay'::text])) AND (recovery_class = ANY (ARRAY['replay'::text, 'idempotent-with-key'::text, 'never-replay'::text])) AND selected_recovery_class = recovery_class)",
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
        table: "effect_attempts",
        name: "effect_attempts_key_check",
        definition: "CHECK (recovery_class <> 'idempotent-with-key'::text OR attempt_key IS NOT NULL AND attempt_key <> ''::text)",
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
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_tenant_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_action_check",
        definition: "CHECK (action = ANY (ARRAY['park'::text, 'release'::text, 'resolve'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_selection_check",
        definition: "CHECK (selection_kind = ANY (ARRAY['single'::text, 'bulk'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_principal_check",
        definition: "CHECK (principal <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_role_check",
        definition: "CHECK (effective_role = ANY (ARRAY['system'::text, 'project-deployer'::text, 'project-admin'::text, 'platform-admin-break-glass'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_role_action_check",
        definition: "CHECK (effective_role = 'system'::text AND action = 'park'::text AND selection_kind = 'single'::text OR effective_role = 'project-deployer'::text AND (action = ANY (ARRAY['park'::text, 'release'::text])) OR (effective_role = ANY (ARRAY['project-admin'::text, 'platform-admin-break-glass'::text])))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_basis_check",
        definition: "CHECK (basis IS NULL OR (basis = ANY (ARRAY['external-evidence'::text, 'counterparty-confirmation'::text, 'operator-judgment'::text])))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_correlation_check",
        definition: "CHECK (correlation_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_resolution_audit_check",
        definition: "CHECK (action = 'resolve'::text AND basis IS NOT NULL AND evidence_ref IS NOT NULL AND evidence_ref <> ''::text OR action <> 'resolve'::text AND basis IS NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_break_glass_check",
        definition: "CHECK (effective_role = 'platform-admin-break-glass'::text AND break_glass_reason IS NOT NULL AND break_glass_reason <> ''::text OR effective_role <> 'platform-admin-break-glass'::text AND break_glass_reason IS NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_bulk_bounds_check",
        definition: "CHECK (selection_kind <> 'bulk'::text OR connection_name IS NOT NULL AND connection_name <> ''::text AND connection_generation IS NOT NULL AND connection_generation <> ''::text AND window_start IS NOT NULL AND window_end IS NOT NULL AND isfinite(window_start) AND isfinite(window_end) AND window_start < window_end)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_disposition_requests",
        name: "effect_disposition_requests_single_filters_check",
        definition: "CHECK (selection_kind <> 'single'::text OR connection_name IS NULL AND connection_generation IS NULL AND flow_id IS NULL AND window_start IS NULL AND window_end IS NULL)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_dispositions",
        name: "effect_dispositions_tenant_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_dispositions",
        name: "effect_dispositions_selection_ordinal_check",
        definition: "CHECK (selection_ordinal >= 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_dispositions",
        name: "effect_dispositions_action_check",
        definition: "CHECK (action = ANY (ARRAY['park'::text, 'release'::text, 'resolve'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_dispositions",
        name: "effect_dispositions_resolution_status_check",
        definition: "CHECK (resolution_status IS NULL OR (resolution_status = ANY (ARRAY['succeeded'::text, 'failed'::text])))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_dispositions",
        name: "effect_dispositions_failure_kind_check",
        definition: "CHECK (failure_kind IS NULL OR (failure_kind = ANY (ARRAY['terminal'::text, 'invalid-input'::text])))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "effect_dispositions",
        name: "effect_dispositions_outcome_check",
        definition: "CHECK ((action <> 'resolve'::text AND resolution_status IS NULL AND success_payload IS NULL AND success_port IS NULL AND success_context IS NULL AND failure_kind IS NULL AND failure_detail IS NULL OR action = 'resolve'::text AND resolution_status = 'succeeded'::text AND success_payload IS NOT NULL AND success_port IS NOT NULL AND success_port <> ''::text AND (success_context IS NULL OR jsonb_typeof(success_context) = 'object'::text) AND failure_kind IS NULL AND failure_detail IS NULL OR action = 'resolve'::text AND resolution_status = 'failed'::text AND success_payload IS NULL AND success_port IS NULL AND success_context IS NULL AND (failure_kind = ANY (ARRAY['terminal'::text, 'invalid-input'::text])) AND failure_detail IS NOT NULL AND jsonb_typeof(failure_detail) = 'object'::text AND failure_detail ? 'message'::text AND jsonb_typeof(failure_detail -> 'message'::text) = 'string'::text AND (NOT failure_detail ? 'code'::text OR (failure_detail -> 'code'::text) = 'null'::jsonb OR jsonb_typeof(failure_detail -> 'code'::text) = 'string'::text)) IS TRUE)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "flows",
        name: "flows_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "test_suites",
        name: "test_suites_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "test_cases",
        name: "test_cases_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "authoring_test_sets",
        name: "authoring_test_sets_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "authoring_test_sets",
        name: "authoring_test_sets_schema_version_check",
        definition: "CHECK (schema_version = '0.1'::text)",
        origin: CheckOrigin::Inline("schema_version"),
    },
    CheckSpec {
        table: "authoring_test_sets",
        name: "authoring_test_sets_byte_length_check",
        definition: "CHECK (byte_length >= 1 AND byte_length <= 1048576)",
        origin: CheckOrigin::Inline("byte_length"),
    },
    CheckSpec {
        table: "authoring_test_sets",
        name: "authoring_test_sets_check",
        definition: "CHECK (byte_length = octet_length(exact_bytes))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_sets",
        name: "authoring_test_sets_check1",
        definition: "CHECK (test_set_hash = ('sha256:'::text || encode(sha256(exact_bytes), 'hex'::text)))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_sets",
        name: "authoring_test_sets_check2",
        definition: "CHECK (NOT (convert_from(exact_bytes, 'UTF8'::name)::jsonb ->> 'schema-version'::text) IS DISTINCT FROM schema_version)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_report_id_check",
        definition: "CHECK (report_id <> ''::text)",
        origin: CheckOrigin::Inline("report_id"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_execution_id_check",
        definition: "CHECK (execution_id <> ''::text)",
        origin: CheckOrigin::Inline("execution_id"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_flow_id_check",
        definition: "CHECK (flow_id <> ''::text)",
        origin: CheckOrigin::Inline("flow_id"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_suite_flow_version_check",
        definition: "CHECK (suite_flow_version > 0)",
        origin: CheckOrigin::Inline("suite_flow_version"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_suite_id_check",
        definition: "CHECK (suite_id <> ''::text)",
        origin: CheckOrigin::Inline("suite_id"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_command_json_check",
        definition: "CHECK (jsonb_typeof(command_json) = 'object'::text)",
        origin: CheckOrigin::Inline("command_json"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_command_hash_check",
        definition: "CHECK (command_hash <> ''::text)",
        origin: CheckOrigin::Inline("command_hash"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_lineage_json_check",
        definition: "CHECK (jsonb_typeof(lineage_json) = 'object'::text AND ((lineage_json ->> 'kind'::text) = ANY (ARRAY['draft'::text, 'release'::text])))",
        origin: CheckOrigin::Inline("lineage_json"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_lineage_hash_check",
        definition: "CHECK (lineage_hash <> ''::text)",
        origin: CheckOrigin::Inline("lineage_hash"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_state_check",
        definition: "CHECK (state = ANY (ARRAY['pending'::text, 'finalized'::text]))",
        origin: CheckOrigin::Inline("state"),
    },
    CheckSpec {
        table: "authoring_report_reservations",
        name: "authoring_report_reservations_finalization_pair",
        definition: "CHECK (state = 'pending'::text AND finalized_at IS NULL OR state = 'finalized'::text AND finalized_at IS NOT NULL AND finalized_at >= created_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_suite_case_facts",
        name: "authoring_suite_case_facts_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "authoring_suite_case_facts",
        name: "authoring_suite_case_facts_report_id_check",
        definition: "CHECK (report_id <> ''::text)",
        origin: CheckOrigin::Inline("report_id"),
    },
    CheckSpec {
        table: "authoring_suite_case_facts",
        name: "authoring_suite_case_facts_ordinal_check",
        definition: "CHECK (ordinal >= 0)",
        origin: CheckOrigin::Inline("ordinal"),
    },
    CheckSpec {
        table: "authoring_suite_case_facts",
        name: "authoring_suite_case_facts_case_id_check",
        definition: "CHECK (case_id <> ''::text)",
        origin: CheckOrigin::Inline("case_id"),
    },
    CheckSpec {
        table: "authoring_suite_case_facts",
        name: "authoring_suite_case_facts_run_id_check",
        definition: "CHECK (run_id <> ''::text)",
        origin: CheckOrigin::Inline("run_id"),
    },
    CheckSpec {
        table: "authoring_suite_case_facts",
        name: "authoring_suite_case_facts_status_check",
        definition: "CHECK (status = ANY (ARRAY['dispatched'::text, 'running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text, 'infrastructure-failure'::text, 'effect-uncertain'::text]))",
        origin: CheckOrigin::Inline("status"),
    },
    CheckSpec {
        table: "authoring_suite_case_facts",
        name: "authoring_suite_case_facts_fail_kind_check",
        definition: "CHECK (fail_kind = ANY (ARRAY['terminal'::text, 'retry-exhausted'::text, 'invalid-input'::text, 'runaway-budget'::text, 'effect-uncertain'::text]))",
        origin: CheckOrigin::Inline("fail_kind"),
    },
    CheckSpec {
        table: "authoring_suite_case_facts",
        name: "authoring_suite_case_facts_outcome_check",
        definition: "CHECK (jsonb_typeof(outcome) = 'object'::text)",
        origin: CheckOrigin::Inline("outcome"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_report_id_check",
        definition: "CHECK (report_id <> ''::text)",
        origin: CheckOrigin::Inline("report_id"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_execution_id_check",
        definition: "CHECK (execution_id <> ''::text)",
        origin: CheckOrigin::Inline("execution_id"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_flow_id_check",
        definition: "CHECK (flow_id <> ''::text)",
        origin: CheckOrigin::Inline("flow_id"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_suite_flow_version_check",
        definition: "CHECK (suite_flow_version > 0)",
        origin: CheckOrigin::Inline("suite_flow_version"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_suite_id_check",
        definition: "CHECK (suite_id <> ''::text)",
        origin: CheckOrigin::Inline("suite_id"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_lineage_json_check",
        definition: "CHECK (jsonb_typeof(lineage_json) = 'object'::text AND ((lineage_json ->> 'kind'::text) = ANY (ARRAY['draft'::text, 'release'::text])))",
        origin: CheckOrigin::Inline("lineage_json"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_lineage_hash_check",
        definition: "CHECK (lineage_hash <> ''::text)",
        origin: CheckOrigin::Inline("lineage_hash"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_edit_to_run_ms_check",
        definition: "CHECK (edit_to_run_ms IS NULL OR edit_to_run_ms >= 0)",
        origin: CheckOrigin::Inline("edit_to_run_ms"),
    },
    CheckSpec {
        table: "authoring_suite_reports",
        name: "authoring_suite_reports_refusal_check",
        definition: "CHECK (refusal IS NULL OR jsonb_typeof(refusal) = 'object'::text)",
        origin: CheckOrigin::Inline("refusal"),
    },
    CheckSpec {
        table: "run_queue",
        name: "run_queue_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "run_queue",
        name: "run_queue_partition_policy_check",
        definition: "CHECK (partition_policy = ANY (ARRAY['blocking'::text, 'leapfrog'::text]))",
        origin: CheckOrigin::Inline("partition_policy"),
    },
    CheckSpec {
        table: "run_queue",
        name: "run_queue_lease_generation_check",
        definition: "CHECK (lease_generation >= 0)",
        origin: CheckOrigin::Inline("lease_generation"),
    },
    CheckSpec {
        table: "partition_owner",
        name: "partition_owner_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
    CheckSpec {
        table: "run_dead_letters",
        name: "run_dead_letters_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Inline("tenant_id"),
    },
];

const LOCK_CATALOG_HEAD_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.lock_catalog_head(p_tenant_id text, p_catalog_id text, p_environment text)\n RETURNS integer\n LANGUAGE plpgsql\n SECURITY DEFINER\n SET search_path TO 'pg_catalog', 'catalog'\nAS $function$\nDECLARE\n    applied_version int;\nBEGIN\n    SELECT head.applied_catalog_version INTO applied_version\n    FROM catalog.catalog_heads AS head\n    WHERE p_tenant_id = NULLIF(current_setting('app.tenant', true), '')\n      AND head.tenant_id = p_tenant_id\n      AND head.catalog_id = p_catalog_id\n      AND head.environment = p_environment\n    FOR SHARE OF head;\n    RETURN applied_version;\nEND\n$function$\n";

const GUARD_EVENT_LINEAGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_event_lineage_immutable()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF NEW.event_source_run_id IS DISTINCT FROM OLD.event_source_run_id\n       OR NEW.event_root_run_id IS DISTINCT FROM OLD.event_root_run_id\n       OR NEW.event_depth IS DISTINCT FROM OLD.event_depth THEN\n        RAISE EXCEPTION 'event causation lineage is immutable';\n    END IF;\n    RETURN NEW;\nEND\n$function$\n";

const REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_effect_fact_change()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    RAISE EXCEPTION USING\n        ERRCODE = '55000',\n        MESSAGE = 'effect-disposition-immutable';\nEND\n$function$\n";

const REJECT_IMMUTABLE_AUTHORING_REPORT_CHANGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_authoring_report_change()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    RAISE EXCEPTION USING\n        ERRCODE = '55000',\n        MESSAGE = 'authoring-report-immutable';\nEND\n$function$\n";

const REJECT_IMMUTABLE_AUTHORING_TEST_SET_CHANGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_authoring_test_set_change()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    RAISE EXCEPTION USING\n        ERRCODE = '55000',\n        MESSAGE = 'authoring-test-set-immutable';\nEND\n$function$\n";

const GUARD_AUTHORING_REPORT_WRITE_DEF: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_authoring_report_write()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
DECLARE
    new_row jsonb := to_jsonb(NEW);
    old_row jsonb := CASE WHEN TG_OP = 'UPDATE' THEN to_jsonb(OLD) END;
    reservation_command jsonb;
    expected_case_count bigint;
    actual_case_count bigint;
    max_fact_ordinal int;
    all_facts_passed boolean;
BEGIN
    IF TG_TABLE_NAME = 'authoring_report_reservations' THEN
        IF TG_OP = 'INSERT' THEN
            IF new_row ->> 'state' <> 'pending'
               OR new_row -> 'finalized_at' <> 'null'::jsonb THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'authoring-report-reservation-must-start-pending';
            END IF;
            IF jsonb_typeof(new_row -> 'command_json' -> 'cases')
                   IS DISTINCT FROM 'array' THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
            IF jsonb_array_length(new_row -> 'command_json' -> 'cases')
                   > 2147483647 THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
            IF EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                WHERE jsonb_typeof(command_case.value) <> 'object'
                   OR NULLIF(command_case.value ->> 'case-id', '') IS NULL
                   OR NULLIF(command_case.value ->> 'case-content-hash', '') IS NULL
                   OR NULLIF(command_case.value ->> 'run-id', '') IS NULL
                   OR NULLIF(command_case.value ->> 'execution-schema', '') IS NULL
            ) OR EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                GROUP BY command_case.value ->> 'case-id'
                HAVING count(*) > 1
            ) OR EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                GROUP BY command_case.value ->> 'run-id'
                HAVING count(*) > 1
            ) THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
        ELSIF TG_OP = 'UPDATE' THEN
            IF (new_row - 'state' - 'finalized_at')
                   IS DISTINCT FROM (old_row - 'state' - 'finalized_at')
               OR old_row ->> 'state' <> 'pending'
               OR new_row ->> 'state' <> 'finalized'
               OR new_row -> 'finalized_at' = 'null'::jsonb
               OR NOT EXISTS (
                   SELECT 1 FROM wamn_run.authoring_suite_reports AS report
                   WHERE report.tenant_id = old_row ->> 'tenant_id'
                     AND report.report_id = old_row ->> 'report_id'
               ) THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'authoring-report-reservation-uncontrolled-update';
            END IF;
        ELSE
            RAISE EXCEPTION USING
                ERRCODE = '55000',
                MESSAGE = 'authoring-report-reservation-unexpected-operation';
        END IF;
    ELSIF TG_TABLE_NAME = 'authoring_suite_case_facts' AND TG_OP = 'INSERT' THEN
        SELECT reservation.command_json INTO reservation_command
        FROM wamn_run.authoring_report_reservations AS reservation
        WHERE reservation.tenant_id = new_row ->> 'tenant_id'
          AND reservation.report_id = new_row ->> 'report_id'
          AND reservation.state = 'pending'
        FOR UPDATE;
        IF reservation_command IS NULL OR NOT EXISTS (
            SELECT 1
            FROM jsonb_array_elements(
                reservation_command -> 'cases'
            ) WITH ORDINALITY AS command_case(value, position)
            WHERE command_case.position - 1
                    = (new_row ->> 'ordinal')::bigint
              AND command_case.value ->> 'case-id'
                    = new_row ->> 'case_id'
              AND command_case.value ->> 'run-id'
                    = new_row ->> 'run_id'
              AND NULLIF(command_case.value ->> 'case-content-hash', '')
                    IS NOT NULL
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-case-fact-command-mismatch';
        END IF;
    ELSIF TG_TABLE_NAME = 'authoring_suite_reports' AND TG_OP = 'INSERT' THEN
        SELECT reservation.command_json INTO reservation_command
        FROM wamn_run.authoring_report_reservations AS reservation
        WHERE reservation.tenant_id = new_row ->> 'tenant_id'
          AND reservation.report_id = new_row ->> 'report_id'
          AND reservation.execution_id = new_row ->> 'execution_id'
          AND reservation.flow_id = new_row ->> 'flow_id'
          AND reservation.suite_flow_version
                = (new_row ->> 'suite_flow_version')::int
          AND reservation.suite_id = new_row ->> 'suite_id'
          AND reservation.lineage_json = new_row -> 'lineage_json'
          AND reservation.lineage_hash = new_row ->> 'lineage_hash'
          AND reservation.state = 'pending'
        FOR UPDATE;
        IF reservation_command IS NULL THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-reservation-mismatch';
        END IF;

        expected_case_count := jsonb_array_length(
            reservation_command -> 'cases'
        );
        SELECT count(*), COALESCE(max(fact.ordinal), -1),
               COALESCE(bool_and(fact.passed), true)
        INTO actual_case_count, max_fact_ordinal, all_facts_passed
        FROM wamn_run.authoring_suite_case_facts AS fact
        WHERE fact.tenant_id = new_row ->> 'tenant_id'
          AND fact.report_id = new_row ->> 'report_id';

        IF (new_row -> 'refusal' = 'null'::jsonb
            AND actual_case_count <> expected_case_count)
           OR (new_row -> 'refusal' <> 'null'::jsonb
               AND actual_case_count > expected_case_count)
           OR max_fact_ordinal <> actual_case_count - 1 THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-case-cardinality-mismatch';
        END IF;
        IF (new_row ->> 'passed')::boolean IS DISTINCT FROM
           (new_row -> 'refusal' = 'null'::jsonb AND all_facts_passed) THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-summary-mismatch';
        END IF;
    ELSE
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'authoring-report-unexpected-write';
    END IF;
    RETURN NEW;
END
$function$
"#;

const GUARD_EFFECT_FACT_APPEND_DEF: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_effect_fact_append()
 RETURNS trigger
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE
    current_can_migrate boolean := COALESCE(
        (SELECT candidate.rolsuper OR candidate.rolbypassrls
         FROM pg_catalog.pg_roles AS candidate
         WHERE candidate.rolname = CURRENT_USER),
        false
    );
BEGIN
    IF NOT current_can_migrate THEN
        RAISE EXCEPTION USING
            ERRCODE = '42501',
            MESSAGE = 'effect-fact-append-requires-migration-authority';
    END IF;
    RETURN NEW;
END
$function$
"#;

const GUARD_EFFECT_DISPOSITION_APPEND_DEF: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_effect_disposition_append()
 RETURNS trigger
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE
    owner_name text := pg_catalog.pg_get_userbyid((
        SELECT rel.relowner
        FROM pg_catalog.pg_class AS rel
        WHERE rel.oid = TG_RELID
    ));
    current_is_super boolean := COALESCE(
        (SELECT candidate.rolsuper
         FROM pg_catalog.pg_roles AS candidate
         WHERE candidate.rolname = CURRENT_USER),
        false
    );
BEGIN
    IF NOT current_is_super
       AND NOT (CURRENT_USER = owner_name AND CURRENT_USER <> SESSION_USER) THEN
        RAISE EXCEPTION USING
            ERRCODE = '42501',
            MESSAGE = 'effect-disposition-append-requires-trusted-adapter';
    END IF;
    RETURN NEW;
END
$function$
"#;

const RUNS_EVENT_LINEAGE_TRIGGER_DEF: &str = "CREATE TRIGGER runs_event_lineage_immutable BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_event_lineage_immutable()";

const LOCK_CATALOG_HEAD_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.lock_catalog_head(
    p_tenant_id text,
    p_catalog_id text,
    p_environment text
)
RETURNS int
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, catalog
AS $$
DECLARE
    applied_version int;
BEGIN
    SELECT head.applied_catalog_version INTO applied_version
    FROM catalog.catalog_heads AS head
    WHERE p_tenant_id = NULLIF(current_setting('app.tenant', true), '')
      AND head.tenant_id = p_tenant_id
      AND head.catalog_id = p_catalog_id
      AND head.environment = p_environment
    FOR SHARE OF head;
    RETURN applied_version;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.lock_catalog_head(text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION wamn_run.lock_catalog_head(text, text, text) TO wamn_app;
GRANT EXECUTE ON FUNCTION wamn_run.lock_catalog_head(text, text, text)
    TO wamn_scenario_author;"#;

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

const REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_effect_fact_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'effect-disposition-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_effect_fact_change() FROM PUBLIC;"#;

const REJECT_IMMUTABLE_AUTHORING_REPORT_CHANGE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_authoring_report_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'authoring-report-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_authoring_report_change() FROM PUBLIC;"#;

const REJECT_IMMUTABLE_AUTHORING_TEST_SET_CHANGE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_authoring_test_set_change()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'authoring-test-set-immutable';
END
$$;
REVOKE ALL ON FUNCTION wamn_run.reject_immutable_authoring_test_set_change() FROM PUBLIC;"#;

const GUARD_AUTHORING_REPORT_WRITE_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_authoring_report_write()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    new_row jsonb := to_jsonb(NEW);
    old_row jsonb := CASE WHEN TG_OP = 'UPDATE' THEN to_jsonb(OLD) END;
    reservation_command jsonb;
    expected_case_count bigint;
    actual_case_count bigint;
    max_fact_ordinal int;
    all_facts_passed boolean;
BEGIN
    IF TG_TABLE_NAME = 'authoring_report_reservations' THEN
        IF TG_OP = 'INSERT' THEN
            IF new_row ->> 'state' <> 'pending'
               OR new_row -> 'finalized_at' <> 'null'::jsonb THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'authoring-report-reservation-must-start-pending';
            END IF;
            IF jsonb_typeof(new_row -> 'command_json' -> 'cases')
                   IS DISTINCT FROM 'array' THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
            IF jsonb_array_length(new_row -> 'command_json' -> 'cases')
                   > 2147483647 THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
            IF EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                WHERE jsonb_typeof(command_case.value) <> 'object'
                   OR NULLIF(command_case.value ->> 'case-id', '') IS NULL
                   OR NULLIF(command_case.value ->> 'case-content-hash', '') IS NULL
                   OR NULLIF(command_case.value ->> 'run-id', '') IS NULL
                   OR NULLIF(command_case.value ->> 'execution-schema', '') IS NULL
            ) OR EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                GROUP BY command_case.value ->> 'case-id'
                HAVING count(*) > 1
            ) OR EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    new_row -> 'command_json' -> 'cases'
                ) AS command_case(value)
                GROUP BY command_case.value ->> 'run-id'
                HAVING count(*) > 1
            ) THEN
                RAISE EXCEPTION USING
                    ERRCODE = '23514',
                    MESSAGE = 'authoring-report-command-cases-invalid';
            END IF;
        ELSIF TG_OP = 'UPDATE' THEN
            IF (new_row - 'state' - 'finalized_at')
                   IS DISTINCT FROM (old_row - 'state' - 'finalized_at')
               OR old_row ->> 'state' <> 'pending'
               OR new_row ->> 'state' <> 'finalized'
               OR new_row -> 'finalized_at' = 'null'::jsonb
               OR NOT EXISTS (
                   SELECT 1 FROM wamn_run.authoring_suite_reports AS report
                   WHERE report.tenant_id = old_row ->> 'tenant_id'
                     AND report.report_id = old_row ->> 'report_id'
               ) THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'authoring-report-reservation-uncontrolled-update';
            END IF;
        ELSE
            RAISE EXCEPTION USING
                ERRCODE = '55000',
                MESSAGE = 'authoring-report-reservation-unexpected-operation';
        END IF;
    ELSIF TG_TABLE_NAME = 'authoring_suite_case_facts' AND TG_OP = 'INSERT' THEN
        SELECT reservation.command_json INTO reservation_command
        FROM wamn_run.authoring_report_reservations AS reservation
        WHERE reservation.tenant_id = new_row ->> 'tenant_id'
          AND reservation.report_id = new_row ->> 'report_id'
          AND reservation.state = 'pending'
        FOR UPDATE;
        IF reservation_command IS NULL OR NOT EXISTS (
            SELECT 1
            FROM jsonb_array_elements(
                reservation_command -> 'cases'
            ) WITH ORDINALITY AS command_case(value, position)
            WHERE command_case.position - 1
                    = (new_row ->> 'ordinal')::bigint
              AND command_case.value ->> 'case-id'
                    = new_row ->> 'case_id'
              AND command_case.value ->> 'run-id'
                    = new_row ->> 'run_id'
              AND NULLIF(command_case.value ->> 'case-content-hash', '')
                    IS NOT NULL
        ) THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-case-fact-command-mismatch';
        END IF;
    ELSIF TG_TABLE_NAME = 'authoring_suite_reports' AND TG_OP = 'INSERT' THEN
        SELECT reservation.command_json INTO reservation_command
        FROM wamn_run.authoring_report_reservations AS reservation
        WHERE reservation.tenant_id = new_row ->> 'tenant_id'
          AND reservation.report_id = new_row ->> 'report_id'
          AND reservation.execution_id = new_row ->> 'execution_id'
          AND reservation.flow_id = new_row ->> 'flow_id'
          AND reservation.suite_flow_version
                = (new_row ->> 'suite_flow_version')::int
          AND reservation.suite_id = new_row ->> 'suite_id'
          AND reservation.lineage_json = new_row -> 'lineage_json'
          AND reservation.lineage_hash = new_row ->> 'lineage_hash'
          AND reservation.state = 'pending'
        FOR UPDATE;
        IF reservation_command IS NULL THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-reservation-mismatch';
        END IF;

        expected_case_count := jsonb_array_length(
            reservation_command -> 'cases'
        );
        SELECT count(*), COALESCE(max(fact.ordinal), -1),
               COALESCE(bool_and(fact.passed), true)
        INTO actual_case_count, max_fact_ordinal, all_facts_passed
        FROM wamn_run.authoring_suite_case_facts AS fact
        WHERE fact.tenant_id = new_row ->> 'tenant_id'
          AND fact.report_id = new_row ->> 'report_id';

        IF (new_row -> 'refusal' = 'null'::jsonb
            AND actual_case_count <> expected_case_count)
           OR (new_row -> 'refusal' <> 'null'::jsonb
               AND actual_case_count > expected_case_count)
           OR max_fact_ordinal <> actual_case_count - 1 THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-case-cardinality-mismatch';
        END IF;
        IF (new_row ->> 'passed')::boolean IS DISTINCT FROM
           (new_row -> 'refusal' = 'null'::jsonb AND all_facts_passed) THEN
            RAISE EXCEPTION USING
                ERRCODE = '23514',
                MESSAGE = 'authoring-report-summary-mismatch';
        END IF;
    ELSE
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'authoring-report-unexpected-write';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_authoring_report_write() FROM PUBLIC;"#;

const GUARD_EFFECT_FACT_APPEND_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_effect_fact_append()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    current_can_migrate boolean := COALESCE(
        (SELECT candidate.rolsuper OR candidate.rolbypassrls
         FROM pg_catalog.pg_roles AS candidate
         WHERE candidate.rolname = CURRENT_USER),
        false
    );
BEGIN
    IF NOT current_can_migrate THEN
        RAISE EXCEPTION USING
            ERRCODE = '42501',
            MESSAGE = 'effect-fact-append-requires-migration-authority';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_effect_fact_append() FROM PUBLIC;"#;

const GUARD_EFFECT_DISPOSITION_APPEND_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.guard_effect_disposition_append()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    owner_name text := pg_catalog.pg_get_userbyid((
        SELECT rel.relowner
        FROM pg_catalog.pg_class AS rel
        WHERE rel.oid = TG_RELID
    ));
    current_is_super boolean := COALESCE(
        (SELECT candidate.rolsuper
         FROM pg_catalog.pg_roles AS candidate
         WHERE candidate.rolname = CURRENT_USER),
        false
    );
BEGIN
    IF NOT current_is_super
       AND NOT (CURRENT_USER = owner_name AND CURRENT_USER <> SESSION_USER) THEN
        RAISE EXCEPTION USING
            ERRCODE = '42501',
            MESSAGE = 'effect-disposition-append-requires-trusted-adapter';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.guard_effect_disposition_append() FROM PUBLIC;"#;

const RUNS_EVENT_LINEAGE_TRIGGER_SQL: &str = "CREATE TRIGGER runs_event_lineage_immutable \
    BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth \
    ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION \
    wamn_run.guard_event_lineage_immutable();";

struct HelperSpec {
    name: &'static str,
    definition: &'static str,
    sql: &'static str,
}

const HELPER_SPECS: &[HelperSpec] = &[
    HelperSpec {
        name: "lock_catalog_head",
        definition: LOCK_CATALOG_HEAD_DEF,
        sql: LOCK_CATALOG_HEAD_SQL,
    },
    HelperSpec {
        name: "guard_event_lineage_immutable",
        definition: GUARD_EVENT_LINEAGE_DEF,
        sql: GUARD_EVENT_LINEAGE_SQL,
    },
    HelperSpec {
        name: "reject_immutable_effect_fact_change",
        definition: REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_DEF,
        sql: REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_SQL,
    },
    HelperSpec {
        name: "reject_immutable_authoring_test_set_change",
        definition: REJECT_IMMUTABLE_AUTHORING_TEST_SET_CHANGE_DEF,
        sql: REJECT_IMMUTABLE_AUTHORING_TEST_SET_CHANGE_SQL,
    },
    HelperSpec {
        name: "reject_immutable_authoring_report_change",
        definition: REJECT_IMMUTABLE_AUTHORING_REPORT_CHANGE_DEF,
        sql: REJECT_IMMUTABLE_AUTHORING_REPORT_CHANGE_SQL,
    },
    HelperSpec {
        name: "guard_authoring_report_write",
        definition: GUARD_AUTHORING_REPORT_WRITE_DEF,
        sql: GUARD_AUTHORING_REPORT_WRITE_SQL,
    },
    HelperSpec {
        name: "guard_effect_fact_append",
        definition: GUARD_EFFECT_FACT_APPEND_DEF,
        sql: GUARD_EFFECT_FACT_APPEND_SQL,
    },
    HelperSpec {
        name: "guard_effect_disposition_append",
        definition: GUARD_EFFECT_DISPOSITION_APPEND_DEF,
        sql: GUARD_EFFECT_DISPOSITION_APPEND_SQL,
    },
];

#[derive(Debug)]
struct TriggerSpec {
    table: String,
    name: String,
    definition: String,
    sql: String,
}

fn trigger_specs() -> Vec<TriggerSpec> {
    let mut specs = vec![TriggerSpec {
        table: "runs".to_string(),
        name: "runs_event_lineage_immutable".to_string(),
        definition: RUNS_EVENT_LINEAGE_TRIGGER_DEF.to_string(),
        sql: RUNS_EVENT_LINEAGE_TRIGGER_SQL.to_string(),
    }];
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
        "effect_disposition_requests",
        "effect_dispositions",
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
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        let name = format!("{table}_insert_guard");
        specs.push(TriggerSpec {
            table: table.to_string(),
            name: name.clone(),
            definition: format!(
                "CREATE TRIGGER {name} BEFORE INSERT ON wamn_run.{table} FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_fact_append()"
            ),
            sql: format!(
                "CREATE TRIGGER {name} BEFORE INSERT ON wamn_run.{table} FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_fact_append();"
            ),
        });
    }
    for (table, name) in [
        (
            "effect_disposition_requests",
            "effect_disposition_requests_insert_guard",
        ),
        ("effect_dispositions", "effect_dispositions_insert_guard"),
    ] {
        specs.push(TriggerSpec {
            table: table.to_string(),
            name: name.to_string(),
            definition: format!(
                "CREATE TRIGGER {name} BEFORE INSERT ON wamn_run.{table} FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_disposition_append()"
            ),
            sql: format!(
                "CREATE TRIGGER {name} BEFORE INSERT ON wamn_run.{table} FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_effect_disposition_append();"
            ),
        });
    }
    for (table, name, event, function) in [
        (
            "authoring_test_sets",
            "authoring_test_sets_update_immutable",
            "UPDATE",
            "reject_immutable_authoring_test_set_change",
        ),
        (
            "authoring_test_sets",
            "authoring_test_sets_delete_immutable",
            "DELETE",
            "reject_immutable_authoring_test_set_change",
        ),
        (
            "authoring_report_reservations",
            "authoring_report_reservations_controlled_insert",
            "INSERT",
            "guard_authoring_report_write",
        ),
        (
            "authoring_report_reservations",
            "authoring_report_reservations_controlled_update",
            "UPDATE",
            "guard_authoring_report_write",
        ),
        (
            "authoring_report_reservations",
            "authoring_report_reservations_delete_immutable",
            "DELETE",
            "reject_immutable_authoring_report_change",
        ),
        (
            "authoring_suite_case_facts",
            "authoring_suite_case_facts_require_pending",
            "INSERT",
            "guard_authoring_report_write",
        ),
        (
            "authoring_suite_case_facts",
            "authoring_suite_case_facts_update_immutable",
            "UPDATE",
            "reject_immutable_authoring_report_change",
        ),
        (
            "authoring_suite_case_facts",
            "authoring_suite_case_facts_delete_immutable",
            "DELETE",
            "reject_immutable_authoring_report_change",
        ),
        (
            "authoring_suite_reports",
            "authoring_suite_reports_require_reservation",
            "INSERT",
            "guard_authoring_report_write",
        ),
        (
            "authoring_suite_reports",
            "authoring_suite_reports_update_immutable",
            "UPDATE",
            "reject_immutable_authoring_report_change",
        ),
        (
            "authoring_suite_reports",
            "authoring_suite_reports_delete_immutable",
            "DELETE",
            "reject_immutable_authoring_report_change",
        ),
    ] {
        specs.push(TriggerSpec {
            table: table.to_string(),
            name: name.to_string(),
            definition: format!(
                "CREATE TRIGGER {name} BEFORE {event} ON wamn_run.{table} \
                 FOR EACH ROW EXECUTE FUNCTION wamn_run.{function}()"
            ),
            sql: format!(
                "CREATE TRIGGER {name} BEFORE {event} ON wamn_run.{table} \
                 FOR EACH ROW EXECUTE FUNCTION wamn_run.{function}();"
            ),
        });
    }
    specs
}

const CURRENT_EFFECT_ATTEMPT_FK_NAME: &str = "node_runs_current_effect_attempt_fk";
const CURRENT_EFFECT_ATTEMPT_FK_DEF: &str = "FOREIGN KEY (tenant_id, current_effect_attempt_id, run_id, node_id, occurrence) REFERENCES wamn_run.effect_attempts(tenant_id, attempt_id, run_id, node_id, occurrence)";
const CURRENT_EFFECT_ATTEMPT_FK_SQL: &str = "ALTER TABLE wamn_run.node_runs \
    ADD CONSTRAINT node_runs_current_effect_attempt_fk \
    FOREIGN KEY (tenant_id, current_effect_attempt_id, run_id, node_id, occurrence) \
    REFERENCES wamn_run.effect_attempts \
        (tenant_id, attempt_id, run_id, node_id, occurrence)";

const PREDECESSOR_EFFECT_ATTEMPT_FK_NAME: &str = "effect_attempts_predecessor_fk";
const PREDECESSOR_EFFECT_ATTEMPT_FK_DEF: &str = "FOREIGN KEY (tenant_id, predecessor_attempt_id, run_id, node_id, occurrence) REFERENCES wamn_run.effect_attempts(tenant_id, attempt_id, run_id, node_id, occurrence)";
const PREDECESSOR_EFFECT_ATTEMPT_FK_SQL: &str = "ALTER TABLE wamn_run.effect_attempts \
    ADD CONSTRAINT effect_attempts_predecessor_fk \
    FOREIGN KEY (tenant_id, predecessor_attempt_id, run_id, node_id, occurrence) \
    REFERENCES wamn_run.effect_attempts \
        (tenant_id, attempt_id, run_id, node_id, occurrence)";
const EFFECT_DISPATCH_ATTEMPT_FK_NAME: &str = "effect_attempt_dispatches_attempt_fk";
const EFFECT_DISPATCH_ATTEMPT_FK_DEF: &str = "FOREIGN KEY (tenant_id, attempt_id, attempt_started_at) REFERENCES wamn_run.effect_attempts(tenant_id, attempt_id, attempt_started_at)";
const EFFECT_DISPATCH_ATTEMPT_FK_SQL: &str = "ALTER TABLE wamn_run.effect_attempt_dispatches \
     ADD CONSTRAINT effect_attempt_dispatches_attempt_fk \
     FOREIGN KEY (tenant_id, attempt_id, attempt_started_at) \
     REFERENCES wamn_run.effect_attempts \
         (tenant_id, attempt_id, attempt_started_at)";
const EFFECT_OUTCOME_DISPATCH_FK_NAME: &str = "effect_attempt_outcomes_dispatch_fk";
const EFFECT_OUTCOME_DISPATCH_FK_DEF: &str = "FOREIGN KEY (tenant_id, attempt_id, dispatched_at) REFERENCES wamn_run.effect_attempt_dispatches(tenant_id, attempt_id, dispatched_at)";
const EFFECT_OUTCOME_DISPATCH_FK_SQL: &str = "ALTER TABLE wamn_run.effect_attempt_outcomes \
     ADD CONSTRAINT effect_attempt_outcomes_dispatch_fk \
     FOREIGN KEY (tenant_id, attempt_id, dispatched_at) \
     REFERENCES wamn_run.effect_attempt_dispatches \
         (tenant_id, attempt_id, dispatched_at)";

const FLOW_AUTHOR_CHECK_NAME: &str = "flow_artifacts_verified_author_principal_check";
const FLOW_AUTHOR_CHECK_DEF: &str =
    "CHECK (verified_author_principal IS NULL OR verified_author_principal <> ''::text)";
const RELEASE_PUBLISHER_CHECK_NAME: &str = "release_manifests_verified_publisher_principal_check";
const RELEASE_PUBLISHER_CHECK_DEF: &str =
    "CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''::text)";

const LEGACY_EFFECT_AUTHORITY_COLUMNS: [&str; 11] = [
    "attempt",
    "selected_recovery_class",
    "recovery_class",
    "generation_fact_kind",
    "connection_generation",
    "credential_generation",
    "attempt_started_at",
    "attempt_dispatched_at",
    "attempt_deadline_at",
    "attempt_input_ref",
    "attempt_key",
];

/// Count legacy node projections that still need immutable effect authority.
/// Called only after the observer has established that every named column and
/// `current_effect_attempt_id` exists.
pub fn count_legacy_effect_attempt_rows_sql(schema: &BareSchemaName) -> String {
    format!(
        "SELECT count(*) FROM {}.node_runs \
          WHERE current_effect_attempt_id IS NULL \
            AND (status = 'started' OR selected_recovery_class IS NOT NULL \
                 OR recovery_class IS NOT NULL OR generation_fact_kind IS NOT NULL \
                 OR connection_generation IS NOT NULL OR credential_generation IS NOT NULL \
                 OR attempt_started_at IS NOT NULL OR attempt_dispatched_at IS NOT NULL \
                 OR attempt_deadline_at IS NOT NULL OR attempt_input_ref IS NOT NULL \
                 OR attempt_key IS NOT NULL)",
        schema.quoted(),
    )
}

/// One additive, idempotent cutover from legacy mutable attempt columns to the
/// immutable attempt/dispatch/outcome ledgers. The planner schedules it after
/// all tables and the nullable current pointer exist, but before the composite
/// pointer FK is installed.
fn legacy_effect_attempt_backfill_sql(schema: &BareSchemaName) -> String {
    let schema = schema.quoted();
    format!(
        r#"DO $backfill$
DECLARE
    backfilled_attempts bigint;
    backfilled_dispatches bigint;
    backfilled_outcomes bigint;
BEGIN
    LOCK TABLE {schema}.node_runs IN SHARE ROW EXCLUSIVE MODE;

    IF EXISTS (
        SELECT 1 FROM {schema}.node_runs AS n
         WHERE n.current_effect_attempt_id IS NULL
           AND (n.status = 'started' OR n.selected_recovery_class IS NOT NULL
                OR n.recovery_class IS NOT NULL OR n.generation_fact_kind IS NOT NULL
                OR n.connection_generation IS NOT NULL OR n.credential_generation IS NOT NULL
                OR n.attempt_started_at IS NOT NULL OR n.attempt_dispatched_at IS NOT NULL
                OR n.attempt_deadline_at IS NOT NULL OR n.attempt_input_ref IS NOT NULL
                OR n.attempt_key IS NOT NULL)
           AND (
               n.attempt >= 0 AND n.occurrence >= 0 AND n.seq >= 0
               AND n.selected_recovery_class IN
                   ('replay','idempotent-with-key','never-replay')
               AND n.recovery_class = n.selected_recovery_class
               AND n.generation_fact_kind IN ('not-required','attested')
               AND n.attempt_started_at IS NOT NULL
               AND n.attempt_deadline_at IS NOT NULL
               AND NULLIF(n.attempt_input_ref, '') IS NOT NULL
               AND n.attempt_started_at <= n.attempt_deadline_at
               AND (n.attempt_dispatched_at IS NULL
                    OR n.attempt_started_at <= n.attempt_dispatched_at)
               AND (n.status NOT IN ('success','error')
                    OR (n.attempt_dispatched_at IS NOT NULL
                        AND n.ended_at IS NOT NULL
                        AND n.attempt_dispatched_at <= n.ended_at))
               AND (n.recovery_class <> 'idempotent-with-key'
                    OR NULLIF(n.attempt_key, '') IS NOT NULL)
               AND ((n.generation_fact_kind = 'not-required'
                     AND n.connection_generation IS NULL
                     AND n.credential_generation IS NULL)
                    OR (n.generation_fact_kind = 'attested'
                        AND NULLIF(n.connection_generation, '') IS NOT NULL
                        AND NULLIF(n.credential_generation, '') IS NOT NULL))
           ) IS NOT TRUE
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'legacy-effect-attempt-incomplete';
    END IF;

    IF EXISTS (
        SELECT 1
          FROM {schema}.node_runs AS n
          JOIN {schema}.runs AS r
            ON r.tenant_id = n.tenant_id AND r.run_id = n.run_id
          LEFT JOIN catalog.flow_artifacts AS artifact
            ON artifact.tenant_id = r.tenant_id
           AND artifact.flow_id = r.flow_id
           AND artifact.flow_version = r.flow_version
           AND artifact.artifact_hash =
               r.invocation_context #>> '{{principal,artifact-digest}}'
          LEFT JOIN LATERAL jsonb_array_elements(artifact.graph_json -> 'nodes') AS node(value)
            ON node.value ->> 'id' = n.node_id
         WHERE n.current_effect_attempt_id IS NULL
           AND n.generation_fact_kind = 'attested'
           AND NULLIF(node.value ->> 'connection', '') IS NULL
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'legacy-effect-attempt-connection-unresolved';
    END IF;

WITH candidates AS MATERIALIZED (
    SELECT n.tenant_id, gen_random_uuid() AS attempt_id,
           n.run_id, n.node_id, n.occurrence, n.seq,
           n.attempt AS attempt_index,
           n.selected_recovery_class, n.recovery_class,
           n.generation_fact_kind,
           CASE WHEN n.generation_fact_kind = 'attested'
                THEN node.value ->> 'connection' END AS connection_name,
           n.connection_generation, n.credential_generation,
           n.attempt_started_at, n.attempt_dispatched_at,
           n.attempt_deadline_at, n.attempt_input_ref, n.attempt_key,
           n.status, n.ended_at
      FROM {schema}.node_runs AS n
      JOIN {schema}.runs AS r
        ON r.tenant_id = n.tenant_id AND r.run_id = n.run_id
      LEFT JOIN catalog.flow_artifacts AS artifact
        ON artifact.tenant_id = r.tenant_id
       AND artifact.flow_id = r.flow_id
       AND artifact.flow_version = r.flow_version
       AND artifact.artifact_hash =
           r.invocation_context #>> '{{principal,artifact-digest}}'
      LEFT JOIN LATERAL jsonb_array_elements(artifact.graph_json -> 'nodes') AS node(value)
        ON node.value ->> 'id' = n.node_id
     WHERE n.current_effect_attempt_id IS NULL
       AND n.selected_recovery_class IS NOT NULL
),
attempts AS (
    INSERT INTO {schema}.effect_attempts
           (tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index,
            predecessor_attempt_id,legacy_imported,
            selected_recovery_class,recovery_class,
            generation_fact_kind,connection_name,connection_generation,
            credential_generation,verified_author_principal,
            verified_publisher_principal,attempt_started_at,attempt_deadline_at,
            attempt_input_ref,attempt_key)
    SELECT tenant_id,attempt_id,run_id,node_id,occurrence,seq,attempt_index,
           NULL,true,selected_recovery_class,recovery_class,generation_fact_kind,
           connection_name,connection_generation,credential_generation,
           NULL,NULL,attempt_started_at,attempt_deadline_at,attempt_input_ref,attempt_key
      FROM candidates
    RETURNING tenant_id,attempt_id
),
dispatches AS (
    INSERT INTO {schema}.effect_attempt_dispatches
           (tenant_id,attempt_id,attempt_started_at,dispatched_at)
    SELECT c.tenant_id,c.attempt_id,c.attempt_started_at,c.attempt_dispatched_at
      FROM candidates AS c
      JOIN attempts AS a
        ON a.tenant_id = c.tenant_id AND a.attempt_id = c.attempt_id
     WHERE c.attempt_dispatched_at IS NOT NULL
    RETURNING tenant_id,attempt_id,dispatched_at
),
outcomes AS (
    INSERT INTO {schema}.effect_attempt_outcomes
           (tenant_id,attempt_id,dispatched_at,outcome_status,recorded_at)
    SELECT c.tenant_id,c.attempt_id,d.dispatched_at,c.status,
           c.ended_at
      FROM candidates AS c
      JOIN dispatches AS d
        ON d.tenant_id = c.tenant_id AND d.attempt_id = c.attempt_id
     WHERE c.status IN ('success','error')
    RETURNING tenant_id,attempt_id
),
updated AS (
    UPDATE {schema}.node_runs AS n
       SET current_effect_attempt_id = c.attempt_id
      FROM candidates AS c
      JOIN attempts AS a
        ON a.tenant_id = c.tenant_id AND a.attempt_id = c.attempt_id
     WHERE n.tenant_id = c.tenant_id AND n.run_id = c.run_id
       AND n.node_id = c.node_id AND n.occurrence = c.occurrence
    RETURNING n.tenant_id,n.run_id
)
SELECT (SELECT count(*) FROM updated),
       (SELECT count(*) FROM dispatches),
       (SELECT count(*) FROM outcomes)
  INTO backfilled_attempts, backfilled_dispatches, backfilled_outcomes;

    IF EXISTS (
        SELECT 1 FROM {schema}.node_runs AS n
         WHERE n.current_effect_attempt_id IS NULL
           AND (n.status = 'started' OR n.selected_recovery_class IS NOT NULL
                OR n.recovery_class IS NOT NULL OR n.generation_fact_kind IS NOT NULL
                OR n.connection_generation IS NOT NULL OR n.credential_generation IS NOT NULL
                OR n.attempt_started_at IS NOT NULL OR n.attempt_dispatched_at IS NOT NULL
                OR n.attempt_deadline_at IS NOT NULL OR n.attempt_input_ref IS NOT NULL
                OR n.attempt_key IS NOT NULL)
    ) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'legacy-effect-attempt-backfill-incomplete';
    END IF;
END
$backfill$;"#,
    )
}

fn disposition_provenance_migration_sql() -> &'static str {
    let start = CATALOG_SCHEMA_SQL
        .find("-- BEGIN DISPOSITION PROVENANCE STORAGE MIGRATION")
        .expect("disposition provenance migration start");
    let end = CATALOG_SCHEMA_SQL
        .find("-- END DISPOSITION PROVENANCE STORAGE MIGRATION")
        .expect("disposition provenance migration end");
    &CATALOG_SCHEMA_SQL[start..end]
}

/// The run-plane record files in APPLY ORDER: run-state first (schema header +
/// `runs`, which everything FKs), then the flow registry, then the 11.2 flow
/// test-suite tables (FK to `flows`, so AFTER it), then the queue.
const RUN_PLANE_FILES: [&str; 4] = [RUN_STATE_SQL, FLOWS_SQL, FLOW_TESTS_SQL, RUN_QUEUE_SQL];

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
        table: "catalogs",
        app: &["SELECT", "INSERT", "UPDATE", "DELETE"],
        author: &[],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "flow_artifacts",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "release_manifests",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "release_flows",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "catalog_heads",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "flow_drafts",
        app: &[],
        author: &["SELECT", "INSERT", "UPDATE"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "validated_flow_drafts",
        app: &["SELECT"],
        author: &["SELECT", "INSERT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "draft_safe_connection_grants",
        app: &["SELECT"],
        author: &["SELECT", "INSERT", "UPDATE"],
    },
    // The command ledger is append-only management-plane evidence: the author
    // adds and reads rows, the guest runtime credential never sees it, and
    // nobody gets UPDATE or DELETE.
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "authoring_command_audit",
        app: &[],
        author: &["SELECT", "INSERT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_requirements",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_instances",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_generations",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::Catalog,
        table: "connection_bindings",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "runs",
        app: &["SELECT", "INSERT", "UPDATE", "DELETE"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "test_suites",
        app: &["SELECT", "INSERT", "UPDATE", "DELETE"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "test_cases",
        app: &["SELECT", "INSERT", "UPDATE", "DELETE"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "authoring_test_sets",
        app: &[],
        author: &["SELECT", "INSERT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "authoring_report_reservations",
        app: &[],
        author: &["SELECT", "INSERT", "UPDATE"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "authoring_suite_case_facts",
        app: &[],
        author: &["SELECT", "INSERT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "authoring_suite_reports",
        app: &[],
        author: &["SELECT", "INSERT"],
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

/// The outbox-era tables the l5i9.19 teardown retired. A pre-teardown schema
/// (or one restored from a pre-teardown snapshot) still carries them.
pub const LEGACY_OUTBOX_TABLES: [&str; 2] = ["outbox", "evt_shadow"];

/// The constant trigger AND function name the retired wamn-schema-compiler outbox emission
/// used (`CREATE OR REPLACE TRIGGER wamn_outbox_event … EXECUTE FUNCTION
/// wamn_outbox_event()`, one trigger per entity table, the function unqualified
/// so it landed in the apply-time schema).
pub const OUTBOX_TRIGGER_NAME: &str = "wamn_outbox_event";

/// The non-login database authority held only by trusted scenario-host
/// credentials. The guest-visible `wamn_app` role is never a member.
pub const SCENARIO_AUTHOR_ROLE: &str = "wamn_scenario_author";

/// Security attributes observed for the host-only scenario author role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScenarioAuthorRoleObservation {
    pub can_login: bool,
    pub is_superuser: bool,
    pub can_create_database: bool,
    pub can_create_role: bool,
    pub inherits_roles: bool,
    pub can_replicate: bool,
    pub bypasses_rls: bool,
}

impl ScenarioAuthorRoleObservation {
    fn is_host_only(self) -> bool {
        !self.can_login
            && !self.is_superuser
            && !self.can_create_database
            && !self.can_create_role
            && !self.inherits_roles
            && !self.can_replicate
            && !self.bypasses_rls
    }
}

/// A validated project schema name usable in both quoted SQL and bare DDL rewrites.
///
/// PostgreSQL's identifier representation, byte limit, and quoting live in
/// [`Identifier`]. This wrapper adds only the lowercase unquoted grammar required
/// by [`rewrite_schema`], which substitutes the name into canonical deploy SQL as
/// a bare identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BareSchemaName(Identifier);

/// Why a value cannot be used as a bare project schema name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidBareSchemaName {
    kind: InvalidBareSchemaNameKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InvalidBareSchemaNameKind {
    PostgreSql(InvalidIdentifier),
    BareSyntax,
}

impl InvalidBareSchemaName {
    /// The violated PostgreSQL or bare-schema invariant.
    pub fn reason(&self) -> &str {
        match &self.kind {
            InvalidBareSchemaNameKind::PostgreSql(error) => error.reason(),
            InvalidBareSchemaNameKind::BareSyntax => {
                "schema name must match the lowercase bare identifier syntax [a-z_][a-z0-9_]*"
            }
        }
    }
}

impl fmt::Display for InvalidBareSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for InvalidBareSchemaName {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            InvalidBareSchemaNameKind::PostgreSql(error) => Some(error),
            InvalidBareSchemaNameKind::BareSyntax => None,
        }
    }
}

impl From<InvalidIdentifier> for InvalidBareSchemaName {
    fn from(error: InvalidIdentifier) -> Self {
        Self {
            kind: InvalidBareSchemaNameKind::PostgreSql(error),
        }
    }
}

impl BareSchemaName {
    /// Validate a schema name before it reaches generated SQL or an admin effect.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidBareSchemaName> {
        let identifier = Identifier::new(value)?;
        let mut bytes = identifier.as_str().bytes();
        if !matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(InvalidBareSchemaName {
                kind: InvalidBareSchemaNameKind::BareSyntax,
            });
        }
        Ok(Self(identifier))
    }

    /// The validated identifier in the bare representation used by deploy SQL.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The validated identifier quoted by the canonical pg-core implementation.
    pub fn quoted(&self) -> String {
        self.0.quoted()
    }
}

impl fmt::Display for BareSchemaName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What the driver observed live, scoped to ONE project-env schema (plus the
/// per-database `catalog` metadata schema). Everything here is a read — the
/// pure planner turns it into the action list.
#[derive(Debug, Clone, Default)]
pub struct RunPlaneObservation {
    /// Host-only scenario-author role attributes, or absent when the cluster
    /// has not yet provisioned the role.
    pub scenario_author_role: Option<ScenarioAuthorRoleObservation>,
    /// Whether guest-visible `wamn_app` inherits the host-only author role.
    pub app_is_scenario_author_member: bool,
    /// Direct table grants for the authoring-state security surface, keyed by
    /// `(schema, table, grantee)` and containing uppercase privilege names.
    pub authoring_table_privileges: BTreeMap<(String, String, String), BTreeSet<String>>,
    /// Effective table privileges (direct, inherited, or ownership-derived)
    /// for the two security-boundary roles on every managed authoring table.
    pub authoring_effective_table_privileges: BTreeMap<(String, String, String), BTreeSet<String>>,
    /// Effective mutation/reference authority on any column. PostgreSQL keeps
    /// column ACLs separate from table ACLs, so a table-level REVOKE alone is
    /// not a sufficient boundary proof.
    pub authoring_effective_column_privileges: BTreeMap<(String, String, String), BTreeSet<String>>,
    /// Owner of every managed authoring table, keyed by `(schema, table)`.
    /// Ownership is authority to restore revoked ACLs and therefore must stay
    /// outside both the guest-visible and host-only author roles.
    pub authoring_table_owners: BTreeMap<(String, String), String>,
    /// Schemas on which the host-only author role effectively has USAGE.
    pub scenario_author_schema_usage: BTreeSet<String>,
    /// Whether the host-only author may call the narrow SECURITY DEFINER
    /// catalog-head lock without gaining direct UPDATE/row-lock authority.
    pub scenario_author_can_lock_catalog_head: bool,
    /// EVERY ordinary table in the target schema → its live column names.
    /// Includes entity/floor tables (ignored by the planner) and any legacy
    /// outbox-era tables (planned for teardown).
    pub tables: BTreeMap<String, BTreeSet<String>>,
    /// EVERY index in the target schema → its live `pg_indexes.indexdef`.
    pub indexes: BTreeMap<String, String>,
    /// Tables in the target schema carrying the legacy `wamn_outbox_event`
    /// trigger.
    pub outbox_trigger_tables: Vec<String>,
    /// Whether the legacy `wamn_outbox_event()` function exists in the target
    /// schema.
    pub outbox_function_present: bool,
    /// Whether the per-database `catalog` metadata schema exists.
    pub catalog_schema_present: bool,
    /// Tables present in the `catalog` schema (empty when the schema is absent).
    pub catalog_tables: BTreeSet<String>,
    /// Catalog table columns used by additive cross-plane migrations.
    pub catalog_columns: BTreeMap<String, BTreeSet<String>>,
    /// Catalog CHECKs owned by additive cross-plane migrations.
    pub catalog_checks: BTreeMap<(String, String), String>,
    /// Legacy node projections still carrying effect authority without an
    /// immutable current-attempt pointer.
    pub legacy_effect_attempt_rows: i64,
    /// Rows in `catalog.event_registrations` still carrying the legacy `state`
    /// key (0 when the table is absent — nothing to strip).
    pub stale_registration_state_rows: i64,
    /// Every CHECK constraint on a record table, keyed by `(table, name)`, with
    /// PostgreSQL's canonical `pg_get_constraintdef(..., true)` definition.
    pub checks: BTreeMap<(String, String), String>,
    /// Managed foreign keys keyed by `(table, name)`, with PostgreSQL's
    /// canonical `pg_get_constraintdef(..., true)` definition. The planner
    /// repairs only explicitly named record FKs; unrelated live FKs are inert.
    pub foreign_keys: BTreeMap<(String, String), String>,
    /// Every non-internal trigger on a record table, keyed by `(table, name)`,
    /// with PostgreSQL's canonical `pg_get_triggerdef(..., true)` definition.
    pub triggers: BTreeMap<(String, String), String>,
    /// Canonical `pg_get_functiondef` output for the run-state helper
    /// functions, keyed by function name.
    pub helper_functions: BTreeMap<String, String>,
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
    /// Backfill immutable attempt/dispatch/outcome facts from the legacy
    /// mutable node projection before readers switch authority.
    BackfillEffectAttempts,
    /// Drop/re-add a drifted record CHECK, or add it when absent.
    RepairConstraint,
    /// Drop/re-add a missing or drifted named record foreign key.
    RepairForeignKey,
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
    /// Apply the whole `catalog-schema.sql` (the `catalog` schema is absent).
    EnsureCatalogSchema,
    /// Create a missing `catalog` table from its record section.
    CreateCatalogTable,
    /// Add the nullable verified author/publisher provenance columns required
    /// before immutable attempt writers activate.
    EnsureCatalogProvenance,
    /// Converge authoring-state schema/table grants and remove guest write
    /// authority or membership in the host-only role.
    RepairAuthoringPrivilege,
    /// Strip the legacy `state` key from stored registrations.
    StripRegistrationState,
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
/// target (reported, never executed) and live columns the record does not know
/// (SURFACED — the plan never drops them). Idempotent: planning against the
/// post-apply state yields no actions.
#[derive(Debug, Clone, Default)]
pub struct RunPlanePlan {
    pub actions: Vec<RunPlaneAction>,
    /// Run-plane record tables present live with full column + index parity.
    pub at_target: Vec<String>,
    /// `(table, column)` live columns not in the record — extra, untouched.
    pub extra_columns: Vec<(String, String)>,
}

impl RunPlanePlan {
    /// Whether there is anything to apply (a no-op reconcile is the expected
    /// steady state and worth reporting as such).
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }
}

/// Reconcile one project-env's run-plane schema (+ the per-database `catalog`
/// metadata schema) against the schema of record. Pure: `obs` is what the
/// driver read; the returned plan is what it should execute, in order.
pub fn plan_run_plane(schema: &BareSchemaName, obs: &RunPlaneObservation) -> RunPlanePlan {
    let mut plan = RunPlanePlan::default();

    // The standalone record files grant privileged authoring writes to this
    // role, so it must exist (and remain non-login/non-bypass) before any
    // missing schema/table section executes.
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

    // 1. Missing run-plane tables → EnsureSchema once, then per-table sections
    //    in file order (FKs resolve: runs before node_runs/flows/queue).
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
    for spec in HELPER_SPECS {
        if obs
            .helper_functions
            .get(spec.name)
            .is_none_or(|definition| {
                normalize_observed_schema(definition, schema) != spec.definition
            })
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairHelperFunction,
                target: spec.name.to_string(),
                sql: rewrite_schema(spec.sql, schema),
            });
        }
    }
    if !obs.scenario_author_can_lock_catalog_head {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairAuthoringPrivilege,
            target: format!("{}.lock_catalog_head", schema.as_str()),
            sql: format!(
                "GRANT EXECUTE ON FUNCTION {}.lock_catalog_head(text, text, text) \
                 TO {SCENARIO_AUTHOR_ROLE}",
                schema.quoted()
            ),
        });
    }
    plan.actions.extend(creates);

    // Catalog storage must converge before any legacy effect-attempt backfill:
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
        let flow_needs_provenance = obs.catalog_tables.contains("flow_artifacts")
            && !obs
                .catalog_columns
                .get("flow_artifacts")
                .is_some_and(|columns| columns.contains("verified_author_principal"));
        let release_needs_provenance = obs.catalog_tables.contains("release_manifests")
            && !obs
                .catalog_columns
                .get("release_manifests")
                .is_some_and(|columns| columns.contains("verified_publisher_principal"));
        let flow_check_needs_repair = obs.catalog_tables.contains("flow_artifacts")
            && obs
                .catalog_checks
                .get(&(
                    "flow_artifacts".to_string(),
                    FLOW_AUTHOR_CHECK_NAME.to_string(),
                ))
                .is_none_or(|definition| definition != FLOW_AUTHOR_CHECK_DEF);
        let release_check_needs_repair = obs.catalog_tables.contains("release_manifests")
            && obs
                .catalog_checks
                .get(&(
                    "release_manifests".to_string(),
                    RELEASE_PUBLISHER_CHECK_NAME.to_string(),
                ))
                .is_none_or(|definition| definition != RELEASE_PUBLISHER_CHECK_DEF);
        if flow_needs_provenance
            || release_needs_provenance
            || flow_check_needs_repair
            || release_check_needs_repair
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::EnsureCatalogProvenance,
                target: "catalog.disposition-provenance".to_string(),
                sql: disposition_provenance_migration_sql().to_string(),
            });
        }
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
        let (schema_name, present) = match spec.schema {
            AuthoringTableSchema::Catalog => ("catalog", obs.catalog_tables.contains(spec.table)),
            AuthoringTableSchema::RunPlane => {
                (schema.as_str(), obs.tables.contains_key(spec.table))
            }
        };
        if !present {
            continue;
        }
        let expected_for = |grantee: &str| -> BTreeSet<String> {
            let privileges = match grantee {
                "wamn_app" => spec.app,
                SCENARIO_AUTHOR_ROLE => spec.author,
                "PUBLIC" => &[],
                _ => unreachable!("closed authoring grantee set"),
            };
            privileges
                .iter()
                .map(|value| (*value).to_string())
                .collect()
        };
        let direct_drifted = ["PUBLIC", "wamn_app", SCENARIO_AUTHOR_ROLE]
            .into_iter()
            .any(|grantee| {
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
        let effective_drifted = ["wamn_app", SCENARIO_AUTHOR_ROLE]
            .into_iter()
            .any(|grantee| {
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
        let effective_column_drifted =
            ["wamn_app", SCENARIO_AUTHOR_ROLE]
                .into_iter()
                .any(|grantee| {
                    let expected_columns: BTreeSet<String> = expected_for(grantee)
                        .into_iter()
                        .filter(|privilege| {
                            ["SELECT", "INSERT", "UPDATE", "REFERENCES"]
                                .contains(&privilege.as_str())
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
            .is_some_and(|owner| owner == "wamn_app" || owner == SCENARIO_AUTHOR_ROLE);
        if !direct_drifted && !effective_drifted && !effective_column_drifted && !boundary_owned {
            continue;
        }

        let qualified = format!("{}.{}", quote_ident(schema_name), quote_ident(spec.table));
        let mut sql = String::new();
        if direct_drifted {
            sql = format!(
                "REVOKE ALL PRIVILEGES ON TABLE {qualified} FROM PUBLIC; \
                 REVOKE ALL PRIVILEGES ON TABLE {qualified} FROM wamn_app; \
                 REVOKE ALL PRIVILEGES ON TABLE {qualified} FROM {SCENARIO_AUTHOR_ROLE}"
            );
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
        for (grantee, expected) in [("wamn_app", spec.app), (SCENARIO_AUTHOR_ROLE, spec.author)] {
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
             IN ('wamn_app', '{SCENARIO_AUTHOR_ROLE}')"
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
    //    live table lacks (record order); surface live extras, never drop them.
    for file in RUN_PLANE_FILES {
        for table in record_tables(file, "wamn_run") {
            let Some(live_cols) = obs.tables.get(&table) else {
                continue;
            };
            let record_cols = record_columns(file, "wamn_run", &table);
            for (col, def) in &record_cols {
                if !live_cols.contains(col) {
                    plan.actions.push(RunPlaneAction {
                        kind: RunPlaneActionKind::AddColumn,
                        target: format!("{table}.{col}"),
                        sql: format!(
                            "ALTER TABLE {}.{} ADD COLUMN {def}",
                            schema.quoted(),
                            quote_ident(&table),
                        ),
                    });
                }
            }
            let known: BTreeSet<&str> = record_cols.iter().map(|(c, _)| c.as_str()).collect();
            for col in live_cols {
                if !known.contains(col.as_str()) {
                    plan.extra_columns.push((table.clone(), col.clone()));
                }
            }
        }
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
    for (table, name) in obs.checks.keys() {
        if obs.tables.contains_key(table)
            && record_table_names().contains(table.as_str())
            && !expected_checks.contains(&(table.as_str(), name.as_str()))
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

    // 2c. Backfill the immutable attempt boundary before the pointer FK makes
    // it authoritative. A present legacy table with a newly added pointer is
    // always scheduled once; subsequent observations count only rows still
    // carrying legacy effect authority without a pointer.
    if let Some(columns) = obs.tables.get("node_runs") {
        let legacy_authority_present = LEGACY_EFFECT_AUTHORITY_COLUMNS
            .iter()
            .all(|column| columns.contains(*column));
        let pointer_missing = !columns.contains("current_effect_attempt_id");
        if pointer_missing || (legacy_authority_present && obs.legacy_effect_attempt_rows > 0) {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::BackfillEffectAttempts,
                target: "node_runs.current_effect_attempt_id".to_string(),
                sql: legacy_effect_attempt_backfill_sql(schema),
            });
        }
    }

    // 2d. The composite current-attempt pointer is added only after missing
    // tables and additive columns exist. This avoids the legacy-upgrade bug in
    // which creating `effect_attempts` tried to reference a not-yet-added
    // `node_runs.current_effect_attempt_id`.
    let fk_key = (
        "node_runs".to_string(),
        CURRENT_EFFECT_ATTEMPT_FK_NAME.to_string(),
    );
    if obs.foreign_keys.get(&fk_key).is_none_or(|definition| {
        normalize_observed_schema(definition, schema) != CURRENT_EFFECT_ATTEMPT_FK_DEF
    }) {
        let drop = if obs.foreign_keys.contains_key(&fk_key) {
            format!(
                "ALTER TABLE {}.{} DROP CONSTRAINT {}; ",
                schema.quoted(),
                quote_ident("node_runs"),
                quote_ident(CURRENT_EFFECT_ATTEMPT_FK_NAME),
            )
        } else {
            String::new()
        };
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairForeignKey,
            target: format!("node_runs.{CURRENT_EFFECT_ATTEMPT_FK_NAME}"),
            sql: format!(
                "{drop}{}",
                rewrite_schema(CURRENT_EFFECT_ATTEMPT_FK_SQL, schema)
            ),
        });
    }

    // Existing effect ledgers also converge their typed predecessor and
    // temporal boundary FKs. A missing table's canonical CREATE section
    // already carries these, so repair only observed live tables.
    for (table, name, definition, sql) in [
        (
            "effect_attempts",
            PREDECESSOR_EFFECT_ATTEMPT_FK_NAME,
            PREDECESSOR_EFFECT_ATTEMPT_FK_DEF,
            PREDECESSOR_EFFECT_ATTEMPT_FK_SQL,
        ),
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

    // 2e. User triggers are explicit record objects. Missing-table sections
    // carry their own triggers; present tables are repaired exactly, and the
    // immutable-ledger triggers are never mistaken for extras.
    let trigger_specs = trigger_specs();
    let expected_triggers: BTreeSet<(&str, &str)> = trigger_specs
        .iter()
        .map(|spec| (spec.table.as_str(), spec.name.as_str()))
        .collect();
    for spec in &trigger_specs {
        if !obs.tables.contains_key(&spec.table) {
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
    if obs.stale_registration_state_rows > 0 {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::StripRegistrationState,
            target: format!("{} registrations", obs.stale_registration_state_rows),
            sql: strip_registration_state_sql().to_string(),
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

/// A present index is STALE when the record definition names a record column of
/// its table that the live definition does not (word-boundary token match, so
/// `run_id` never matches inside `root_run_id`). This is deliberately the
/// narrow, real drift class — the pre-E4 `run_queue_claimable` without
/// `stream_seq` and the pre-hardening disposition history without
/// `append_ordinal` — not a general definition differ.
fn index_definition_stale(file: &str, table: &str, record_stmt: &str, live_def: &str) -> bool {
    // Unit observations intentionally use the schema-of-record statement as
    // the live definition. PostgreSQL's `pg_indexes` rendering is checked
    // below; the record itself is already canonical by construction.
    if live_def == record_stmt {
        return false;
    }

    let live = live_def.split_whitespace().collect::<Vec<_>>().join(" ");
    let btree_suffix = live.split_once(" USING btree ").map(|(_, suffix)| suffix);
    let security_index_is_stale = if record_stmt.contains("effect_dispositions_append_order") {
        !live.starts_with("CREATE UNIQUE INDEX ") || btree_suffix != Some("(append_ordinal)")
    } else if record_stmt.contains("effect_dispositions_request_ordinal") {
        !live.starts_with("CREATE UNIQUE INDEX ")
            || btree_suffix != Some("(tenant_id, request_id, selection_ordinal)")
    } else if record_stmt.contains("effect_dispositions_one_resolution") {
        !live.starts_with("CREATE UNIQUE INDEX ")
            || btree_suffix != Some("(tenant_id, attempt_id) WHERE (action = 'resolve'::text)")
    } else {
        false
    };
    if security_index_is_stale {
        return true;
    }

    let record_tokens = ident_tokens(record_stmt);
    let live_tokens = ident_tokens(&live);
    record_columns(file, "wamn_run", table)
        .iter()
        .any(|(col, _)| record_tokens.contains(col.as_str()) && !live_tokens.contains(col.as_str()))
}

/// Identifier-ish tokens of a SQL string: maximal `[A-Za-z0-9_]+` runs.
fn ident_tokens(sql: &str) -> BTreeSet<&str> {
    sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .collect()
}

fn quote_ident(s: &str) -> String {
    wamn_schema_compiler::sql::quote_ident(s)
}

fn record_table_names() -> BTreeSet<String> {
    RUN_PLANE_FILES
        .iter()
        .flat_map(|file| record_tables(file, "wamn_run"))
        .collect()
}

fn normalize_observed_schema(definition: &str, schema: &BareSchemaName) -> String {
    definition
        .replace(
            &format!(
                "SET search_path TO 'pg_catalog', '{}', 'pg_temp'",
                schema.as_str()
            ),
            "SET search_path TO 'pg_catalog', 'wamn_run', 'pg_temp'",
        )
        .replace(&format!("{}.", schema.as_str()), "wamn_run.")
}

/// The legacy registration `state`-key strip (the l5i9.19 teardown runbook): a
/// stored document still carrying `state` fails parse post-teardown, so the
/// materializer HOLDs its flow (delayed-never-lost) until the key is removed.
/// Runs as the superuser (RLS bypassed — the key is legacy across all tenants).
pub fn strip_registration_state_sql() -> &'static str {
    "UPDATE catalog.event_registrations SET registration = registration - 'state' \
     WHERE registration ? 'state'"
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

/// Security attributes of the host-only scenario-author role (zero or one row).
pub fn select_scenario_author_role_sql() -> &'static str {
    "SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolinherit, \
            rolreplication, rolbypassrls \
       FROM pg_catalog.pg_roles WHERE rolname = 'wamn_scenario_author'"
}

/// Whether guest-visible `wamn_app` inherits the host-only author role.
pub fn select_app_scenario_author_membership_sql() -> &'static str {
    "SELECT COALESCE(pg_catalog.pg_has_role( \
        (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app'), \
        (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'wamn_scenario_author'), \
        'MEMBER'), false)"
}

/// Direct grants on the managed authoring/release boundary. `$1` is the project
/// run-plane schema; PUBLIC is included so stale ambient writes are visible.
pub fn select_authoring_table_privileges_sql() -> &'static str {
    "SELECT table_schema, table_name, grantee, privilege_type \
       FROM information_schema.table_privileges \
      WHERE grantee IN ('PUBLIC', 'wamn_app', 'wamn_scenario_author') \
        AND ((table_schema = 'catalog' AND table_name IN \
              ('catalogs', 'flow_artifacts', 'release_manifests', \
               'release_flows', 'catalog_heads', \
               'flow_drafts', 'validated_flow_drafts', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings', \
               'draft_safe_connection_grants', 'authoring_command_audit')) \
          OR (table_schema = $1 AND table_name IN \
              ('runs', 'test_suites', 'test_cases', \
               'authoring_test_sets', \
               'authoring_report_reservations', \
               'authoring_suite_case_facts', \
               'authoring_suite_reports'))) \
      ORDER BY table_schema, table_name, grantee, privilege_type"
}

/// Effective privileges for both boundary roles, including authority obtained
/// through inherited groups or table ownership. Direct ACL convergence cannot
/// safely revoke an arbitrary group or reassign an owner, so the planner uses
/// this observation to install a post-repair refusal instead of false-cleaning.
pub fn select_authoring_effective_table_privileges_sql() -> &'static str {
    "SELECT namespace.nspname, relation.relname, actor.rolname, privilege.name \
       FROM pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('DELETE'::text), ('TRUNCATE'::text), \
                          ('REFERENCES'::text), ('TRIGGER'::text)) \
                  AS privilege(name) \
       JOIN pg_catalog.pg_class AS relation ON relation.relkind = 'r' \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
      WHERE actor.rolname IN ('wamn_app', 'wamn_scenario_author') \
        AND ((namespace.nspname = 'catalog' AND relation.relname IN \
              ('catalogs', 'flow_artifacts', 'release_manifests', \
               'release_flows', 'catalog_heads', \
               'flow_drafts', 'validated_flow_drafts', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings', \
               'draft_safe_connection_grants', 'authoring_command_audit')) \
          OR (namespace.nspname = $1 AND relation.relname IN \
              ('runs', 'test_suites', 'test_cases', \
               'authoring_test_sets', \
               'authoring_report_reservations', \
               'authoring_suite_case_facts', 'authoring_suite_reports'))) \
        AND pg_catalog.has_table_privilege( \
              actor.oid, relation.oid, privilege.name) \
      ORDER BY namespace.nspname, relation.relname, actor.rolname, privilege.name"
}

/// Effective per-column read/mutation/reference authority. Table-level
/// privileges also appear here and are expected when present in the table
/// spec; the drift of interest is a surviving column grant not represented by
/// that spec.
pub fn select_authoring_effective_column_privileges_sql() -> &'static str {
    "SELECT namespace.nspname, relation.relname, actor.rolname, privilege.name \
       FROM pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('REFERENCES'::text)) AS privilege(name) \
       JOIN pg_catalog.pg_class AS relation ON relation.relkind = 'r' \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
      WHERE actor.rolname IN ('wamn_app', 'wamn_scenario_author') \
        AND ((namespace.nspname = 'catalog' AND relation.relname IN \
              ('catalogs', 'flow_artifacts', 'release_manifests', \
               'release_flows', 'catalog_heads', \
               'flow_drafts', 'validated_flow_drafts', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings', \
               'draft_safe_connection_grants', 'authoring_command_audit')) \
          OR (namespace.nspname = $1 AND relation.relname IN \
              ('runs', 'test_suites', 'test_cases', \
               'authoring_test_sets', \
               'authoring_report_reservations', \
               'authoring_suite_case_facts', 'authoring_suite_reports'))) \
        AND pg_catalog.has_any_column_privilege( \
              actor.oid, relation.oid, privilege.name) \
      ORDER BY namespace.nspname, relation.relname, actor.rolname, privilege.name"
}

/// Owners of the managed authoring/release boundary. PostgreSQL ownership is
/// stronger than an ACL and can remain after every direct privilege is
/// revoked, so it is observed independently of `has_table_privilege`.
pub fn select_authoring_table_owners_sql() -> &'static str {
    "SELECT namespace.nspname, relation.relname, owner.rolname \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner \
      WHERE relation.relkind = 'r' \
        AND ((namespace.nspname = 'catalog' AND relation.relname IN \
              ('catalogs', 'flow_artifacts', 'release_manifests', \
               'release_flows', 'catalog_heads', \
               'flow_drafts', 'validated_flow_drafts', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings', \
               'draft_safe_connection_grants', 'authoring_command_audit')) \
          OR (namespace.nspname = $1 AND relation.relname IN \
              ('runs', 'test_suites', 'test_cases', \
               'authoring_test_sets', \
               'authoring_report_reservations', \
               'authoring_suite_case_facts', 'authoring_suite_reports'))) \
      ORDER BY namespace.nspname, relation.relname"
}

/// Effective schema USAGE for the host-only author role on catalog and `$1`.
/// OID overloads make an absent role/schema a false row rather than an error,
/// preserving strictly read-only from-zero dry runs.
pub fn select_scenario_author_schema_usage_sql() -> &'static str {
    "SELECT target.schema_name, \
            COALESCE(pg_catalog.has_schema_privilege( \
                author.oid, namespace.oid, 'USAGE'), false) \
       FROM (VALUES ('catalog'::text), ($1::text)) AS target(schema_name) \
       LEFT JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.nspname = target.schema_name \
       LEFT JOIN pg_catalog.pg_roles AS author \
         ON author.rolname = 'wamn_scenario_author' \
      ORDER BY target.schema_name"
}

/// Whether the host-only author can execute the tenant-checking catalog-head
/// lock bridge in `$1`; absent roles/functions yield false for from-zero plans.
pub fn select_scenario_author_catalog_lock_privilege_sql() -> &'static str {
    "SELECT COALESCE(pg_catalog.has_function_privilege( \
         (SELECT oid FROM pg_catalog.pg_roles \
           WHERE rolname = 'wamn_scenario_author'), \
         pg_catalog.to_regprocedure(pg_catalog.format( \
           '%I.lock_catalog_head(text,text,text)', $1::text)), \
         'EXECUTE'), false)"
}

/// Every ordinary table + column in `$1`: `(relname, attname)` in attnum order.
pub fn select_schema_columns_sql() -> &'static str {
    "SELECT c.relname, a.attname FROM pg_class c \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     JOIN pg_attribute a ON a.attrelid = c.oid \
     WHERE n.nspname = $1 AND c.relkind = 'r' AND a.attnum > 0 AND NOT a.attisdropped \
     ORDER BY c.relname, a.attnum"
}

/// Every index in `$1`: `(indexname, indexdef)`.
pub fn select_schema_indexes_sql() -> &'static str {
    "SELECT indexname, indexdef FROM pg_indexes WHERE schemaname = $1"
}

/// Every CHECK on an ordinary table in `$1`: `(table, name, canonical def)`.
pub fn select_schema_checks_sql() -> &'static str {
    "SELECT c.relname, con.conname, pg_get_constraintdef(con.oid, true) \
     FROM pg_constraint con \
     JOIN pg_class c ON c.oid = con.conrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relkind = 'r' AND con.contype = 'c' \
     ORDER BY c.relname, con.conname"
}

/// Every named foreign key on an ordinary table in `$1`.
pub fn select_schema_foreign_keys_sql() -> &'static str {
    "SELECT c.relname, con.conname, pg_get_constraintdef(con.oid, true) \
     FROM pg_constraint con \
     JOIN pg_class c ON c.oid = con.conrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relkind = 'r' AND con.contype = 'f' \
     ORDER BY c.relname, con.conname"
}

/// Every non-internal trigger in `$1`: `(table, name, canonical def)`. A
/// non-origin enablement mode is suffixed so disabled/replica-only guards
/// cannot compare equal to the enabled schema of record.
pub fn select_schema_triggers_sql() -> &'static str {
    "SELECT c.relname, t.tgname, \
            CASE WHEN t.tgenabled = 'O' THEN pg_get_triggerdef(t.oid, true) \
                 ELSE pg_get_triggerdef(t.oid, true) || ' /* trigger-mode:' || t.tgenabled::text || ' */' \
            END \
     FROM pg_trigger t \
     JOIN pg_class c ON c.oid = t.tgrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND NOT t.tgisinternal \
     ORDER BY c.relname, t.tgname"
}

/// The canonical definitions of the run-state helper functions in `$1`.
pub fn select_run_plane_helper_functions_sql() -> &'static str {
    "SELECT p.proname, pg_get_functiondef(p.oid) \
     FROM pg_proc p \
     JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = $1 \
       AND p.proname IN ('lock_catalog_head', 'guard_event_lineage_immutable', \
                         'reject_immutable_effect_fact_change', \
                         'reject_immutable_authoring_test_set_change', \
                         'reject_immutable_authoring_report_change', \
                         'guard_authoring_report_write', \
                         'guard_effect_fact_append', \
                         'guard_effect_disposition_append') \
     ORDER BY p.proname"
}

/// Tables in `$1` carrying the legacy `wamn_outbox_event` trigger.
pub fn select_outbox_trigger_tables_sql() -> &'static str {
    "SELECT c.relname FROM pg_trigger t \
     JOIN pg_class c ON c.oid = t.tgrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND t.tgname = 'wamn_outbox_event' AND NOT t.tgisinternal"
}

/// Whether the legacy `wamn_outbox_event()` function exists in `$1`.
pub fn select_outbox_function_present_sql() -> &'static str {
    "SELECT EXISTS ( SELECT FROM pg_proc p \
     JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = $1 AND p.proname = 'wamn_outbox_event' )"
}

/// Whether the per-database `catalog` metadata schema exists.
pub fn catalog_schema_present_sql() -> &'static str {
    "SELECT EXISTS ( SELECT FROM pg_namespace WHERE nspname = 'catalog' )"
}

/// Rows in `catalog.event_registrations` still carrying the legacy `state` key
/// (the shell runs this only when the table was observed present).
pub fn count_stale_registration_state_sql() -> &'static str {
    "SELECT count(*) FROM catalog.event_registrations WHERE registration ? 'state'"
}

// ---------------------------------------------------------------------------
// Record parsing: slice the deploy/sql sources per table. The files follow the
// repo layout convention (one `CREATE TABLE <q>.<t> (` per table, one column
// per definition start line, full-line `--` comments, statements after the
// table body up to the next CREATE TABLE belong to that table's section); the
// tests below pin the parse against all four shipped files so a layout change
// fails here, not silently.
// ---------------------------------------------------------------------------

/// The canonical deploy DDL rewrite from the `wamn_run` schema to the target
/// project schema (the `publish-catalog --runstate` convention, relocated here
/// as the single owner). The dot-anchored replace leaves prose mentions like
/// `wamn_run_store` untouched. [`BareSchemaName`] makes the unquoted
/// interpolation requirement explicit in the API.
pub fn rewrite_schema(ddl: &str, schema: &BareSchemaName) -> String {
    ddl.replace(
        "SET search_path = pg_catalog, wamn_run, pg_temp",
        &format!("SET search_path = pg_catalog, {schema}, pg_temp"),
    )
    .replace("wamn_run.", &format!("{schema}."))
    // The guarded form FIRST: `SCHEMA wamn_run` is not a substring of it, so
    // missing it left `CREATE SCHEMA IF NOT EXISTS wamn_run` unrewritten (the
    // pre-wamn-1wdq bug: publish --runstate silently created a stray
    // `wamn_run` schema on the target DB while publish pre-created the real
    // target — caught by this verb's from-zero gate leg).
    .replace(
        "SCHEMA IF NOT EXISTS wamn_run",
        &format!("SCHEMA IF NOT EXISTS {schema}"),
    )
    .replace("SCHEMA wamn_run", &format!("SCHEMA {schema}"))
}

/// Every `CREATE TABLE <qualifier>.<name>` in `src`, in file order.
fn record_tables(src: &str, qualifier: &str) -> Vec<String> {
    let head = format!("CREATE TABLE {qualifier}.");
    src.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix(&head)?;
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// The file header: every line before the first `CREATE TABLE <qualifier>.`.
/// For run-state.sql this is the idempotent `CREATE SCHEMA IF NOT EXISTS` +
/// role usage grant (plus prose comments).
#[cfg(test)]
fn header_section(src: &str, qualifier: &str) -> String {
    let head = format!("CREATE TABLE {qualifier}.");
    src.lines()
        .take_while(|line| !line.trim().starts_with(&head))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The schema declaration/grant prefix only. Helper functions are reconciled
/// independently, so a missing table can never replay a plain `CREATE
/// FUNCTION` against an already-present helper.
fn schema_header_section(src: &str, qualifier: &str) -> String {
    let function_head = format!("CREATE FUNCTION {qualifier}.");
    src.lines()
        .take_while(|line| !line.trim().starts_with(&function_head))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One table's section: from its `CREATE TABLE` line up to (excluding) the next
/// `CREATE TABLE <qualifier>.`, independently reconciled run-plane helper
/// function, or EOF — the table body plus its indexes, RLS enablement, policy,
/// triggers, and grants. Catalog helpers remain part of their table sections;
/// run-plane helpers are reconciled independently before missing table sections
/// execute. Leading comment banners belong to the PREVIOUS section (they are
/// comments; nothing is lost).
fn table_section(src: &str, qualifier: &str, table: &str) -> String {
    let head = format!("CREATE TABLE {qualifier}.{table} (");
    let any_head = format!("CREATE TABLE {qualifier}.");
    let function_head = format!("CREATE FUNCTION {qualifier}.");
    let replace_function_head = format!("CREATE OR REPLACE FUNCTION {qualifier}.");
    let mut out = Vec::new();
    let mut in_section = false;
    for line in src.lines() {
        let t = line.trim();
        if !in_section {
            if t.starts_with(&head) {
                in_section = true;
                out.push(line);
            }
            continue;
        }
        if t.starts_with(&any_head)
            || (qualifier == "wamn_run"
                && (t.starts_with(&function_head) || t.starts_with(&replace_function_head)))
        {
            break;
        }
        if t == "-- BEGIN POST-TABLE CONSTRAINTS" {
            break;
        }
        out.push(line);
    }
    assert!(
        !out.is_empty(),
        "record parse: no section for {qualifier}.{table} — schema-of-record layout changed"
    );
    out.join("\n")
}

/// The column definitions of `CREATE TABLE <qualifier>.<table> ( … )` in `src`:
/// `(name, full definition)` pairs, in record order, constraints and comments
/// skipped. Parenthesis-depth aware so a multi-line definition (the `runs`
/// status CHECK) parses whole; definitions are whitespace-collapsed for direct
/// use in `ALTER TABLE … ADD COLUMN`.
fn record_columns(src: &str, qualifier: &str, table: &str) -> Vec<(String, String)> {
    const CONSTRAINT_KEYWORDS: [&str; 5] = ["PRIMARY", "FOREIGN", "CONSTRAINT", "CHECK", "UNIQUE"];
    let head = format!("CREATE TABLE {qualifier}.{table} (");
    let mut cols = Vec::new();
    let mut in_table = false;
    let mut depth: i32 = 0;
    let mut item: Option<(bool, Vec<String>)> = None; // (is_column, lines)
    let flush = |item: &mut Option<(bool, Vec<String>)>, cols: &mut Vec<(String, String)>| {
        if let Some((true, lines)) = item.take() {
            let def = lines
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let def = def.strip_suffix(',').unwrap_or(&def).to_string();
            let name = def
                .split_whitespace()
                .next()
                .expect("non-empty column definition")
                .to_string();
            cols.push((name, def));
        }
    };
    for line in src.lines() {
        let t = line.trim();
        if !in_table {
            if t.starts_with(&head) {
                in_table = true;
            }
            continue;
        }
        if depth == 0 && item.is_none() && t.starts_with(')') {
            break; // end of the table body
        }
        if t.is_empty() || t.starts_with("--") {
            continue;
        }
        if item.is_none() {
            let tok = t.split_whitespace().next().unwrap_or_default();
            let is_column = !CONSTRAINT_KEYWORDS.contains(&tok);
            item = Some((is_column, Vec::new()));
        }
        if let Some((_, lines)) = &mut item {
            lines.push(t.to_string());
        }
        depth += t.chars().filter(|c| *c == '(').count() as i32;
        depth -= t.chars().filter(|c| *c == ')').count() as i32;
        if depth <= 0 && t.ends_with(',') {
            depth = 0;
            flush(&mut item, &mut cols);
        }
        if depth < 0 {
            // The body's closing `)` rode the last item's line; flush and stop.
            flush(&mut item, &mut cols);
            break;
        }
    }
    flush(&mut item, &mut cols);
    assert!(
        !cols.is_empty(),
        "record parse: no columns for {qualifier}.{table} — schema-of-record layout changed"
    );
    cols
}

/// Every `CREATE [UNIQUE] INDEX <name> ON <qualifier>.<table> …;` statement in
/// `src`: `(index name, table, full statement)`.
fn index_statements(src: &str, qualifier: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in src.lines() {
        let t = line.trim();
        match &mut current {
            None if t.starts_with("CREATE INDEX ") || t.starts_with("CREATE UNIQUE INDEX ") => {
                current = Some(vec![t.to_string()]);
            }
            None => continue,
            Some(lines) => lines.push(t.to_string()),
        }
        if t.ends_with(';') {
            let stmt = current.take().expect("complete statement").join(" ");
            let stmt = stmt.strip_suffix(';').unwrap_or(&stmt).to_string();
            let mut words = stmt.split_whitespace().skip_while(|w| *w != "INDEX");
            words.next(); // "INDEX"
            let name = words.next().expect("index name").to_string();
            let mut words = stmt.split_whitespace().skip_while(|w| *w != "ON");
            words.next(); // "ON"
            let table = words
                .next()
                .expect("index table")
                .trim_start_matches(&format!("{qualifier}."))
                .to_string();
            out.push((name, table, stmt));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    // Verified against the last complete reorganized schema in git history
    // (`95be7d1`). Keeping the required final apparatus as one exact suffix
    // detects mid-token truncation, omitted grants/indexes, reordered objects,
    // and unreviewed objects appended after the canonical tail.
    const EVENT_REGISTRATIONS_TAIL: &str = "\
CREATE POLICY event_registrations_tenant ON catalog.event_registrations
    USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))
    WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.event_registrations TO wamn_app;
-- Impact-analysis (wamn-wvb) + materializer lookup by the rename-proof entity id.
CREATE INDEX event_registrations_by_entity
    ON catalog.event_registrations (tenant_id, catalog_id, entity_id);
";

    fn catalog_tail_is_complete(sql: &str) -> bool {
        sql.ends_with(EVENT_REGISTRATIONS_TAIL)
    }

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
            scenario_author_can_lock_catalog_head: true,
            scenario_author_role: Some(ScenarioAuthorRoleObservation {
                can_login: false,
                is_superuser: false,
                can_create_database: false,
                can_create_role: false,
                inherits_roles: false,
                can_replicate: false,
                bypasses_rls: false,
            }),
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
            let columns = record_columns(CATALOG_SCHEMA_SQL, "catalog", &table)
                .into_iter()
                .map(|(column, _)| column)
                .collect();
            obs.catalog_tables.insert(table.clone());
            obs.catalog_columns.insert(table, columns);
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
        obs.catalog_checks.insert(
            (
                "flow_artifacts".to_string(),
                FLOW_AUTHOR_CHECK_NAME.to_string(),
            ),
            FLOW_AUTHOR_CHECK_DEF.to_string(),
        );
        obs.catalog_checks.insert(
            (
                "release_manifests".to_string(),
                RELEASE_PUBLISHER_CHECK_NAME.to_string(),
            ),
            RELEASE_PUBLISHER_CHECK_DEF.to_string(),
        );
        for spec in CHECK_SPECS {
            obs.checks.insert(
                (spec.table.to_string(), spec.name.to_string()),
                spec.definition.to_string(),
            );
        }
        for spec in HELPER_SPECS {
            obs.helper_functions
                .insert(spec.name.to_string(), spec.definition.to_string());
        }
        for spec in trigger_specs() {
            obs.triggers
                .insert((spec.table, spec.name), spec.definition);
        }
        obs.foreign_keys.insert(
            (
                "node_runs".to_string(),
                CURRENT_EFFECT_ATTEMPT_FK_NAME.to_string(),
            ),
            CURRENT_EFFECT_ATTEMPT_FK_DEF.to_string(),
        );
        obs.foreign_keys.insert(
            (
                "effect_attempts".to_string(),
                PREDECESSOR_EFFECT_ATTEMPT_FK_NAME.to_string(),
            ),
            PREDECESSOR_EFFECT_ATTEMPT_FK_DEF.to_string(),
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
    fn record_tables_are_pinned() {
        assert_eq!(
            record_tables(RUN_STATE_SQL, "wamn_run"),
            [
                "runs",
                "invocation_admissions",
                "node_runs",
                "effect_attempts",
                "effect_attempt_dispatches",
                "effect_attempt_outcomes",
                "effect_disposition_requests",
                "effect_dispositions",
            ]
        );
        assert_eq!(record_tables(FLOWS_SQL, "wamn_run"), ["flows"]);
        assert_eq!(
            record_tables(FLOW_TESTS_SQL, "wamn_run"),
            [
                "test_suites",
                "test_cases",
                "authoring_test_sets",
                "authoring_report_reservations",
                "authoring_suite_case_facts",
                "authoring_suite_reports",
            ]
        );
        assert_eq!(
            record_tables(RUN_QUEUE_SQL, "wamn_run"),
            ["run_queue", "partition_owner", "run_dead_letters"]
        );
        let catalog = record_tables(CATALOG_SCHEMA_SQL, "catalog");
        assert!(catalog.first().is_some_and(|t| t == "catalogs"));
        assert!(catalog.contains(&"event_registrations".to_string()));
        for exposure_table in [
            "release_exposure_manifests",
            "release_sources",
            "release_attachments",
            "attachment_tombstones",
            "attachment_activation",
            "attachment_activation_events",
        ] {
            assert!(catalog.contains(&exposure_table.to_string()));
        }
        for connection_table in [
            "connection_requirements",
            "connection_instances",
            "connection_generations",
            "connection_bindings",
            "connection_generation_retention",
            "draft_safe_connection_grants",
        ] {
            assert!(catalog.contains(&connection_table.to_string()));
        }
        for authoring_table in [
            "flow_drafts",
            "validated_flow_drafts",
            "authoring_command_audit",
        ] {
            assert!(catalog.contains(&authoring_table.to_string()));
        }
        assert_eq!(
            catalog.len(),
            29,
            "catalog-schema.sql table count: {catalog:?}"
        );
    }

    #[test]
    fn catalog_schema_ends_with_the_complete_event_registration_apparatus() {
        assert!(catalog_tail_is_complete(CATALOG_SCHEMA_SQL));
    }

    #[test]
    fn catalog_tail_guard_rejects_truncation_omission_and_object_order_drift() {
        let truncated = CATALOG_SCHEMA_SQL
            .strip_suffix("id);\n")
            .expect("canonical tail ends with the entity index");
        assert!(!catalog_tail_is_complete(truncated));

        let without_grant = CATALOG_SCHEMA_SQL.replace(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.event_registrations TO wamn_app;\n",
            "",
        );
        assert!(!catalog_tail_is_complete(&without_grant));

        let grant =
            "GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.event_registrations TO wamn_app;\n";
        let before_tail = CATALOG_SCHEMA_SQL
            .strip_suffix(EVENT_REGISTRATIONS_TAIL)
            .expect("canonical schema has the guarded tail");
        let reordered_tail = format!("{}{grant}", EVENT_REGISTRATIONS_TAIL.replace(grant, ""));
        let reordered = format!("{before_tail}{reordered_tail}");
        assert!(!catalog_tail_is_complete(&reordered));
    }

    #[test]
    fn run_queue_record_columns_carry_the_drift_set() {
        let cols = record_columns(RUN_QUEUE_SQL, "wamn_run", "run_queue");
        let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "tenant_id",
                "run_id",
                "partition_key",
                "partition_policy",
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
        // The E4 / D20 definitions the drifted-schema ALTERs are built from.
        let def = |n: &str| cols.iter().find(|(c, _)| c == n).unwrap().1.clone();
        assert_eq!(def("stream_seq"), "stream_seq bigint NOT NULL DEFAULT 0");
        assert_eq!(
            def("partition_policy"),
            "partition_policy text NOT NULL DEFAULT 'blocking' \
             CHECK (partition_policy IN ('blocking', 'leapfrog'))"
        );
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
            !names.contains(&"'cancelled',"),
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
    fn effect_disposition_append_order_and_closed_outcome_are_pinned() {
        let columns = record_columns(RUN_STATE_SQL, "wamn_run", "effect_dispositions");
        let append = columns
            .iter()
            .find(|(name, _)| name == "append_ordinal")
            .expect("append-order column is in the schema of record");
        assert_eq!(
            append.1,
            "append_ordinal bigint GENERATED ALWAYS AS IDENTITY"
        );

        let history = index_statements(RUN_STATE_SQL, "wamn_run")
            .into_iter()
            .find(|(name, _, _)| name == "effect_dispositions_attempt_history")
            .expect("attempt-history index is in the schema of record")
            .2;
        assert!(history.contains("append_ordinal DESC"), "{history}");
        assert!(!history.contains("created_at"), "{history}");
        let append_order = index_statements(RUN_STATE_SQL, "wamn_run")
            .into_iter()
            .find(|(name, _, _)| name == "effect_dispositions_append_order")
            .expect("global append-order uniqueness is in the schema of record")
            .2;
        assert!(append_order.starts_with("CREATE UNIQUE INDEX"));
        assert!(append_order.contains("(append_ordinal)"));

        let outcome = CHECK_SPECS
            .iter()
            .find(|spec| spec.name == "effect_dispositions_outcome_check")
            .expect("closed outcome CHECK is observed");
        assert!(outcome.definition.ends_with(" IS TRUE)"));
        assert!(
            outcome
                .definition
                .contains("failure_detail ? 'message'::text")
        );
        assert!(
            outcome
                .definition
                .contains("jsonb_typeof(failure_detail -> 'message'::text) = 'string'::text")
        );
    }

    /// Sections carry the table's whole apparatus: indexes, RLS, policy, grant.
    #[test]
    fn table_sections_carry_indexes_rls_and_grants() {
        let rq = table_section(RUN_QUEUE_SQL, "wamn_run", "run_queue");
        assert!(rq.contains("CREATE INDEX run_queue_claimable"));
        assert!(rq.contains("CREATE INDEX run_queue_partition"));
        assert!(rq.contains("FORCE ROW LEVEL SECURITY"));
        assert!(!rq.contains("CREATE TABLE wamn_run.partition_owner"));

        let dl = table_section(RUN_QUEUE_SQL, "wamn_run", "run_dead_letters");
        assert!(dl.contains("GRANT SELECT, INSERT ON wamn_run.run_dead_letters"));
        assert!(dl.contains("CREATE POLICY run_dead_letters_tenant"));

        let cat = table_section(CATALOG_SCHEMA_SQL, "catalog", "catalogs");
        assert!(cat.contains("catalogs_one_applied_per_env"));
        let artifacts = table_section(CATALOG_SCHEMA_SQL, "catalog", "flow_artifacts");
        assert!(artifacts.contains("register_flow_artifact"));

        let dispositions = table_section(RUN_STATE_SQL, "wamn_run", "effect_dispositions");
        assert!(dispositions.contains("effect_dispositions_delete_immutable"));
        assert!(!dispositions.contains("node_runs_current_effect_attempt_fk"));

        let cases = table_section(FLOW_TESTS_SQL, "wamn_run", "test_cases");
        assert!(!cases.contains("reject_immutable_authoring_test_set_change"));
        let test_sets = table_section(FLOW_TESTS_SQL, "wamn_run", "authoring_test_sets");
        assert!(test_sets.contains("authoring_test_sets_update_immutable"));
        assert!(test_sets.contains("GRANT SELECT, INSERT"));
        assert!(!test_sets.contains("reject_immutable_authoring_report_change"));

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
                "authoring_suite_reports_flow",
                "effect_attempts_bulk_scope",
                "effect_attempts_occurrence",
                "effect_dispositions_append_order",
                "effect_dispositions_attempt_history",
                "effect_dispositions_one_resolution",
                "effect_dispositions_request_ordinal",
                "flows_active",
                "flows_active_webhook_path",
                "invocation_admissions_expiry",
                "invocation_admissions_run",
                "node_runs_seq",
                "run_queue_claimable",
                "run_queue_partition",
                "runs_cancel_requested",
                "runs_event_root",
                "runs_flow",
                "runs_idempotency",
                "runs_invoke_root",
                "runs_parent_occurrence",
                "runs_response_deadline",
                "runs_root",
                "runs_run_deadline",
                "runs_waiting_child",
            ]
        );
        let (_, table, stmt) = index_statements(RUN_QUEUE_SQL, "wamn_run")
            .into_iter()
            .find(|(n, _, _)| n == "run_queue_claimable")
            .unwrap();
        assert_eq!(table, "run_queue");
        assert!(stmt.contains("stream_seq"));
        // The multi-line partial expression index parses to one statement.
        let (_, _, wh) = index_statements(FLOWS_SQL, "wamn_run")
            .into_iter()
            .find(|(n, _, _)| n == "flows_active_webhook_path")
            .unwrap();
        assert!(wh.contains("IS NOT NULL"), "{wh}");
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
            18,
            "all eighteen run-plane tables at target, including test-set inputs and reports"
        );
    }

    #[test]
    fn effective_indirect_or_owner_authority_never_plans_false_clean() {
        let mut obs = observation_at_record();
        obs.authoring_table_owners.insert(
            ("catalog".to_string(), "flow_drafts".to_string()),
            "wamn_app".to_string(),
        );
        obs.authoring_effective_table_privileges
            .entry((
                "catalog".to_string(),
                "draft_safe_connection_grants".to_string(),
                "wamn_app".to_string(),
            ))
            .or_default()
            .insert("INSERT".to_string());
        obs.authoring_effective_table_privileges
            .entry((
                "catalog".to_string(),
                "release_manifests".to_string(),
                SCENARIO_AUTHOR_ROLE.to_string(),
            ))
            .or_default()
            .insert("UPDATE".to_string());
        obs.authoring_effective_column_privileges
            .entry((
                "catalog".to_string(),
                "validated_flow_drafts".to_string(),
                "wamn_app".to_string(),
            ))
            .or_default()
            .insert("UPDATE".to_string());

        let plan = plan_run_plane(&schema("demo"), &obs);
        for table in [
            "flow_drafts",
            "draft_safe_connection_grants",
            "release_manifests",
            "validated_flow_drafts",
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
            assert!(
                repair
                    .sql
                    .contains("authoring-effective-privilege-out-of-bounds")
            );
            if table == "validated_flow_drafts" {
                assert!(repair.sql.contains("has_any_column_privilege"));
            }
            if table == "flow_drafts" {
                assert!(repair.sql.contains("relation.relowner"));
            }
        }
    }

    #[test]
    fn guest_column_select_on_authoring_test_sets_never_plans_false_clean() {
        let mut obs = observation_at_record();
        obs.authoring_effective_column_privileges
            .entry((
                "demo".to_string(),
                "authoring_test_sets".to_string(),
                "wamn_app".to_string(),
            ))
            .or_default()
            .insert("SELECT".to_string());

        let plan = plan_run_plane(&schema("demo"), &obs);
        let repair = plan
            .actions
            .iter()
            .find(|action| {
                action.kind == RunPlaneActionKind::RepairAuthoringPrivilege
                    && action.target == "demo.authoring_test_sets"
            })
            .expect("column-level guest read authority must be surfaced");
        assert!(repair.sql.contains("DO $effective_acl$"));
        assert!(repair.sql.contains("pg_catalog.has_any_column_privilege"));
        assert!(
            repair
                .sql
                .contains("'wamn_app', '\"demo\".\"authoring_test_sets\"', 'SELECT'")
        );
        assert!(
            repair
                .sql
                .contains("authoring-effective-privilege-out-of-bounds")
        );
    }

    /// The v1-era drift set (the live 2jkm.41 sweep findings) plans exactly the
    /// additive repairs: E4/D20 columns, the claimable-index recreate, the
    /// missing fqg.20/v8cv tables, the outbox-era teardown, the registration
    /// state strip.
    #[test]
    fn v1_era_drift_plans_the_additive_repairs() {
        let mut obs = observation_at_record();
        // run_queue predates E4 + D20; the claimable index predates stream_seq.
        let rq = obs.tables.get_mut("run_queue").unwrap();
        rq.remove("stream_seq");
        rq.remove("partition_policy");
        obs.indexes.insert(
            "run_queue_claimable".into(),
            "CREATE INDEX run_queue_claimable ON demo.run_queue \
             USING btree (tenant_id, available_at, lease_expires_at)"
                .into(),
        );
        // fqg.20 / v8cv tables not yet provisioned.
        obs.tables.remove("partition_owner");
        obs.tables.remove("run_dead_letters");
        obs.indexes.remove("run_queue_partition");
        // The outbox era: tables + trigger + function + a stored state key.
        obs.tables
            .insert("outbox".into(), BTreeSet::from(["id".into()]));
        obs.tables
            .insert("evt_shadow".into(), BTreeSet::from(["id".into()]));
        obs.outbox_trigger_tables = vec!["receipts".into()];
        obs.outbox_function_present = true;
        obs.stale_registration_state_rows = 2;
        // The catalog schema predates l5i9.16.
        obs.catalog_tables.remove("event_registrations");

        let plan = plan_run_plane(&schema("demo"), &obs);
        let sqls: Vec<&str> = plan.actions.iter().map(|a| a.sql.as_str()).collect();
        let kinds: Vec<RunPlaneActionKind> = plan.actions.iter().map(|a| a.kind).collect();

        assert!(sqls.contains(
            &"ALTER TABLE \"demo\".\"run_queue\" ADD COLUMN stream_seq bigint NOT NULL DEFAULT 0"
        ));
        assert!(sqls.iter().any(|s| s.starts_with(
            "ALTER TABLE \"demo\".\"run_queue\" ADD COLUMN partition_policy text NOT NULL"
        )));
        let recreate = plan
            .actions
            .iter()
            .find(|a| a.kind == RunPlaneActionKind::RecreateIndex)
            .expect("claimable index recreates");
        assert!(
            recreate
                .sql
                .starts_with("DROP INDEX \"demo\".\"run_queue_claimable\"; ")
        );
        assert!(recreate.sql.contains("stream_seq"));
        assert!(plan
            .actions
            .iter()
            .any(|a| a.kind == RunPlaneActionKind::CreateTable && a.target == "partition_owner"));
        assert!(plan.actions.iter().any(|a| {
            a.kind == RunPlaneActionKind::CreateTable
                && a.target == "run_dead_letters"
                && a.sql
                    .contains("GRANT SELECT, INSERT ON demo.run_dead_letters")
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
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == RunPlaneActionKind::CreateCatalogTable
                    && a.target == "event_registrations")
        );
        assert!(sqls.contains(&strip_registration_state_sql()));
        // Nothing in the plan drops a live COLUMN (additive posture).
        assert!(!sqls.iter().any(|s| s.contains("DROP COLUMN")));
    }

    #[test]
    fn disposition_security_drift_plans_exact_additive_repairs() {
        let mut obs = observation_at_record();
        obs.tables
            .get_mut("effect_dispositions")
            .expect("record disposition table")
            .remove("append_ordinal");
        obs.indexes.insert(
            "effect_dispositions_attempt_history".into(),
            "CREATE INDEX effect_dispositions_attempt_history ON demo.effect_dispositions USING btree (tenant_id, attempt_id, created_at DESC)".into(),
        );
        obs.indexes.remove("effect_dispositions_append_order");
        obs.indexes.insert(
            "effect_dispositions_request_ordinal".into(),
            "CREATE UNIQUE INDEX effect_dispositions_request_ordinal ON demo.effect_dispositions USING btree (tenant_id, request_id, selection_ordinal) WHERE false".into(),
        );
        obs.indexes.insert(
            "effect_dispositions_one_resolution".into(),
            "CREATE UNIQUE INDEX effect_dispositions_one_resolution ON demo.effect_dispositions USING btree (tenant_id, attempt_id) WHERE ((action = 'resolve'::text) OR true)".into(),
        );
        obs.checks.insert(
            (
                "effect_dispositions".into(),
                "effect_dispositions_outcome_check".into(),
            ),
            "CHECK (true)".into(),
        );
        obs.helper_functions.insert(
            "guard_effect_disposition_append".into(),
            "CREATE OR REPLACE FUNCTION demo.guard_effect_disposition_append()".into(),
        );

        let plan = plan_run_plane(&schema("demo"), &obs);
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::AddColumn
                && action.target == "effect_dispositions.append_ordinal"
                && action
                    .sql
                    .contains("append_ordinal bigint GENERATED ALWAYS AS IDENTITY")
        }));
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RecreateIndex
                && action.target == "effect_dispositions_attempt_history"
                && action.sql.contains("append_ordinal DESC")
                && !action.sql.contains("created_at DESC")
        }));
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::CreateIndex
                && action.target == "effect_dispositions_append_order"
        }));
        for target in [
            "effect_dispositions_request_ordinal",
            "effect_dispositions_one_resolution",
        ] {
            assert!(plan.actions.iter().any(|action| {
                action.kind == RunPlaneActionKind::RecreateIndex && action.target == target
            }));
        }
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairConstraint
                && action.target == "effect_dispositions.effect_dispositions_outcome_check"
                && action.sql.contains("IS TRUE")
                && action.sql.contains("failure_detail ? 'message'::text")
        }));
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairHelperFunction
                && action.target == "guard_effect_disposition_append"
                && action.sql.contains("SET search_path = pg_catalog, pg_temp")
                && !action.sql.contains("pg_has_role")
        }));
    }

    #[test]
    fn legacy_attempt_backfill_precedes_current_pointer_fk() {
        let mut obs = observation_at_record();
        let node_columns = obs.tables.get_mut("node_runs").expect("record node table");
        node_columns.remove("current_effect_attempt_id");
        node_columns.remove("attempt_input_ref");
        obs.foreign_keys.remove(&(
            "node_runs".to_string(),
            CURRENT_EFFECT_ATTEMPT_FK_NAME.to_string(),
        ));

        let plan = plan_run_plane(&schema("demo"), &obs);
        let add_pointer = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::AddColumn
                    && action.target == "node_runs.current_effect_attempt_id"
            })
            .expect("add current pointer");
        let add_legacy_column = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::AddColumn
                    && action.target == "node_runs.attempt_input_ref"
            })
            .expect("add missing legacy authority column");
        let backfill = plan
            .actions
            .iter()
            .position(|action| action.kind == RunPlaneActionKind::BackfillEffectAttempts)
            .expect("legacy effect backfill");
        let foreign_key = plan
            .actions
            .iter()
            .position(|action| action.kind == RunPlaneActionKind::RepairForeignKey)
            .expect("current pointer FK");
        assert!(add_pointer < backfill && add_legacy_column < backfill && backfill < foreign_key);
        let sql = &plan.actions[backfill].sql;
        assert!(sql.contains("LOCK TABLE \"demo\".node_runs IN SHARE ROW EXCLUSIVE MODE"));
        assert!(sql.contains("legacy-effect-attempt-incomplete"));
        assert!(sql.contains("legacy-effect-attempt-backfill-incomplete"));
        assert!(sql.contains(") IS NOT TRUE"), "NULL facts must refuse");
        assert!(sql.contains("legacy-effect-attempt-connection-unresolved"));
        assert!(sql.contains("INSERT INTO \"demo\".effect_attempts"));
        assert!(sql.contains("INSERT INTO \"demo\".effect_attempt_dispatches"));
        assert!(sql.contains("INSERT INTO \"demo\".effect_attempt_outcomes"));
        assert!(sql.contains("predecessor_attempt_id,legacy_imported"));
        assert!(sql.contains("SET current_effect_attempt_id = c.attempt_id"));
        assert!(!sql.contains("verified_author_principal AS"));
    }

    #[test]
    fn effect_lineage_and_temporal_fks_are_repaired_on_existing_ledgers() {
        let mut obs = observation_at_record();
        for (table, name) in [
            ("effect_attempts", PREDECESSOR_EFFECT_ATTEMPT_FK_NAME),
            ("effect_attempt_dispatches", EFFECT_DISPATCH_ATTEMPT_FK_NAME),
            ("effect_attempt_outcomes", EFFECT_OUTCOME_DISPATCH_FK_NAME),
        ] {
            obs.foreign_keys
                .remove(&(table.to_string(), name.to_string()));
        }

        let targets: BTreeSet<String> = plan_run_plane(&schema("demo"), &obs)
            .actions
            .into_iter()
            .filter(|action| action.kind == RunPlaneActionKind::RepairForeignKey)
            .map(|action| action.target)
            .collect();
        for target in [
            "effect_attempts.effect_attempts_predecessor_fk",
            "effect_attempt_dispatches.effect_attempt_dispatches_attempt_fk",
            "effect_attempt_outcomes.effect_attempt_outcomes_dispatch_fk",
        ] {
            assert!(
                targets.contains(target),
                "missing repair for {target}: {targets:#?}"
            );
        }
    }

    #[test]
    fn catalog_provenance_columns_are_reconciled_before_runtime_activation() {
        let mut obs = observation_at_record();
        obs.catalog_columns
            .get_mut("flow_artifacts")
            .expect("flow artifact columns")
            .remove("verified_author_principal");
        obs.catalog_columns
            .get_mut("release_manifests")
            .expect("release manifest columns")
            .remove("verified_publisher_principal");

        let plan = plan_run_plane(&schema("demo"), &obs);
        let action = plan
            .actions
            .iter()
            .find(|action| action.kind == RunPlaneActionKind::EnsureCatalogProvenance)
            .expect("catalog provenance migration");
        assert!(
            action
                .sql
                .contains("ADD COLUMN IF NOT EXISTS verified_author_principal")
        );
        assert!(
            action
                .sql
                .contains("ADD COLUMN IF NOT EXISTS verified_publisher_principal")
        );
    }

    #[test]
    fn catalog_provenance_check_drift_is_repaired() {
        let mut obs = observation_at_record();
        obs.catalog_checks.insert(
            (
                "flow_artifacts".to_string(),
                FLOW_AUTHOR_CHECK_NAME.to_string(),
            ),
            "CHECK (true)".to_string(),
        );

        let action = plan_run_plane(&schema("demo"), &obs)
            .actions
            .into_iter()
            .find(|action| action.kind == RunPlaneActionKind::EnsureCatalogProvenance)
            .expect("catalog provenance CHECK repair");
        assert!(
            action.sql.contains(
                "DROP CONSTRAINT IF EXISTS flow_artifacts_verified_author_principal_check"
            )
        );
        assert!(
            action
                .sql
                .contains("ADD CONSTRAINT flow_artifacts_verified_author_principal_check")
        );
    }

    #[test]
    fn legacy_attempt_observation_is_schema_scoped() {
        let sql = count_legacy_effect_attempt_rows_sql(&schema("demo"));
        assert!(sql.contains("FROM \"demo\".node_runs"));
        assert!(sql.contains("current_effect_attempt_id IS NULL"));
        assert!(sql.contains("attempt_dispatched_at IS NOT NULL"));
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
                "runs",
                "invocation_admissions",
                "node_runs",
                "effect_attempts",
                "effect_attempt_dispatches",
                "effect_attempt_outcomes",
                "effect_disposition_requests",
                "effect_dispositions",
                "flows",
                "test_suites",
                "test_cases",
                "authoring_test_sets",
                "authoring_report_reservations",
                "authoring_suite_case_facts",
                "authoring_suite_reports",
                "run_queue",
                "partition_owner",
                "run_dead_letters"
            ]
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == RunPlaneActionKind::EnsureCatalogSchema)
        );
        let report_helper = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::RepairHelperFunction
                    && action.target == "reject_immutable_authoring_report_change"
            })
            .expect("authoring report helper is provisioned");
        let report_table = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::CreateTable
                    && action.target == "authoring_suite_reports"
            })
            .expect("authoring report table is provisioned");
        assert!(
            report_helper < report_table,
            "the standalone run-plane helper must exist before report triggers"
        );
        let test_set_helper = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::RepairHelperFunction
                    && action.target == "reject_immutable_authoring_test_set_change"
            })
            .expect("authoring test-set helper is provisioned");
        let test_set_table = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::CreateTable
                    && action.target == "authoring_test_sets"
            })
            .expect("authoring test-set table is provisioned");
        assert!(
            test_set_helper < test_set_table,
            "the standalone run-plane helper must exist before test-set triggers"
        );
        // No column/index repairs on tables being created (sections carry them).
        assert!(!kinds.contains(&RunPlaneActionKind::AddColumn));
        assert!(!kinds.contains(&RunPlaneActionKind::CreateIndex));
        assert!(kinds.contains(&RunPlaneActionKind::RepairForeignKey));
        // The rewrite reached the sections.
        let rq = plan
            .actions
            .iter()
            .find(|a| a.target == "run_queue")
            .unwrap();
        assert!(rq.sql.contains("CREATE TABLE wamn_runner_demo.run_queue"));
        assert!(!rq.sql.contains("wamn_run."));
    }

    /// A live column the record does not know is SURFACED, never dropped.
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

    #[test]
    fn authoring_test_set_checks_and_privileges_are_closed_and_pinned() {
        let checks: Vec<&CheckSpec> = CHECK_SPECS
            .iter()
            .filter(|spec| spec.table == "authoring_test_sets")
            .collect();
        assert_eq!(checks.len(), 6);
        assert!(checks.iter().any(|spec| {
            spec.name == "authoring_test_sets_check2"
                && spec.definition == "CHECK (NOT (convert_from(exact_bytes, 'UTF8'::name)::jsonb ->> 'schema-version'::text) IS DISTINCT FROM schema_version)"
        }));

        let privileges = AUTHORING_PRIVILEGE_SPECS
            .iter()
            .find(|spec| {
                matches!(spec.schema, AuthoringTableSchema::RunPlane)
                    && spec.table == "authoring_test_sets"
            })
            .expect("authoring test-set privilege boundary is observed");
        assert!(privileges.app.is_empty());
        assert_eq!(privileges.author, ["SELECT", "INSERT"]);
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
            8
        );
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairTrigger
                && action.target == "runs.runs_event_lineage_immutable"
        }));
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairTrigger
                && action.target == "effect_attempts.effect_attempts_insert_guard"
        }));
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairTrigger
                && action.target
                    == "effect_disposition_requests.effect_disposition_requests_insert_guard"
        }));
        assert_eq!(
            plan.actions
                .iter()
                .filter(|action| action.kind == RunPlaneActionKind::RepairTrigger)
                .count(),
            27
        );
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairTrigger
                && action.target == "authoring_test_sets.authoring_test_sets_delete_immutable"
        }));
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairTrigger
                && action.target
                    == "authoring_suite_reports.authoring_suite_reports_update_immutable"
        }));
    }

    #[test]
    fn effect_fact_append_guard_is_migration_only_and_temp_safe() {
        assert!(GUARD_EFFECT_FACT_APPEND_SQL.contains("SET search_path = pg_catalog, pg_temp"));
        assert!(GUARD_EFFECT_FACT_APPEND_SQL.contains("candidate.rolsuper"));
        assert!(GUARD_EFFECT_FACT_APPEND_SQL.contains("candidate.rolbypassrls"));
        assert!(
            GUARD_EFFECT_FACT_APPEND_SQL
                .contains("effect-fact-append-requires-migration-authority")
        );
        assert!(!GUARD_EFFECT_FACT_APPEND_SQL.contains("wamn_app"));
    }

    #[test]
    fn disposition_append_guard_is_catalog_qualified_and_temp_safe() {
        assert!(
            GUARD_EFFECT_DISPOSITION_APPEND_SQL.contains("SET search_path = pg_catalog, pg_temp")
        );
        assert!(GUARD_EFFECT_DISPOSITION_APPEND_SQL.contains("pg_catalog.pg_class"));
        assert!(GUARD_EFFECT_DISPOSITION_APPEND_SQL.contains("pg_catalog.pg_roles"));
        assert!(!GUARD_EFFECT_DISPOSITION_APPEND_SQL.contains("wamn_platform_admin"));
        assert!(!GUARD_EFFECT_DISPOSITION_APPEND_SQL.contains("pg_has_role"));
        assert!(
            GUARD_EFFECT_DISPOSITION_APPEND_SQL
                .contains("CURRENT_USER = owner_name AND CURRENT_USER <> SESSION_USER")
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

    /// The queue-missing manifestation (the live poc_f1 case): run-state +
    /// flows present, queue absent → exactly the three queue creates (+ the
    /// schema ensure, which is idempotent).
    #[test]
    fn queue_missing_plans_only_the_queue_creates() {
        let mut obs = observation_at_record();
        obs.tables.remove("run_queue");
        obs.tables.remove("partition_owner");
        obs.tables.remove("run_dead_letters");
        obs.indexes.remove("run_queue_claimable");
        obs.indexes.remove("run_queue_partition");
        let plan = plan_run_plane(&schema("poc_f1"), &obs);
        let creates: Vec<&str> = plan
            .actions
            .iter()
            .filter(|a| a.kind == RunPlaneActionKind::CreateTable)
            .map(|a| a.target.as_str())
            .collect();
        assert_eq!(
            creates,
            ["run_queue", "partition_owner", "run_dead_letters"]
        );
        assert!(
            plan.actions
                .iter()
                .all(|a| a.kind != RunPlaneActionKind::AddColumn)
        );
    }

    /// The dot-anchored rewrite (relocated from publish_catalog as the single
    /// owner): qualified names + the schema header rewrite; prose does not.
    #[test]
    fn schema_rewrite_is_dot_anchored() {
        let schema = schema("poc_f1");
        for (ddl, table) in [(RUN_STATE_SQL, "runs"), (FLOWS_SQL, "flows")] {
            let out = rewrite_schema(ddl, &schema);
            assert!(
                out.contains(&format!("CREATE TABLE poc_f1.{table}")),
                "{table}"
            );
            assert!(!out.contains("wamn_run."), "no qualified wamn_run left");
            assert!(!out.contains("SCHEMA wamn_run"), "schema header rewritten");
        }
        // The GUARDED schema-create form rewrites too (the pre-wamn-1wdq bug:
        // `SCHEMA wamn_run` is not a substring of `SCHEMA IF NOT EXISTS
        // wamn_run`, so the header create silently targeted `wamn_run`).
        let out = rewrite_schema(RUN_STATE_SQL, &schema);
        assert!(out.contains("CREATE SCHEMA IF NOT EXISTS poc_f1 "));
        assert!(!out.contains("IF NOT EXISTS wamn_run"));
        // The prose mention of the wamn_run_store crate must survive verbatim.
        assert!(rewrite_schema(RUN_STATE_SQL, &schema).contains("wamn_run_store"));
        assert!(rewrite_schema(RUN_STATE_SQL, &schema).contains("CREATE TABLE poc_f1.node_runs"));
        assert!(
            rewrite_schema(RUN_STATE_SQL, &schema)
                .contains("SET search_path = pg_catalog, pg_temp")
        );
        assert!(
            !rewrite_schema(RUN_STATE_SQL, &schema)
                .contains("SET search_path = pg_catalog, wamn_run")
        );
        assert!(
            !rewrite_schema(RUN_STATE_SQL, &schema)
                .contains("SET search_path = pg_catalog, pg_temp, poc_f1")
        );
        assert!(
            rewrite_schema(FLOWS_SQL, &schema)
                .contains("CREATE UNIQUE INDEX flows_active_webhook_path ON poc_f1.flows")
        );
    }

    /// Observation SQL pins (the shell binds these verbatim; the live gate
    /// proves they observe real state).
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
        assert!(select_authoring_table_privileges_sql().contains("draft_safe_connection_grants"));
        assert!(select_authoring_table_privileges_sql().contains("authoring_report_reservations"));
        // Every observation query must see the ledger, or the planner reads an
        // empty privilege set and plans a repair that can never converge.
        for observation in [
            select_authoring_table_privileges_sql(),
            select_authoring_effective_table_privileges_sql(),
            select_authoring_effective_column_privileges_sql(),
            select_authoring_table_owners_sql(),
        ] {
            assert!(
                observation.contains("authoring_command_audit"),
                "{observation}"
            );
            assert!(observation.contains("authoring_test_sets"), "{observation}");
        }
        assert!(select_authoring_effective_table_privileges_sql().contains("has_table_privilege"));
        assert!(select_authoring_effective_table_privileges_sql().contains("release_manifests"));
        assert!(select_authoring_table_owners_sql().contains("relation.relowner"));
        assert!(
            select_authoring_effective_column_privileges_sql().contains("has_any_column_privilege")
        );
        assert!(select_authoring_effective_column_privileges_sql().contains("('SELECT'::text)"));
        assert!(select_scenario_author_schema_usage_sql().contains("has_schema_privilege"));
        assert!(
            select_scenario_author_catalog_lock_privilege_sql()
                .contains("lock_catalog_head(text,text,text)")
        );
        assert!(select_run_plane_helper_functions_sql().contains("pg_get_functiondef"));
        assert!(
            select_run_plane_helper_functions_sql().contains("reject_immutable_effect_fact_change")
        );
        assert!(
            select_run_plane_helper_functions_sql()
                .contains("reject_immutable_authoring_report_change")
        );
        assert!(
            select_run_plane_helper_functions_sql()
                .contains("reject_immutable_authoring_test_set_change")
        );
        assert!(
            select_run_plane_helper_functions_sql().contains("guard_effect_disposition_append")
        );
        assert_eq!(
            strip_registration_state_sql(),
            "UPDATE catalog.event_registrations SET registration = registration - 'state' \
             WHERE registration ? 'state'"
        );
    }
}
