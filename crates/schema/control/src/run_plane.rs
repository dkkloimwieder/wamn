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
//!    `deploy/sql/catalog-schema.sql` (the `catalog` metadata schema the
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
//! the frame/effect-writer cutovers remove retired identity/recovery columns;
//! the capture-projection cutover removes retired non-authoritative node
//! columns; the rerun-lineage cutover removes only the two retired run columns
//! and their canonical index while preserving every run row; and the stored-test
//! cutover removes retired persistence. PostgreSQL
//! validates new CHECKs against existing rows and aborts on incompatible data.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use wamn_pg_core::{Identifier, InvalidIdentifier};

/// The schema of record, compiled in — the same sources provisioning applies
/// (`publish-catalog --runstate`, the f1 provisioning Job) and the wamn-9mg8
/// stand-in drift guard pins.
const RUN_STATE_SQL: &str = include_str!("../../../../deploy/sql/run-state.sql");
const AUTHORING_TESTS_SQL: &str = include_str!("../../../../deploy/sql/authoring-tests.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");

const RUNS_EXECUTION_PINS_CHECK_DEF: &str = "CHECK (catalog_id <> ''::text AND catalog_version > 0 AND environment <> ''::text AND execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'::text)";
const RELEASE_FLOWS_BUNDLE_CHECK_DEF: &str =
    "CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'::text)";
const RUNS_RELEASE_FK_DEF: &str = "FOREIGN KEY (tenant_id, catalog_id, catalog_version) REFERENCES catalog.release_manifests(tenant_id, catalog_id, catalog_version)";
const RUNS_EXECUTION_BUNDLE_FK_DEF: &str = "FOREIGN KEY (tenant_id, execution_bundle_hash) REFERENCES catalog.execution_bundles(tenant_id, execution_bundle_hash)";
const RELEASE_FLOWS_EXECUTION_BUNDLE_FK_DEF: &str = "FOREIGN KEY (tenant_id, execution_bundle_hash) REFERENCES catalog.execution_bundles(tenant_id, execution_bundle_hash)";
const RUNS_RELEASE_INDEX_DEF: &str = "CREATE INDEX runs_release ON wamn_run.runs USING btree (tenant_id, catalog_id, catalog_version)";
const RUNS_EXECUTION_BUNDLE_INDEX_DEF: &str = "CREATE INDEX runs_execution_bundle ON wamn_run.runs USING btree (tenant_id, execution_bundle_hash)";
const RUNS_ROOT_INDEX_DEF: &str = "CREATE INDEX runs_root ON wamn_run.runs USING btree (tenant_id, root_run_id) WHERE (root_run_id IS NOT NULL)";
const RELEASE_FLOWS_EXECUTION_BUNDLE_INDEX_DEF: &str = "CREATE INDEX release_flows_execution_bundle ON catalog.release_flows USING btree (tenant_id, execution_bundle_hash)";
const RUNS_ADMISSION_PINS_TRIGGER_DEF: &str = "CREATE TRIGGER runs_admission_pins_immutable BEFORE UPDATE OF catalog_id, catalog_version, environment, execution_bundle_hash, capture_mode, durability_class, release_version, manifest_digest ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_run_admission_pins_immutable()";
const ENVIRONMENT_POLICY_TENANT_QUAL: &str =
    "tenant_id = NULLIF(current_setting('app.tenant'::text, true), ''::text)";

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
        definition: RUNS_EXECUTION_PINS_CHECK_DEF,
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
        // The claim-time release record: absent, or complete and well formed.
        // Table-origin because it names two columns, and explicitly named so it
        // can never collide with the retired child-run `runs_check3` numbering.
        // The two `IS NOT NULL` conjuncts are what make "complete" true rather
        // than merely claimed: a well-formed half pair leaves the second
        // disjunct NULL, and a NULL CHECK expression is satisfied
        // (wamn-0h0g.15.126). This literal is the `pg_get_constraintdef(oid,
        // true)` pretty rendering, derived on PostgreSQL 18, not hand-written.
        definition: "CHECK (release_version IS NULL AND manifest_digest IS NULL OR release_version IS NOT NULL AND manifest_digest IS NOT NULL AND release_version > 0 AND manifest_digest ~ '^sha256:[0-9a-f]{64}$'::text)",
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
        name: "node_runs_error_kind_check",
        definition: "CHECK (error_kind = ANY (ARRAY['retryable'::text, 'rate-limited'::text, 'terminal'::text, 'invalid-input'::text]))",
        origin: CheckOrigin::Inline("error_kind"),
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_frame_check",
        definition: "CHECK (frame_id >= 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_frame_relation_check",
        definition: "CHECK (frame_id = 0 AND parent_frame_id IS NULL AND call_site_id IS NULL OR frame_id > 0 AND parent_frame_id IS NOT NULL AND parent_frame_id >= 0 AND parent_frame_id < frame_id AND call_site_id IS NOT NULL AND call_site_id ~ '^[a-z0-9-]+$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_plan_hash_check",
        definition: "CHECK (current_plan_hash ~ '^sha256:[0-9a-f]{64}$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "node_runs",
        name: "node_runs_local_node_check",
        definition: "CHECK (local_node_id ~ '^[a-z0-9-]+$'::text)",
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
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_report_id_check",
        definition: "CHECK (report_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_command_hash_check",
        definition: "CHECK (command_hash ~ '^sha256:[0-9a-f]{64}$'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_validated_draft_id_check",
        definition: "CHECK (validated_draft_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_catalog_id_check",
        definition: "CHECK (catalog_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_catalog_version_check",
        definition: "CHECK (catalog_version > 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_case_count_check",
        definition: "CHECK (case_count >= 1 AND case_count <= 256)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_state_check",
        definition: "CHECK (state = ANY (ARRAY['pending'::text, 'finalized'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_check",
        definition: "CHECK (whole_deadline_at > created_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_run_reservations",
        name: "authoring_test_run_reservations_check1",
        definition: "CHECK (state = 'pending'::text AND finalized_at IS NULL OR state = 'finalized'::text AND finalized_at IS NOT NULL AND finalized_at >= created_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_report_id_check",
        definition: "CHECK (report_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_ordinal_check",
        definition: "CHECK (ordinal >= 0 AND ordinal <= 255)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_case_id_check",
        definition: "CHECK (case_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_run_id_check",
        definition: "CHECK (run_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_catalog_id_check",
        definition: "CHECK (catalog_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_catalog_version_check",
        definition: "CHECK (catalog_version > 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_validated_draft_id_check",
        definition: "CHECK (validated_draft_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_state_check",
        definition: "CHECK (state = ANY (ARRAY['pending'::text, 'finalized'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_failure_kind_check",
        definition: "CHECK (failure_kind = ANY (ARRAY['assertion-failed'::text, 'deadline-exhausted'::text, 'effect-uncertain'::text]))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_summary_check",
        definition: "CHECK (summary IS NULL OR jsonb_typeof(summary) = 'object'::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_check",
        definition: "CHECK (case_deadline_at > created_at)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_case_runs",
        name: "authoring_test_case_runs_check1",
        definition: "CHECK (state = 'pending'::text AND passed IS NULL AND failure_kind IS NULL AND summary IS NULL AND finalized_at IS NULL OR state = 'finalized'::text AND passed IS NOT NULL AND summary IS NOT NULL AND finalized_at IS NOT NULL AND finalized_at >= created_at AND (passed AND failure_kind IS NULL OR NOT passed AND failure_kind IS NOT NULL))",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_reports",
        name: "authoring_test_reports_tenant_id_check",
        definition: "CHECK (tenant_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_reports",
        name: "authoring_test_reports_report_id_check",
        definition: "CHECK (report_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_reports",
        name: "authoring_test_reports_validated_draft_id_check",
        definition: "CHECK (validated_draft_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_reports",
        name: "authoring_test_reports_catalog_id_check",
        definition: "CHECK (catalog_id <> ''::text)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_reports",
        name: "authoring_test_reports_catalog_version_check",
        definition: "CHECK (catalog_version > 0)",
        origin: CheckOrigin::Table,
    },
    CheckSpec {
        table: "authoring_test_reports",
        name: "authoring_test_reports_summary_check",
        definition: "CHECK (jsonb_typeof(summary) = 'object'::text)",
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

const LOCK_CATALOG_HEAD_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.lock_catalog_head(p_tenant_id text, p_catalog_id text, p_environment text)\n RETURNS integer\n LANGUAGE plpgsql\n SECURITY DEFINER\n SET search_path TO 'pg_catalog', 'catalog'\nAS $function$\nDECLARE\n    applied_version int;\nBEGIN\n    SELECT head.applied_catalog_version INTO applied_version\n    FROM catalog.catalog_heads AS head\n    WHERE p_tenant_id = NULLIF(current_setting('app.tenant', true), '')\n      AND head.tenant_id = p_tenant_id\n      AND head.catalog_id = p_catalog_id\n      AND head.environment = p_environment\n    FOR SHARE OF head;\n    RETURN applied_version;\nEND\n$function$\n";

const PIN_RUN_DURABILITY_CLASS_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.pin_run_durability_class()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nDECLARE\n    projected_environment text;\n    projected_class text;\nBEGIN\n    SELECT policy.expected_environment, policy.durability_class\n      INTO projected_environment, projected_class\n      FROM wamn_run.environment_policies AS policy\n     WHERE policy.tenant_id = NEW.tenant_id;\n    IF NOT FOUND THEN\n        RAISE EXCEPTION USING\n            ERRCODE = '55000',\n            MESSAGE = 'environment-policy-not-converged';\n    END IF;\n    IF NEW.environment IS DISTINCT FROM projected_environment THEN\n        RAISE EXCEPTION USING\n            ERRCODE = '55000',\n            MESSAGE = 'environment-policy-environment-mismatch';\n    END IF;\n    NEW.durability_class := projected_class;\n    RETURN NEW;\nEND\n$function$\n";

const GUARD_EVENT_LINEAGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_event_lineage_immutable()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF NEW.event_source_run_id IS DISTINCT FROM OLD.event_source_run_id\n       OR NEW.event_root_run_id IS DISTINCT FROM OLD.event_root_run_id\n       OR NEW.event_depth IS DISTINCT FROM OLD.event_depth THEN\n        RAISE EXCEPTION 'event causation lineage is immutable';\n    END IF;\n    RETURN NEW;\nEND\n$function$\n";

const GUARD_RUN_ADMISSION_PINS_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_run_admission_pins_immutable()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF NEW.catalog_id IS DISTINCT FROM OLD.catalog_id\n       OR NEW.catalog_version IS DISTINCT FROM OLD.catalog_version\n       OR NEW.environment IS DISTINCT FROM OLD.environment\n       OR NEW.execution_bundle_hash IS DISTINCT FROM OLD.execution_bundle_hash\n       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode\n       OR NEW.durability_class IS DISTINCT FROM OLD.durability_class THEN\n        RAISE EXCEPTION USING\n            ERRCODE = '55000',\n            MESSAGE = 'run-admission-pin-immutable';\n    END IF;\n    IF OLD.release_version IS NOT NULL OR OLD.manifest_digest IS NOT NULL THEN\n        IF NEW.release_version IS NULL AND NEW.manifest_digest IS NULL THEN\n            IF NEW.status NOT IN ('dispatched', 'running')\n               OR EXISTS (SELECT 1 FROM wamn_run.effect_attempts AS effect\n                           WHERE effect.tenant_id = OLD.tenant_id\n                             AND effect.run_id = OLD.run_id\n                             AND OLD.durability_class = 'durable') THEN\n                RAISE EXCEPTION USING\n                    ERRCODE = '55000',\n                    MESSAGE = 'run-release-record-immutable';\n            END IF;\n        ELSIF NEW.release_version IS DISTINCT FROM OLD.release_version\n           OR NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest THEN\n            RAISE EXCEPTION USING\n                ERRCODE = '55000',\n                MESSAGE = 'run-release-record-immutable';\n        END IF;\n    END IF;\n    RETURN NEW;\nEND\n$function$\n";

const GUARD_TERMINAL_RUN_DELETE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_terminal_run_delete()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF OLD.status NOT IN ('completed', 'failed', 'infrastructure-failure') THEN\n        RAISE EXCEPTION USING\n            ERRCODE = '55000',\n            MESSAGE = 'run-delete-nonterminal';\n    END IF;\n    RETURN OLD;\nEND\n$function$\n";

const REJECT_IMMUTABLE_EFFECT_FACT_CHANGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_effect_fact_change()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    RAISE EXCEPTION USING\n        ERRCODE = '55000',\n        MESSAGE = 'effect-fact-immutable';\nEND\n$function$\n";

const REJECT_IMMUTABLE_OPERATOR_RUN_ACTION_CHANGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.reject_immutable_operator_run_action_change()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    RAISE EXCEPTION USING\n        ERRCODE = '55000',\n        MESSAGE = 'operator-run-action-immutable';\nEND\n$function$\n";

const RUNS_EVENT_LINEAGE_TRIGGER_DEF: &str = "CREATE TRIGGER runs_event_lineage_immutable BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_event_lineage_immutable()";
const RUNS_PIN_DURABILITY_CLASS_TRIGGER_DEF: &str = "CREATE TRIGGER runs_pin_durability_class BEFORE INSERT ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.pin_run_durability_class()";
const RUNS_TERMINAL_DELETE_ONLY_TRIGGER_DEF: &str = "CREATE TRIGGER runs_terminal_delete_only BEFORE DELETE ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION wamn_run.guard_terminal_run_delete()";

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

const PIN_RUN_DURABILITY_CLASS_SQL: &str = r#"CREATE OR REPLACE FUNCTION wamn_run.pin_run_durability_class()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    projected_environment text;
    projected_class text;
BEGIN
    SELECT policy.expected_environment, policy.durability_class
      INTO projected_environment, projected_class
      FROM wamn_run.environment_policies AS policy
     WHERE policy.tenant_id = NEW.tenant_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'environment-policy-not-converged';
    END IF;
    IF NEW.environment IS DISTINCT FROM projected_environment THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'environment-policy-environment-mismatch';
    END IF;
    NEW.durability_class := projected_class;
    RETURN NEW;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.pin_run_durability_class() FROM PUBLIC;"#;

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
    IF NEW.catalog_id IS DISTINCT FROM OLD.catalog_id
       OR NEW.catalog_version IS DISTINCT FROM OLD.catalog_version
       OR NEW.environment IS DISTINCT FROM OLD.environment
       OR NEW.execution_bundle_hash IS DISTINCT FROM OLD.execution_bundle_hash
       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode
       OR NEW.durability_class IS DISTINCT FROM OLD.durability_class THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'run-admission-pin-immutable';
    END IF;
    IF OLD.release_version IS NOT NULL OR OLD.manifest_digest IS NOT NULL THEN
        IF NEW.release_version IS NULL AND NEW.manifest_digest IS NULL THEN
            IF NEW.status NOT IN ('dispatched', 'running')
               OR EXISTS (SELECT 1 FROM wamn_run.effect_attempts AS effect
                           WHERE effect.tenant_id = OLD.tenant_id
                             AND effect.run_id = OLD.run_id
                             AND OLD.durability_class = 'durable') THEN
                RAISE EXCEPTION USING
                    ERRCODE = '55000',
                    MESSAGE = 'run-release-record-immutable';
            END IF;
        ELSIF NEW.release_version IS DISTINCT FROM OLD.release_version
           OR NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest THEN
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
const RUNS_PIN_DURABILITY_CLASS_TRIGGER_SQL: &str = "CREATE TRIGGER \
    runs_pin_durability_class BEFORE INSERT ON wamn_run.runs FOR EACH ROW \
    EXECUTE FUNCTION wamn_run.pin_run_durability_class();";
const RUNS_TERMINAL_DELETE_ONLY_TRIGGER_SQL: &str = "CREATE TRIGGER \
    runs_terminal_delete_only BEFORE DELETE ON wamn_run.runs FOR EACH ROW \
    EXECUTE FUNCTION wamn_run.guard_terminal_run_delete();";

/// The ONE encoding of the admission-pin trigger's `CREATE` (wamn-0h0g.20.9).
///
/// Both the steady-state trigger repair and the legacy execution-pin cutover
/// emit this. They used to carry independent copies, and the cutover's copy
/// never grew the `durability_class` arm wamn-0h0g.20.1 added here — so the
/// cutover silently dropped that column's enforcement and left the reconciler
/// repairing what the cutover had just clobbered.
const RUNS_ADMISSION_PINS_TRIGGER_SQL: &str = "CREATE TRIGGER runs_admission_pins_immutable \
    BEFORE UPDATE OF catalog_id, catalog_version, environment, execution_bundle_hash, \
    capture_mode, durability_class, release_version, manifest_digest \
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

fn authoring_test_helper_sql(name: &str) -> &'static str {
    let head = format!("CREATE OR REPLACE FUNCTION wamn_run.{name}()");
    let start = AUTHORING_TESTS_SQL
        .find(&head)
        .unwrap_or_else(|| panic!("authoring-test record must define {name}"));
    let tail = &AUTHORING_TESTS_SQL[start..];
    let function_end = tail
        .find("\n$$;")
        .map(|offset| offset + "\n$$;".len())
        .unwrap_or_else(|| panic!("authoring-test helper {name} must close its body"));
    let privileges = &tail[function_end..];
    let revoke_end = privileges
        .find(';')
        .map(|offset| function_end + offset + 1)
        .unwrap_or_else(|| panic!("authoring-test helper {name} must revoke PUBLIC"));
    &tail[..revoke_end]
}

fn authoring_test_helper_definition(name: &str) -> String {
    let sql = authoring_test_helper_sql(name);
    let (header, body_and_privileges) = sql
        .split_once("\nAS $$\n")
        .unwrap_or_else(|| panic!("authoring-test helper {name} must use its canonical delimiter"));
    let body = body_and_privileges
        .split_once("\n$$;")
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("authoring-test helper {name} must close its canonical body"));
    let signature = header
        .lines()
        .next()
        .expect("authoring-test helper signature is non-empty");
    format!("{signature}\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\n{body}\n$function$\n")
}

fn helper_specs() -> Vec<HelperSpec> {
    vec![
        borrowed_helper_spec(
            "pin_run_durability_class",
            PIN_RUN_DURABILITY_CLASS_DEF,
            PIN_RUN_DURABILITY_CLASS_SQL,
        ),
        borrowed_helper_spec(
            "lock_catalog_head",
            LOCK_CATALOG_HEAD_DEF,
            LOCK_CATALOG_HEAD_SQL,
        ),
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
        HelperSpec {
            name: "reject_immutable_authoring_test_orchestration_change",
            definition: Cow::Owned(authoring_test_helper_definition(
                "reject_immutable_authoring_test_orchestration_change",
            )),
            sql: Cow::Borrowed(authoring_test_helper_sql(
                "reject_immutable_authoring_test_orchestration_change",
            )),
        },
        HelperSpec {
            name: "guard_authoring_test_orchestration_write",
            definition: Cow::Owned(authoring_test_helper_definition(
                "guard_authoring_test_orchestration_write",
            )),
            sql: Cow::Borrowed(authoring_test_helper_sql(
                "guard_authoring_test_orchestration_write",
            )),
        },
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
            name: "runs_pin_durability_class".to_string(),
            definition: RUNS_PIN_DURABILITY_CLASS_TRIGGER_DEF.to_string(),
            sql: RUNS_PIN_DURABILITY_CLASS_TRIGGER_SQL.to_string(),
        },
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
    for (table, name, event, function) in [
        (
            "authoring_test_run_reservations",
            "authoring_test_run_reservations_controlled_insert",
            "INSERT",
            "guard_authoring_test_orchestration_write",
        ),
        (
            "authoring_test_run_reservations",
            "authoring_test_run_reservations_controlled_update",
            "UPDATE",
            "guard_authoring_test_orchestration_write",
        ),
        (
            "authoring_test_run_reservations",
            "authoring_test_run_reservations_delete_immutable",
            "DELETE",
            "reject_immutable_authoring_test_orchestration_change",
        ),
        (
            "authoring_test_case_runs",
            "authoring_test_case_runs_controlled_insert",
            "INSERT",
            "guard_authoring_test_orchestration_write",
        ),
        (
            "authoring_test_case_runs",
            "authoring_test_case_runs_controlled_update",
            "UPDATE",
            "guard_authoring_test_orchestration_write",
        ),
        (
            "authoring_test_case_runs",
            "authoring_test_case_runs_delete_immutable",
            "DELETE",
            "reject_immutable_authoring_test_orchestration_change",
        ),
        (
            "authoring_test_reports",
            "authoring_test_reports_controlled_insert",
            "INSERT",
            "guard_authoring_test_orchestration_write",
        ),
        (
            "authoring_test_reports",
            "authoring_test_reports_update_immutable",
            "UPDATE",
            "reject_immutable_authoring_test_orchestration_change",
        ),
        (
            "authoring_test_reports",
            "authoring_test_reports_delete_immutable",
            "DELETE",
            "reject_immutable_authoring_test_orchestration_change",
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
const TEST_CASE_RESERVATION_FK_NAME: &str = "authoring_test_case_reservation_fk";
const TEST_CASE_RESERVATION_FK_DEF: &str = "FOREIGN KEY (tenant_id, report_id, catalog_id, catalog_version, validated_draft_id) REFERENCES wamn_run.authoring_test_run_reservations(tenant_id, report_id, catalog_id, catalog_version, validated_draft_id)";
const TEST_CASE_RESERVATION_FK_SQL: &str = "ALTER TABLE wamn_run.authoring_test_case_runs \
     ADD CONSTRAINT authoring_test_case_reservation_fk \
     FOREIGN KEY (tenant_id, report_id, catalog_id, catalog_version, validated_draft_id) \
     REFERENCES wamn_run.authoring_test_run_reservations \
         (tenant_id, report_id, catalog_id, catalog_version, validated_draft_id)";
const TEST_REPORT_RESERVATION_FK_NAME: &str = "authoring_test_report_reservation_fk";
const TEST_REPORT_RESERVATION_FK_DEF: &str = "FOREIGN KEY (tenant_id, report_id) REFERENCES wamn_run.authoring_test_run_reservations(tenant_id, report_id)";
const TEST_REPORT_RESERVATION_FK_SQL: &str = "ALTER TABLE wamn_run.authoring_test_reports \
     ADD CONSTRAINT authoring_test_report_reservation_fk \
     FOREIGN KEY (tenant_id, report_id) \
     REFERENCES wamn_run.authoring_test_run_reservations (tenant_id, report_id)";

const FLOW_AUTHOR_CHECK_NAME: &str = "flow_artifacts_verified_author_principal_check";
const FLOW_AUTHOR_CHECK_DEF: &str =
    "CHECK (verified_author_principal IS NULL OR verified_author_principal <> ''::text)";
const RELEASE_PUBLISHER_CHECK_NAME: &str = "release_manifests_verified_publisher_principal_check";
const RELEASE_PUBLISHER_CHECK_DEF: &str =
    "CHECK (verified_publisher_principal IS NULL OR verified_publisher_principal <> ''::text)";
const AUTHORING_COMMAND_KIND_CHECK_NAME: &str = "authoring_command_audit_command_kind_check";
const AUTHORING_COMMAND_KIND_CHECK_DEF: &str = "CHECK (command_kind = ANY (ARRAY['save-flow-draft'::text, 'validate'::text, 'draft-run'::text, 'test-set-run'::text, 'publish'::text]))";
const AUTHORING_COMMAND_REQUEST_HASH_CHECK_NAME: &str =
    "authoring_command_audit_request_hash_check";
const AUTHORING_COMMAND_REQUEST_HASH_CHECK_DEF: &str =
    "CHECK (request_hash ~ '^sha256:[0-9a-f]{64}$'::text)";
const AUTHORING_COMMAND_OUTCOME_PRESENT_CHECK_NAME: &str =
    "authoring_command_audit_outcome_present";
const AUTHORING_COMMAND_OUTCOME_PRESENT_CHECK_DEF: &str = "CHECK (octet_length(outcome_bytes) > 0)";
const AUTHORING_COMMAND_PRIMARY_INDEX_DEF: &str = "CREATE UNIQUE INDEX authoring_command_audit_pkey ON catalog.authoring_command_audit USING btree (tenant_id, principal_id, command_id)";
const AUTHORING_COMMAND_AUDIT_ID_INDEX_DEF: &str = "CREATE UNIQUE INDEX authoring_command_audit_audit_id_key ON catalog.authoring_command_audit USING btree (tenant_id, audit_id)";

const RETIRED_NODE_ATTEMPT_COLUMNS: &[&str] = &[
    "current_effect_attempt_id",
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

const RETIRED_EFFECT_ATTEMPT_COLUMNS: &[&str] = &[
    "attempt_key",
    "attempt_index",
    "predecessor_attempt_id",
    "legacy_imported",
    "selected_recovery_class",
    "recovery_class",
];

const NODE_FRAME_COLUMNS: &[&str] = &[
    "frame_id",
    "parent_frame_id",
    "call_site_id",
    "current_plan_hash",
    "local_node_id",
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
const NODE_RUNS_PKEY_DEF: &str = "CREATE UNIQUE INDEX node_runs_pkey ON \
wamn_run.node_runs USING btree (tenant_id, run_id, frame_id, local_node_id, occurrence)";

const NODE_FRAME_CHECKS: &[&str] = &[
    "node_runs_frame_check",
    "node_runs_frame_relation_check",
    "node_runs_plan_hash_check",
    "node_runs_local_node_check",
];

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

fn retired_node_attempt_check(definition: &str) -> bool {
    let tokens = ident_tokens(definition);
    RETIRED_NODE_ATTEMPT_COLUMNS
        .iter()
        .any(|column| tokens.contains(*column))
}

fn frame_identity_column(table: &str, column: &str) -> bool {
    (table == "node_runs" && NODE_FRAME_COLUMNS.contains(&column))
        || (table == "effect_attempts" && EFFECT_FRAME_COLUMNS.contains(&column))
        || (matches!(table, "node_runs" | "effect_attempts") && column == "node_id")
}

fn frame_identity_check(table: &str, name: &str) -> bool {
    (table == "node_runs" && NODE_FRAME_CHECKS.contains(&name))
        || (table == "effect_attempts" && EFFECT_FRAME_CHECKS.contains(&name))
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

fn node_frame_contract_complete(obs: &RunPlaneObservation, schema: &BareSchemaName) -> bool {
    let Some(columns) = obs.tables.get("node_runs") else {
        return true;
    };
    !columns.contains("node_id")
        && column_contract_complete(obs, "node_runs", "frame_id", "bigint", true)
        && column_contract_complete(obs, "node_runs", "parent_frame_id", "bigint", false)
        && column_contract_complete(obs, "node_runs", "call_site_id", "text", false)
        && column_contract_complete(obs, "node_runs", "current_plan_hash", "text", true)
        && column_contract_complete(obs, "node_runs", "local_node_id", "text", true)
        && check_contract_complete(obs, "node_runs", NODE_FRAME_CHECKS)
        && obs.indexes.get("node_runs_pkey").is_some_and(|definition| {
            normalize_observed_schema(definition, schema) == NODE_RUNS_PKEY_DEF
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
    if name.len() <= wamn_schema_model::MAX_IDENTIFIER_BYTES {
        return name;
    }
    let mut end = wamn_schema_model::MAX_IDENTIFIER_BYTES;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameIdentityCutoverTargets {
    node: bool,
    effect: bool,
    dispatch: bool,
    restore_dispatch_fk: bool,
}

impl FrameIdentityCutoverTargets {
    const fn needed(self) -> bool {
        self.node || self.effect
    }

    fn includes_table(self, table: &str) -> bool {
        matches!(
            (table, self.node, self.effect),
            ("node_runs", true, _) | ("effect_attempts", _, true)
        )
    }
}

fn frame_identity_cutover_targets(
    obs: &RunPlaneObservation,
    schema: &BareSchemaName,
) -> FrameIdentityCutoverTargets {
    let effect = !effect_frame_contract_complete(obs, schema);
    let dispatch = effect && obs.tables.contains_key("effect_attempt_dispatches");
    FrameIdentityCutoverTargets {
        node: !node_frame_contract_complete(obs, schema),
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
    if targets.node {
        sql.push_str(&format!(
            "LOCK TABLE {schema}.node_runs IN ACCESS EXCLUSIVE MODE;\n"
        ));
        populated.push(format!("EXISTS (SELECT 1 FROM {schema}.node_runs)"));
    }
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
            MESSAGE = 'frame-identity-cutover-requires-empty-node-and-effect-facts';
    END IF;
END
$frame_identity_cutover$;
"#,
        populated.join(" OR ")
    ));
    if targets.node {
        sql.push_str(&format!(
            r#"ALTER TABLE {schema}.node_runs
    DROP CONSTRAINT IF EXISTS node_runs_pkey,
    DROP CONSTRAINT IF EXISTS node_runs_frame_check,
    DROP CONSTRAINT IF EXISTS node_runs_frame_relation_check,
    DROP CONSTRAINT IF EXISTS node_runs_plan_hash_check,
    DROP CONSTRAINT IF EXISTS node_runs_local_node_check;
ALTER TABLE {schema}.node_runs
    DROP COLUMN IF EXISTS node_id,
    DROP COLUMN IF EXISTS frame_id,
    DROP COLUMN IF EXISTS parent_frame_id,
    DROP COLUMN IF EXISTS call_site_id,
    DROP COLUMN IF EXISTS current_plan_hash,
    DROP COLUMN IF EXISTS local_node_id;
ALTER TABLE {schema}.node_runs
    ADD COLUMN frame_id bigint NOT NULL DEFAULT 0,
    ADD COLUMN parent_frame_id bigint,
    ADD COLUMN call_site_id text,
    ADD COLUMN current_plan_hash text NOT NULL,
    ADD COLUMN local_node_id text NOT NULL,
    ADD CONSTRAINT node_runs_frame_check CHECK (frame_id >= 0),
    ADD CONSTRAINT node_runs_frame_relation_check CHECK (
        (frame_id = 0 AND parent_frame_id IS NULL AND call_site_id IS NULL)
        OR (frame_id > 0 AND parent_frame_id IS NOT NULL AND parent_frame_id >= 0
            AND parent_frame_id < frame_id AND call_site_id IS NOT NULL
            AND call_site_id ~ '^[a-z0-9-]+$')
    ),
    ADD CONSTRAINT node_runs_plan_hash_check CHECK (current_plan_hash ~ '^sha256:[0-9a-f]{{64}}$'),
    ADD CONSTRAINT node_runs_local_node_check CHECK (local_node_id ~ '^[a-z0-9-]+$'),
    ADD PRIMARY KEY (tenant_id, run_id, frame_id, local_node_id, occurrence);
"#
        ));
    }
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
    let retired_node_columns: Vec<&str> = obs
        .tables
        .get("node_runs")
        .into_iter()
        .flat_map(|columns| {
            RETIRED_NODE_ATTEMPT_COLUMNS
                .iter()
                .copied()
                .filter(|column| columns.contains(*column))
        })
        .collect();
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
    let mut locked_tables = Vec::new();
    if !retired_node_columns.is_empty() {
        locked_tables.push("node_runs");
    }
    locked_tables.extend(present_ledgers.iter().copied());
    let mut sql = locked_tables
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

    if !retired_node_columns.is_empty() {
        sql.push_str(&format!(
            r#"ALTER TABLE {schema}.node_runs
    DROP CONSTRAINT IF EXISTS node_runs_current_effect_attempt_fk,
    DROP CONSTRAINT IF EXISTS node_runs_selected_recovery_class_check,
    DROP CONSTRAINT IF EXISTS node_runs_recovery_class_check,
    DROP CONSTRAINT IF EXISTS node_runs_generation_fact_kind_check,
    DROP CONSTRAINT IF EXISTS node_runs_check,
    DROP CONSTRAINT IF EXISTS node_runs_check1,
    DROP CONSTRAINT IF EXISTS node_runs_check2,
    DROP CONSTRAINT IF EXISTS node_runs_check3;
"#,
        ));
        let drops = retired_node_columns
            .iter()
            .map(|column| format!("DROP COLUMN {}", quote_ident(column)))
            .collect::<Vec<_>>()
            .join(",\n    ");
        sql.push_str(&format!("ALTER TABLE {schema}.node_runs\n    {drops};\n"));
    }

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

fn retired_node_attempt_columns_present(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("node_runs").is_some_and(|columns| {
        RETIRED_NODE_ATTEMPT_COLUMNS
            .iter()
            .any(|column| columns.contains(*column))
    })
}

fn effect_writer_cutover_needed(schema: &BareSchemaName, obs: &RunPlaneObservation) -> bool {
    effect_writer_ledger_cutover_needed(schema, obs) || retired_node_attempt_columns_present(obs)
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
/// `runs`, which everything FKs), then authoring-test persistence, and finally
/// the queue.
const RUN_PLANE_FILES: [&str; 3] = [RUN_STATE_SQL, AUTHORING_TESTS_SQL, RUN_QUEUE_SQL];

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
        // wamn-0h0g.12.20 confined this relation to SELECT. This spec is the
        // SIXTH encoding of the old blanket grant and the only one the run-plane
        // reconciler owns: left at full DML it planned RepairAuthoringPrivilege
        // forever -- never converging -- and each repair re-granted the very DML
        // the DDL and the publish converge path had just revoked.
        app: &["SELECT"],
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
        table: "execution_bundles",
        app: &["SELECT"],
        author: &["SELECT", "INSERT"],
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
        author: &["SELECT"],
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
    // `runs` has a dedicated column-grant reconciler because capture_mode is
    // admission-owned while the remaining run columns retain app writes.
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "environment_policies",
        app: &["SELECT"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "runs",
        app: &["SELECT", "DELETE"],
        author: &["SELECT"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "authoring_test_run_reservations",
        app: &[],
        author: &["SELECT", "INSERT", "UPDATE"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "authoring_test_case_runs",
        app: &[],
        author: &["SELECT", "INSERT", "UPDATE"],
    },
    AuthoringPrivilegeSpec {
        schema: AuthoringTableSchema::RunPlane,
        table: "authoring_test_reports",
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

fn authoring_retry_ledger_ready(obs: &RunPlaneObservation) -> bool {
    let Some(columns) = obs.catalog_columns.get("authoring_command_audit") else {
        return false;
    };
    for (column, column_type) in [("request_hash", "text"), ("outcome_bytes", "bytea")] {
        let key = ("authoring_command_audit".to_string(), column.to_string());
        if !columns.contains(column)
            || !obs.catalog_non_nullable_columns.contains(&key)
            || obs
                .catalog_column_types
                .get(&key)
                .is_none_or(|actual| actual != column_type)
        {
            return false;
        }
    }
    obs.catalog_checks
        .get(&(
            "authoring_command_audit".to_string(),
            AUTHORING_COMMAND_KIND_CHECK_NAME.to_string(),
        ))
        .is_some_and(|definition| definition == AUTHORING_COMMAND_KIND_CHECK_DEF)
        && obs
            .catalog_checks
            .get(&(
                "authoring_command_audit".to_string(),
                AUTHORING_COMMAND_REQUEST_HASH_CHECK_NAME.to_string(),
            ))
            .is_some_and(|definition| definition == AUTHORING_COMMAND_REQUEST_HASH_CHECK_DEF)
        && obs
            .catalog_checks
            .get(&(
                "authoring_command_audit".to_string(),
                AUTHORING_COMMAND_OUTCOME_PRESENT_CHECK_NAME.to_string(),
            ))
            .is_some_and(|definition| definition == AUTHORING_COMMAND_OUTCOME_PRESENT_CHECK_DEF)
        && obs
            .catalog_indexes
            .get("authoring_command_audit_pkey")
            .is_some_and(|definition| definition == AUTHORING_COMMAND_PRIMARY_INDEX_DEF)
        && obs
            .catalog_indexes
            .get("authoring_command_audit_audit_id_key")
            .is_some_and(|definition| definition == AUTHORING_COMMAND_AUDIT_ID_INDEX_DEF)
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
        || obs
            .catalog_columns
            .get("validated_flow_drafts")
            .is_some_and(|columns| columns.contains("suite_flow_version"))
        || (obs.catalog_tables.contains("authoring_command_audit")
            && !authoring_retry_ledger_ready(obs))
}

fn stored_suite_cutover_sql(schema: &BareSchemaName, obs: &RunPlaneObservation) -> String {
    let validation_dimension_present = obs
        .catalog_columns
        .get("validated_flow_drafts")
        .is_some_and(|columns| columns.contains("suite_flow_version"));
    let audit_retry_drifted = obs.catalog_tables.contains("authoring_command_audit")
        && !authoring_retry_ledger_ready(obs);
    let mut statements = Vec::new();
    let mut lock_targets = Vec::new();
    if validation_dimension_present {
        lock_targets.push("catalog.validated_flow_drafts");
    }
    if audit_retry_drifted {
        lock_targets.push("catalog.authoring_command_audit");
    }
    if !lock_targets.is_empty() {
        statements.push(format!(
            "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
            lock_targets.join(", ")
        ));
    }
    if validation_dimension_present {
        statements.push(
            "DO $retired_validation_dimension$ BEGIN \
               IF EXISTS ( \
                    SELECT 1 FROM pg_catalog.pg_attribute \
                     WHERE attrelid = 'catalog.validated_flow_drafts'::regclass \
                       AND attname = 'suite_flow_version' AND NOT attisdropped) \
                  AND EXISTS (SELECT 1 FROM catalog.validated_flow_drafts) \
               THEN RAISE EXCEPTION USING ERRCODE = '55000', \
                    MESSAGE = 'retired-validation-identity-requires-reprovision'; \
               END IF; \
             END $retired_validation_dimension$"
                .to_string(),
        );
        statements.push(
            "ALTER TABLE catalog.validated_flow_drafts \
               DROP CONSTRAINT IF EXISTS validated_flow_drafts_exact_pin, \
               DROP COLUMN IF EXISTS suite_flow_version, \
               ADD CONSTRAINT validated_flow_drafts_exact_pin UNIQUE ( \
                   tenant_id, draft_id, draft_revision, draft_content_hash, \
                   catalog_id, catalog_version, environment, runtime_flow_version, \
                   draft_artifact_hash, execution_bundle_hash, \
                   binding_base_artifact_hash)"
                .to_string(),
        );
    }
    if audit_retry_drifted {
        statements.push(
            "DO $authoring_retry_ledger_cutover$ BEGIN \
               IF EXISTS (SELECT 1 FROM catalog.authoring_command_audit) \
               THEN RAISE EXCEPTION USING ERRCODE = '55000', \
                    MESSAGE = 'authoring-command-retry-ledger-cutover-requires-empty-audit-or-archive-and-reprovision'; \
               END IF; \
             END $authoring_retry_ledger_cutover$"
                .to_string(),
        );
        statements.push(
            "ALTER TABLE catalog.authoring_command_audit \
               DROP CONSTRAINT IF EXISTS authoring_command_audit_pkey, \
               DROP CONSTRAINT IF EXISTS authoring_command_audit_audit_id_key, \
               DROP CONSTRAINT IF EXISTS authoring_command_audit_command_kind_check, \
               DROP CONSTRAINT IF EXISTS authoring_command_audit_request_hash_check, \
               DROP CONSTRAINT IF EXISTS authoring_command_audit_outcome_present, \
               DROP COLUMN IF EXISTS request_hash, \
               DROP COLUMN IF EXISTS outcome_bytes, \
               ADD COLUMN request_hash text NOT NULL, \
               ADD COLUMN outcome_bytes bytea NOT NULL, \
               ADD CONSTRAINT authoring_command_audit_pkey \
                   PRIMARY KEY (tenant_id, principal_id, command_id), \
               ADD CONSTRAINT authoring_command_audit_audit_id_key \
                   UNIQUE (tenant_id, audit_id), \
               ADD CONSTRAINT authoring_command_audit_command_kind_check \
               CHECK (command_kind IN ('save-flow-draft', 'validate', 'draft-run', \
                                       'test-set-run', 'publish')), \
               ADD CONSTRAINT authoring_command_audit_request_hash_check \
                   CHECK (request_hash ~ '^sha256:[0-9a-f]{64}$'), \
               ADD CONSTRAINT authoring_command_audit_outcome_present \
                   CHECK (octet_length(outcome_bytes) > 0)"
                .to_string(),
        );
    }
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

/// The constant trigger AND function name the retired wamn-schema-compiler outbox emission
/// used (`CREATE OR REPLACE TRIGGER wamn_outbox_event … EXECUTE FUNCTION
/// wamn_outbox_event()`, one trigger per entity table, the function unqualified
/// so it landed in the apply-time schema).
pub const OUTBOX_TRIGGER_NAME: &str = "wamn_outbox_event";

/// The non-login database authority held only by trusted scenario-host
/// credentials. The guest-visible `wamn_app` role is never a member.
pub const SCENARIO_AUTHOR_ROLE: &str = "wamn_scenario_author";
/// Stable NOLOGIN ACL role inherited only by scoped writer generations.
pub const EFFECT_WRITER_ROLE: &str = "wamn_effect_writer";
/// Stable NOLOGIN ACL role inherited by the same scoped writer generations.
pub const RUN_PROJECTION_WRITER_ROLE: &str = "wamn_run_projection_writer";
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

/// Provisioning-owned stable effect-writer role boundary observed read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectWriterRoleObservation {
    pub can_login: bool,
    pub is_superuser: bool,
    pub can_create_database: bool,
    pub can_create_role: bool,
    pub inherits_roles: bool,
    pub can_replicate: bool,
    pub bypasses_rls: bool,
    pub can_connect: bool,
    pub owns_objects: bool,
    pub membership_out_of_bounds: bool,
}

impl EffectWriterRoleObservation {
    fn is_acl_only(self) -> bool {
        !self.can_login
            && !self.is_superuser
            && !self.can_create_database
            && !self.can_create_role
            && !self.inherits_roles
            && !self.can_replicate
            && !self.bypasses_rls
            && !self.can_connect
            && !self.owns_objects
            && !self.membership_out_of_bounds
    }
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

/// One live PostgreSQL row-security policy, normalized from `pg_policy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowPolicyObservation {
    pub command: String,
    pub permissive: bool,
    pub roles: BTreeSet<String>,
    pub using_expression: Option<String>,
    pub check_expression: Option<String>,
}

/// The complete row-security apparatus on one observed relation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowSecurityObservation {
    pub enabled: bool,
    pub forced: bool,
    /// Every policy keyed by its PostgreSQL policy name.
    pub policies: BTreeMap<String, RowPolicyObservation>,
}

fn environment_policy_row_security_at_record() -> RowSecurityObservation {
    RowSecurityObservation {
        enabled: true,
        forced: true,
        policies: BTreeMap::from([(
            "environment_policies_tenant".to_string(),
            RowPolicyObservation {
                command: "select".to_string(),
                permissive: true,
                roles: BTreeSet::from(["PUBLIC".to_string()]),
                using_expression: Some(ENVIRONMENT_POLICY_TENANT_QUAL.to_string()),
                check_expression: None,
            },
        )]),
    }
}

/// What the driver observed live, scoped to ONE project-env schema (plus the
/// per-database `catalog` metadata schema). Everything here is a read — the
/// pure planner turns it into the action list.
#[derive(Debug, Clone, Default)]
pub struct RunPlaneObservation {
    /// Exact row counts for the two execution-pin cutover targets.
    pub run_rows: i64,
    pub release_flow_rows: i64,
    /// Total immutable rows across the three effect-writer ledgers that exist.
    /// Any nonzero value makes an incompatible structural cutover refuse.
    pub effect_ledger_rows: i64,
    /// Persisted flow graphs that still carry retired top-level ordering keys.
    pub retired_authored_ordering_rows: i64,
    /// Host-only scenario-author role attributes, or absent when the cluster
    /// has not yet provisioned the role.
    pub scenario_author_role: Option<ScenarioAuthorRoleObservation>,
    /// Stable writer role attributes, ownership, membership, and CONNECT.
    pub effect_writer_role: Option<EffectWriterRoleObservation>,
    /// Stable projection-writer role attributes, ownership, membership, and CONNECT.
    pub run_projection_writer_role: Option<EffectWriterRoleObservation>,
    /// Exact direct `(USAGE-without-PUBLIC, effective-CREATE)` schema boundary.
    pub effect_writer_schema_privileges: (bool, bool),
    /// Exact direct `(USAGE-without-PUBLIC, effective-CREATE)` projection boundary.
    pub run_projection_schema_privileges: (bool, bool),
    /// Direct `node_runs` grants keyed by grantee.
    pub node_runs_table_privileges: BTreeMap<String, BTreeSet<String>>,
    /// Direct `node_runs` column grants keyed by grantee.
    pub node_runs_column_privileges: BTreeMap<String, BTreeSet<String>>,
    /// Effective `node_runs` grants keyed by grantee.
    pub node_runs_effective_privileges: BTreeMap<String, BTreeSet<String>>,
    /// Roles whose effective projection read is inherited from `wamn_app`.
    pub node_runs_app_members: BTreeSet<String>,
    /// Effective `node_runs` column grants keyed by grantee.
    pub node_runs_effective_column_privileges: BTreeMap<String, BTreeSet<String>>,
    /// Owner of `node_runs`, because ownership can restore revoked authority.
    pub node_runs_owner: Option<String>,
    /// Direct ledger grants keyed by `(table, grantee)`.
    pub effect_ledger_table_privileges: BTreeMap<(String, String), BTreeSet<String>>,
    /// Effective ledger grants keyed by `(table, grantee)`.
    pub effect_ledger_effective_privileges: BTreeMap<(String, String), BTreeSet<String>>,
    /// Effective column grants keyed by `(table, grantee)`.
    pub effect_ledger_effective_column_privileges: BTreeMap<(String, String), BTreeSet<String>>,
    /// Ledger owners keyed by table.
    pub effect_ledger_owners: BTreeMap<String, String>,
    /// Effective table privileges held by the writer on its two run-authority
    /// read targets. The target state is empty: only column SELECT is allowed.
    pub effect_writer_run_table_privileges: BTreeMap<String, BTreeSet<String>>,
    /// Effective per-column privileges held by the writer on `runs` and
    /// `run_queue`, keyed by `(table, column)`.
    pub effect_writer_run_column_privileges: BTreeMap<(String, String), BTreeSet<String>>,
    /// Whether guest-visible `wamn_app` inherits the host-only author role.
    pub app_is_scenario_author_member: bool,
    /// Effective `wamn_app` authority on the run capture carrier. The first
    /// value is a table-level INSERT/UPDATE grant (which covers every column);
    /// the second is effective INSERT/UPDATE on `runs.capture_mode` itself;
    /// the third proves the live column grants MATCH the ratified sets
    /// ([`RUNS_APP_INSERT_COLUMNS`] / [`RUNS_APP_UPDATE_COLUMNS`]) exactly —
    /// not that the app holds INSERT+UPDATE on every non-capture column, which
    /// a correctly confined table can never satisfy (wamn-0h0g.12.40).
    pub app_run_capture_privileges: (bool, bool, bool),
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
    /// Whether the cluster-global dispatcher read principal exists at all
    /// (wamn-0h0g.12.123). The reconciler owns that role's IN-DATABASE surface
    /// but never the role itself — `provision-project-env` mints it, with a
    /// password this verb does not have — so an absent role is not drift.
    pub dispatch_reader_role_present: bool,
    /// DIRECT schema-level privileges held by the dispatcher read principal on
    /// the target schema. See
    /// [`select_dispatch_reader_schema_privileges_sql`] for why these are
    /// direct rather than effective.
    pub dispatch_reader_schema_privileges: BTreeSet<String>,
    /// DIRECT table-level privileges held by the dispatcher read principal,
    /// keyed by relation, over every relation the repair's blanket `REVOKE` can
    /// reach.
    pub dispatch_reader_table_privileges: BTreeMap<String, BTreeSet<String>>,
    /// EVERY ordinary table in the target schema → its live column names.
    /// Includes entity/floor tables (ignored by the planner) and retired
    /// outbox/stored-suite tables (planned for teardown).
    pub tables: BTreeMap<String, BTreeSet<String>>,
    /// ENABLE/FORCE flags and every policy on the projected env-policy table.
    /// `None` means the relation itself is absent.
    pub environment_policy_row_security: Option<RowSecurityObservation>,
    /// Live columns that still carry NOT NULL authority.
    pub non_nullable_columns: BTreeSet<(String, String)>,
    /// PostgreSQL-formatted live column types, keyed by `(table, column)`.
    pub column_types: BTreeMap<(String, String), String>,
    /// Live columns that still synthesize values through a default.
    pub defaulted_columns: BTreeSet<(String, String)>,
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
    /// Catalog columns that carry NOT NULL authority.
    pub catalog_non_nullable_columns: BTreeSet<(String, String)>,
    /// PostgreSQL-formatted catalog column types.
    pub catalog_column_types: BTreeMap<(String, String), String>,
    /// Catalog CHECKs owned by additive cross-plane migrations.
    pub catalog_checks: BTreeMap<(String, String), String>,
    /// Catalog indexes, foreign keys, and user triggers observed exactly.
    pub catalog_indexes: BTreeMap<String, String>,
    pub catalog_foreign_keys: BTreeMap<(String, String), String>,
    pub catalog_triggers: BTreeMap<(String, String), String>,
    /// Rows in `catalog.event_registrations` still carrying a retired `state`
    /// or `partition-key` key (0 when the table is absent).
    pub stale_registration_key_rows: i64,
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
    /// Canonical `pg_get_functiondef` output for retained helpers and the two
    /// retired stored-suite helpers, keyed by function name.
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
    /// Strict empty-only conversion from legacy node/effect identity to frames.
    FrameIdentityCutover,
    /// Preserve the output-size fact while removing retired node capture columns.
    CaptureProjectionCutover,
    /// Widen trusted global node-fact sequence storage from int4 to int8.
    WidenNodeRunSequence,
    /// Remove invocation-admission expiry and make the client key optional.
    InvocationAdmissionRetentionCutover,
    /// Delete the retired partition plane after a locked drain/evidence preflight.
    PartitionPlaneCutover,
    /// Delete retired durable child, wait, and invoke-depth run state.
    ChildRunCutover,
    /// Delete retired replay/root run lineage while preserving every run row.
    RerunLineageCutover,
    /// Delete retired stored-suite tables, audit relation, and helper functions.
    StoredSuiteCutover,
    /// Empty-only deletion of the retired effect-disposition request/outcome plane.
    RetiredEffectDispositionCutover,
    /// Strict empty-only installation of the coordinate-bound writer ledgers.
    EffectWriterCutover,
    /// Refuse a provisioning-owned stable writer role outside its frozen shape.
    VerifyEffectWriterRole,
    /// Refuse a projection-writer role outside its frozen ACL-only shape.
    VerifyRunProjectionWriterRole,
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
    /// Apply the whole `catalog-schema.sql` (the `catalog` schema is absent).
    EnsureCatalogSchema,
    /// Create a missing `catalog` table from its record section.
    CreateCatalogTable,
    /// Add the nullable verified author/publisher provenance columns required
    /// before immutable attempt writers activate.
    EnsureCatalogProvenance,
    /// Atomically install the release/run execution pins after an empty-only
    /// preflight under ACCESS EXCLUSIVE locks on both tables.
    ExecutionPinCutover,
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

const RETIRED_CAPTURE_PROJECTION_COLUMNS: &[&str] = &["preview_head", "capture_mode", "redacted"];
const LEGACY_OUTPUT_SIZE_COLUMN: &str = "payload_size";
const RETIRED_RERUN_LINEAGE_COLUMNS: &[&str] = &["replay_of", "root_run_id"];
const RETIRED_EFFECT_DISPOSITION_TABLES: [&str; 2] =
    ["effect_disposition_requests", "effect_dispositions"];
const RETIRED_EFFECT_DISPOSITION_HELPER: &str = "guard_effect_disposition_append";

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

fn capture_output_size_rename_needed(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("node_runs").is_some_and(|columns| {
        columns.contains(LEGACY_OUTPUT_SIZE_COLUMN) && !columns.contains("output_size")
    })
}

fn capture_output_size_conflict(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("node_runs").is_some_and(|columns| {
        columns.contains(LEGACY_OUTPUT_SIZE_COLUMN) && columns.contains("output_size")
    })
}

fn capture_projection_cutover_needed(obs: &RunPlaneObservation) -> bool {
    obs.tables.get("node_runs").is_some_and(|columns| {
        capture_output_size_rename_needed(obs)
            || capture_output_size_conflict(obs)
            || RETIRED_CAPTURE_PROJECTION_COLUMNS
                .iter()
                .any(|column| columns.contains(*column))
    })
}

fn capture_projection_cutover_sql(
    schema: &BareSchemaName,
    rename_output_size: bool,
    output_size_conflict: bool,
) -> String {
    let target = schema.quoted();
    let rename = if rename_output_size {
        format!(
            "ALTER TABLE {target}.node_runs \
               RENAME COLUMN {LEGACY_OUTPUT_SIZE_COLUMN} TO output_size; "
        )
    } else {
        String::new()
    };
    let refuse_conflict = if output_size_conflict {
        "DO $capture_projection$ BEGIN RAISE EXCEPTION USING \
         ERRCODE = '55000', MESSAGE = 'capture-output-size-columns-conflict'; \
         END $capture_projection$; "
    } else {
        ""
    };
    format!(
        "LOCK TABLE {target}.node_runs IN ACCESS EXCLUSIVE MODE; \
         {refuse_conflict}\
         {rename}\
         ALTER TABLE {target}.node_runs \
           DROP COLUMN IF EXISTS preview_head, \
           DROP COLUMN IF EXISTS capture_mode, \
           DROP COLUMN IF EXISTS redacted"
    )
}

impl RunPlanePlan {
    /// Whether there is anything to apply (a no-op reconcile is the expected
    /// steady state and worth reporting as such).
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }
}

fn run_execution_pin_contract_complete(schema: &BareSchemaName, obs: &RunPlaneObservation) -> bool {
    if !obs.tables.contains_key("runs") {
        return false;
    }

    let run_column = |name: &str, ty: &str| {
        let key = ("runs".to_string(), name.to_string());
        obs.tables
            .get("runs")
            .is_some_and(|columns| columns.contains(name))
            && obs.non_nullable_columns.contains(&key)
            && obs
                .column_types
                .get(&key)
                .is_some_and(|actual| actual == ty)
    };
    let run_object = |actual: Option<&String>, expected: &str| {
        actual.is_some_and(|definition| normalize_observed_schema(definition, schema) == expected)
    };

    run_column("catalog_id", "text")
        && run_column("catalog_version", "integer")
        && run_column("environment", "text")
        && run_column("execution_bundle_hash", "text")
        && obs
            .checks
            .get(&("runs".to_string(), "runs_check".to_string()))
            .is_some_and(|definition| definition == RUNS_EXECUTION_PINS_CHECK_DEF)
        && run_object(
            obs.foreign_keys
                .get(&("runs".to_string(), "runs_release_fk".to_string())),
            RUNS_RELEASE_FK_DEF,
        )
        && run_object(
            obs.foreign_keys
                .get(&("runs".to_string(), "runs_execution_bundle_fk".to_string())),
            RUNS_EXECUTION_BUNDLE_FK_DEF,
        )
        && run_object(obs.indexes.get("runs_release"), RUNS_RELEASE_INDEX_DEF)
        && run_object(
            obs.indexes.get("runs_execution_bundle"),
            RUNS_EXECUTION_BUNDLE_INDEX_DEF,
        )
}

fn release_flow_execution_pin_contract_complete(obs: &RunPlaneObservation) -> bool {
    if !obs.catalog_tables.contains("release_flows") {
        return false;
    }

    let key = (
        "release_flows".to_string(),
        "execution_bundle_hash".to_string(),
    );
    obs.catalog_columns
        .get("release_flows")
        .is_some_and(|columns| columns.contains("execution_bundle_hash"))
        && obs.catalog_non_nullable_columns.contains(&key)
        && obs
            .catalog_column_types
            .get(&key)
            .is_some_and(|actual| actual == "text")
        && obs
            .catalog_checks
            .get(&(
                "release_flows".to_string(),
                "release_flows_execution_bundle_hash_check".to_string(),
            ))
            .is_some_and(|definition| definition == RELEASE_FLOWS_BUNDLE_CHECK_DEF)
        && obs
            .catalog_foreign_keys
            .get(&(
                "release_flows".to_string(),
                "release_flows_execution_bundle_fk".to_string(),
            ))
            .is_some_and(|definition| definition == RELEASE_FLOWS_EXECUTION_BUNDLE_FK_DEF)
        && obs
            .catalog_indexes
            .get("release_flows_execution_bundle")
            .is_some_and(|definition| definition == RELEASE_FLOWS_EXECUTION_BUNDLE_INDEX_DEF)
}

fn partial_execution_pin_refusal_sql(
    schema: &BareSchemaName,
    has_runs: bool,
    has_release_flows: bool,
) -> String {
    debug_assert!(has_runs ^ has_release_flows);
    let (lock_target, populated) = if has_runs {
        let runs = format!("{}.runs", schema.quoted());
        (runs.clone(), format!("EXISTS (SELECT 1 FROM {runs})"))
    } else {
        (
            "catalog.release_flows".to_string(),
            "EXISTS (SELECT 1 FROM catalog.release_flows)".to_string(),
        )
    };
    format!(
        r#"LOCK TABLE {lock_target} IN ACCESS EXCLUSIVE MODE;
DO $execution_pin_preflight$
BEGIN
    IF {populated} THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'execution-pin-cutover-requires-empty-run-and-release-membership';
    END IF;
END
$execution_pin_preflight$;"#
    )
}

/// The schema of record's own inline definition of one `runs` column, verbatim.
///
/// Used where a migration must ADD a canonical column: taking the text from the
/// record is what keeps the added column's type, default, and named CHECK from
/// becoming a second encoding that drifts (wamn-0h0g.20.9).
fn runs_record_column_def(column: &str) -> String {
    record_columns(RUN_STATE_SQL, "wamn_run", "runs")
        .into_iter()
        .find_map(|(name, definition)| (name == column).then_some(definition))
        .unwrap_or_else(|| panic!("the schema of record must define runs.{column}"))
}

fn execution_pin_cutover_sql(schema: &BareSchemaName) -> String {
    let target = schema.quoted();
    // The admission-pin trigger below names `capture_mode` and
    // `durability_class`, and PostgreSQL validates a `BEFORE UPDATE OF` column
    // list at CREATE TRIGGER time — so on a legacy database that predates
    // either column the whole cutover aborted with `column "capture_mode" of
    // relation "runs" does not exist`. Both ride this ADD block, WITH the
    // record's inline named CHECK: their checks are `CheckOrigin::Inline`, and
    // the exact-CHECK pass below skips an inline spec whose column the
    // OBSERVATION lacks, so a bare ADD here would leave the column unconstrained
    // for a whole reconcile turn and re-open the non-convergence this fixes.
    let capture_mode_column = runs_record_column_def("capture_mode");
    let durability_class_column = runs_record_column_def("durability_class");
    // One encoding of the guard and its trigger, shared with the steady-state
    // helper/trigger repair. A private copy here is what dropped
    // `durability_class` from both the guard body and the trigger's column list.
    let admission_pin_guard = rewrite_schema(GUARD_RUN_ADMISSION_PINS_SQL, schema);
    let admission_pin_trigger = rewrite_schema(RUNS_ADMISSION_PINS_TRIGGER_SQL, schema);
    format!(
        r#"LOCK TABLE catalog.release_flows, {target}.runs IN ACCESS EXCLUSIVE MODE;
DO $execution_pin_preflight$
BEGIN
    IF EXISTS (SELECT 1 FROM catalog.release_flows)
       OR EXISTS (SELECT 1 FROM {target}.runs) THEN
        RAISE EXCEPTION USING
            ERRCODE = '55000',
            MESSAGE = 'execution-pin-cutover-requires-empty-run-and-release-membership';
    END IF;
END
$execution_pin_preflight$;
ALTER TABLE catalog.release_flows
    ADD COLUMN IF NOT EXISTS execution_bundle_hash text;
ALTER TABLE catalog.release_flows
    ALTER COLUMN execution_bundle_hash TYPE text USING execution_bundle_hash::text,
    ALTER COLUMN execution_bundle_hash SET NOT NULL,
    DROP CONSTRAINT IF EXISTS release_flows_execution_bundle_hash_check,
    DROP CONSTRAINT IF EXISTS release_flows_execution_bundle_fk,
    ADD CONSTRAINT release_flows_execution_bundle_hash_check
        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{{64}}$'),
    ADD CONSTRAINT release_flows_execution_bundle_fk
        FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash);
DROP INDEX IF EXISTS catalog.release_flows_execution_bundle;
CREATE INDEX release_flows_execution_bundle
    ON catalog.release_flows (tenant_id, execution_bundle_hash);
ALTER TABLE {target}.runs
    ADD COLUMN IF NOT EXISTS execution_bundle_hash text,
    ADD COLUMN IF NOT EXISTS release_version int,
    ADD COLUMN IF NOT EXISTS manifest_digest text,
    ADD COLUMN IF NOT EXISTS {capture_mode_column},
    ADD COLUMN IF NOT EXISTS {durability_class_column};
ALTER TABLE {target}.runs
    ALTER COLUMN catalog_id TYPE text USING catalog_id::text,
    ALTER COLUMN catalog_version TYPE integer USING catalog_version::integer,
    ALTER COLUMN environment TYPE text USING environment::text,
    ALTER COLUMN execution_bundle_hash TYPE text USING execution_bundle_hash::text,
    ALTER COLUMN catalog_id SET NOT NULL,
    ALTER COLUMN catalog_version SET NOT NULL,
    ALTER COLUMN environment SET NOT NULL,
    ALTER COLUMN execution_bundle_hash SET NOT NULL,
    DROP CONSTRAINT IF EXISTS runs_check,
    DROP CONSTRAINT IF EXISTS runs_environment_check,
    DROP CONSTRAINT IF EXISTS runs_release_fk,
    DROP CONSTRAINT IF EXISTS runs_execution_bundle_fk,
    ADD CONSTRAINT runs_check CHECK (
        catalog_id <> '' AND catalog_version > 0 AND environment <> ''
        AND execution_bundle_hash ~ '^sha256:[0-9a-f]{{64}}$'),
    ADD CONSTRAINT runs_release_fk
        FOREIGN KEY (tenant_id, catalog_id, catalog_version)
        REFERENCES catalog.release_manifests (tenant_id, catalog_id, catalog_version),
    ADD CONSTRAINT runs_execution_bundle_fk
        FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash);
-- The cutover restores only the RATIFIED authority for the columns it adds
-- (wamn-0h0g.12.40): `execution_bundle_hash` is admission-time INSERT only, and
-- the claim record is UPDATE only. Granting all three both ways re-opened, on
-- the legacy migration path, exactly what run-state.sql closes for fresh
-- installs — and the pin trigger below is BEFORE UPDATE, so it never gated the
-- INSERT half at all.
GRANT INSERT (execution_bundle_hash),
      UPDATE (release_version, manifest_digest)
    ON {target}.runs TO wamn_app;
DROP INDEX IF EXISTS {target}.runs_release;
DROP INDEX IF EXISTS {target}.runs_execution_bundle;
CREATE INDEX runs_release ON {target}.runs (tenant_id, catalog_id, catalog_version);
CREATE INDEX runs_execution_bundle ON {target}.runs (tenant_id, execution_bundle_hash);
{admission_pin_guard}
DROP TRIGGER IF EXISTS runs_admission_pins_immutable ON {target}.runs;
{admission_pin_trigger}"#
    )
}

/// The exact `runs` columns `wamn_app` may INSERT (wamn-0h0g.12.40).
///
/// This is the ratified set that `deploy/sql/run-state.sql` grants, NOT "every
/// canonical column except `capture_mode`". It is the column list of the
/// callable admission's run insert, which subsumes every other app-role insert.
/// A column added to `runs` does not join this set by being added; it joins by
/// being written by a statement `wamn_app` executes, and then by being named
/// here.
const RUNS_APP_INSERT_COLUMNS: &[&str] = &[
    "admission_context_version",
    "attachment_id",
    "catalog_id",
    "catalog_version",
    "environment",
    "event_depth",
    "event_root_run_id",
    "event_source_run_id",
    "execution_bundle_hash",
    "flow_id",
    "flow_version",
    "idempotency_key",
    "input_json",
    "invocation_context",
    "platform_revision",
    "registration_id",
    "response_deadline_at",
    "run_deadline_at",
    "run_id",
    "status",
    "tenant_id",
    "trigger_source",
];

/// The exact `runs` columns `wamn_app` may UPDATE (wamn-0h0g.12.40).
///
/// The union of the run plane's claim, park, release, and terminalize
/// statements. It is deliberately non-empty for a second reason: PostgreSQL
/// requires `UPDATE` on at least one column for any row-locking clause, and the
/// claim and fence paths take `FOR UPDATE`/`FOR KEY SHARE` on `runs`.
const RUNS_APP_UPDATE_COLUMNS: &[&str] = &[
    "caller_http_status",
    "caller_outcome_hash",
    "caller_outcome_json",
    "caller_outcome_kind",
    "caller_release_node_id",
    "caller_released_at",
    "fail_kind",
    "manifest_digest",
    "release_version",
    "result_json",
    "state_json",
    "status",
    "terminal_reason",
    "updated_at",
];

/// The `wamn_app` column grants a `runs` column earns, or `None` for a column
/// that no statement the application role executes ever writes.
fn runs_app_column_grants(column: &str) -> Option<String> {
    let ident = quote_ident(column);
    let mut grants = Vec::new();
    if RUNS_APP_INSERT_COLUMNS.contains(&column) {
        grants.push(format!("INSERT ({ident})"));
    }
    if RUNS_APP_UPDATE_COLUMNS.contains(&column) {
        grants.push(format!("UPDATE ({ident})"));
    }
    (!grants.is_empty()).then(|| grants.join(", "))
}

fn repair_run_capture_privilege_sql(
    schema: &BareSchemaName,
    available_columns: impl IntoIterator<Item = String>,
) -> String {
    let available_columns = available_columns.into_iter().collect::<BTreeSet<_>>();
    debug_assert!(available_columns.contains("capture_mode"));
    // Only columns that BOTH exist on the live table and belong to a ratified
    // set are granted. Intersecting with the observation is what keeps the
    // repair from naming a column a legacy database does not have yet.
    let granted = |ratified: &[&str]| {
        ratified
            .iter()
            .filter(|column| available_columns.contains(**column))
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let insertable_columns = granted(RUNS_APP_INSERT_COLUMNS);
    let updatable_columns = granted(RUNS_APP_UPDATE_COLUMNS);
    let all_columns = available_columns
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    let qualified = format!("{}.runs", schema.quoted());
    let mut writable_clauses = Vec::new();
    if !insertable_columns.is_empty() {
        writable_clauses.push(format!("INSERT ({insertable_columns})"));
    }
    if !updatable_columns.is_empty() {
        writable_clauses.push(format!("UPDATE ({updatable_columns})"));
    }
    let writable_grant = if writable_clauses.is_empty() {
        String::new()
    } else {
        format!(
            "GRANT {} ON TABLE {qualified} TO wamn_app; ",
            writable_clauses.join(", ")
        )
    };
    format!(
        "LOCK TABLE {qualified} IN ACCESS EXCLUSIVE MODE; \
         REVOKE SELECT ({all_columns}), INSERT ({all_columns}), \
                UPDATE ({all_columns}), REFERENCES ({all_columns}) \
           ON TABLE {qualified} FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}; \
         REVOKE ALL PRIVILEGES ON TABLE {qualified} \
           FROM PUBLIC, wamn_app, {SCENARIO_AUTHOR_ROLE}; \
         GRANT SELECT, DELETE ON TABLE {qualified} TO wamn_app; \
         GRANT SELECT ON TABLE {qualified} TO {SCENARIO_AUTHOR_ROLE}; \
         {writable_grant}\
         DO $run_capture_acl$ BEGIN \
           IF EXISTS ( \
                SELECT 1 \
                  FROM unnest(ARRAY['wamn_app','{SCENARIO_AUTHOR_ROLE}']) actor, \
                       unnest(ARRAY['INSERT','UPDATE']) privilege \
                 WHERE pg_catalog.has_column_privilege( \
                   actor, '{qualified}', 'capture_mode', privilege)) \
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
              OR NOT pg_catalog.has_table_privilege( \
                   '{SCENARIO_AUTHOR_ROLE}', '{qualified}', 'SELECT') \
              OR EXISTS ( \
                   SELECT 1 \
                     FROM unnest(ARRAY[ \
                       'INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                    WHERE pg_catalog.has_table_privilege( \
                      '{SCENARIO_AUTHOR_ROLE}', '{qualified}', privilege)) \
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
        || observed(&obs.authoring_table_privileges, SCENARIO_AUTHOR_ROLE) != expected(&["SELECT"])
        || observed(&obs.authoring_effective_table_privileges, "wamn_app")
            != expected(&["SELECT", "DELETE"])
        || observed(
            &obs.authoring_effective_table_privileges,
            SCENARIO_AUTHOR_ROLE,
        ) != expected(&["SELECT"])
        || observed(&obs.authoring_effective_column_privileges, "wamn_app")
            != expected(&["SELECT", "INSERT", "UPDATE"])
        || observed(
            &obs.authoring_effective_column_privileges,
            SCENARIO_AUTHOR_ROLE,
        ) != expected(&["SELECT"])
        || obs
            .authoring_table_owners
            .get(&(schema.as_str().to_string(), "runs".to_string()))
            .is_some_and(|owner| owner == "wamn_app" || owner == SCENARIO_AUTHOR_ROLE)
        || obs.app_run_capture_privileges.0
        || obs.app_run_capture_privileges.1
        || !obs.app_run_capture_privileges.2
}

fn is_effect_writer_generation_role(role: &str) -> bool {
    let Some(suffix) = role.strip_prefix("wamn_effect_writer_") else {
        return false;
    };
    let Some((scope_hash, generation)) = suffix.rsplit_once('_') else {
        return false;
    };
    scope_hash.len() == 40
        && scope_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && matches!(generation, "a" | "b")
}

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
                       AND parent.rolname IN ( \
                             'wamn_effect_writer', 'wamn_run_projection_writer'))) \
            AND (NOT generation.rolcanlogin OR generation.rolsuper \
                 OR generation.rolcreatedb OR generation.rolcreaterole \
                 OR NOT generation.rolinherit OR generation.rolreplication \
                 OR generation.rolbypassrls \
                 OR (SELECT pg_catalog.array_agg( \
                              parent.rolname::text ORDER BY parent.rolname::text) \
                       FROM pg_catalog.pg_auth_members AS edge \
                       JOIN pg_catalog.pg_roles AS parent ON parent.oid = edge.roleid \
                      WHERE edge.member = generation.oid) \
                    IS DISTINCT FROM ARRAY[ \
                         'wamn_effect_writer', 'wamn_run_projection_writer']::text[] \
                 OR EXISTS ( \
                      SELECT 1 FROM pg_catalog.pg_auth_members AS edge \
                       WHERE edge.member = generation.oid \
                         AND (edge.admin_option OR NOT edge.inherit_option \
                              OR NOT edge.set_option)) \
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
    let has_runs = obs.tables.contains_key("runs");
    let capture_mode_present = obs
        .tables
        .get("runs")
        .is_some_and(|columns| columns.contains("capture_mode"));
    let run_capture_privileges_drifted = run_capture_privileges_drifted(schema, obs);
    let has_release_flows = obs.catalog_tables.contains("release_flows");
    let execution_pin_cutover_needed = (has_runs
        && !run_execution_pin_contract_complete(schema, obs))
        || (has_release_flows && !release_flow_execution_pin_contract_complete(obs));

    if (has_runs || has_release_flows)
        && execution_pin_cutover_needed
        && (obs.run_rows != 0 || obs.release_flow_rows != 0)
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::ExecutionPinCutover,
            target: format!("catalog.release_flows+{}.runs", schema.as_str()),
            sql: if has_runs && has_release_flows {
                execution_pin_cutover_sql(schema)
            } else {
                partial_execution_pin_refusal_sql(schema, has_runs, has_release_flows)
            },
        });
        return plan;
    }

    let effect_writer_ledger_cutover_needed = effect_writer_ledger_cutover_needed(schema, obs);
    let effect_writer_cutover_needed = effect_writer_cutover_needed(schema, obs);
    if effect_writer_ledger_cutover_needed && obs.effect_ledger_rows != 0 {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EffectWriterCutover,
            target: "effect-ledgers.coordinate-writer-boundary".to_string(),
            sql: effect_writer_cutover_sql(schema, obs),
        });
        return plan;
    }

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
    if obs
        .run_projection_writer_role
        .is_none_or(|role| !role.is_acl_only())
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::VerifyRunProjectionWriterRole,
            target: RUN_PROJECTION_WRITER_ROLE.to_string(),
            sql: format!("DO $run_projection_writer_role$ \
                  DECLARE role_oid oid; \
                  BEGIN \
                    SELECT oid INTO role_oid FROM pg_catalog.pg_roles \
                     WHERE rolname = 'wamn_run_projection_writer' AND NOT rolcanlogin \
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
                         MESSAGE = 'run-projection-writer-role-out-of-bounds'; \
                    END IF; \
                  END $run_projection_writer_role$",
                generation_contract = generation_role_contract_violation_sql(),
            ),
        });
    }
    let frame_cutover_targets = frame_identity_cutover_targets(obs, schema);
    if frame_cutover_targets.needed() {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::FrameIdentityCutover,
            target: "node_runs.effect_attempts.frame-identity".to_string(),
            sql: frame_identity_cutover_sql(schema, frame_cutover_targets),
        });
    }
    if effect_writer_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::EffectWriterCutover,
            target: "effect-ledgers.coordinate-writer-boundary".to_string(),
            sql: effect_writer_cutover_sql(schema, obs),
        });
    }
    let capture_output_size_rename_needed = capture_output_size_rename_needed(obs);
    let capture_output_size_conflict = capture_output_size_conflict(obs);
    let capture_projection_cutover_needed = capture_projection_cutover_needed(obs);
    if capture_projection_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::CaptureProjectionCutover,
            target: "node_runs.capture-projection".to_string(),
            sql: capture_projection_cutover_sql(
                schema,
                capture_output_size_rename_needed,
                capture_output_size_conflict,
            ),
        });
    }
    if obs.tables.contains_key("node_runs")
        && obs
            .column_types
            .get(&("node_runs".to_string(), "seq".to_string()))
            .is_some_and(|column_type| column_type != "bigint")
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::WidenNodeRunSequence,
            target: "node_runs.seq".to_string(),
            sql: format!(
                "ALTER TABLE {}.node_runs ALTER COLUMN seq TYPE bigint USING seq::bigint",
                schema.quoted()
            ),
        });
    }
    let invocation_columns = obs.tables.get("invocation_admissions");
    let invocation_expiry_present =
        invocation_columns.is_some_and(|columns| columns.contains("expires_at"));
    let invocation_key_non_null = invocation_columns
        .is_some_and(|columns| columns.contains("client_key_digest"))
        && obs.non_nullable_columns.contains(&(
            "invocation_admissions".to_string(),
            "client_key_digest".to_string(),
        ));
    let invocation_expiry_index_present = obs.indexes.contains_key("invocation_admissions_expiry");
    let invocation_retention_cutover_needed =
        invocation_expiry_present || invocation_key_non_null || invocation_expiry_index_present;
    if invocation_retention_cutover_needed {
        let mut statements = Vec::new();
        if invocation_expiry_index_present {
            statements.push(format!(
                "DROP INDEX IF EXISTS {}.invocation_admissions_expiry",
                schema.quoted()
            ));
        }
        let mut alterations = Vec::new();
        if invocation_expiry_present {
            alterations.push("DROP COLUMN IF EXISTS expires_at".to_string());
        }
        if invocation_key_non_null {
            alterations.push("ALTER COLUMN client_key_digest DROP NOT NULL".to_string());
        }
        if !alterations.is_empty() {
            statements.push(format!(
                "ALTER TABLE {}.invocation_admissions {}",
                schema.quoted(),
                alterations.join(", ")
            ));
        }
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::InvocationAdmissionRetentionCutover,
            target: "invocation_admissions.retention".to_string(),
            sql: statements.join("; "),
        });
    }

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
            })
            .cloned();
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairRunCapturePrivilege,
            target: "runs.capture_mode".to_string(),
            sql: repair_run_capture_privilege_sql(schema, available_columns),
        });
    }

    // 1. Missing run-plane tables → EnsureSchema once, then per-table sections
    //    in file order (FKs resolve: runs before node_runs and run_queue).
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

    // `runs` now has immediate catalog FKs, so catalog creation must precede
    // every missing run-table section.
    plan.actions.extend(creates);

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
    if obs.run_projection_schema_privileges != (true, false) {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairEffectWriterPrivilege,
            target: format!("{}.projection-usage", schema.as_str()),
            sql: format!(
                "REVOKE ALL PRIVILEGES ON SCHEMA {} FROM {RUN_PROJECTION_WRITER_ROLE}; \
                 GRANT USAGE ON SCHEMA {} TO {RUN_PROJECTION_WRITER_ROLE}; \
                 DO $run_projection_schema_acl$ BEGIN \
                   IF NOT pg_catalog.has_schema_privilege('{RUN_PROJECTION_WRITER_ROLE}', '{}', 'USAGE') \
                      OR pg_catalog.has_schema_privilege('{RUN_PROJECTION_WRITER_ROLE}', '{}', 'CREATE') \
                   THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                        MESSAGE = 'run-projection-writer-schema-privilege-out-of-bounds'; \
                   END IF; \
                 END $run_projection_schema_acl$",
                schema.quoted(),
                schema.quoted(),
                schema.as_str(),
                schema.as_str(),
            ),
        });
    }
    if obs.tables.contains_key("node_runs")
        && !frame_cutover_targets.node
        && !effect_writer_cutover_needed
        && !effect_writer_ledger_cutover_needed
        && !capture_projection_cutover_needed
    {
        let expected = |grantee: &str| -> BTreeSet<String> {
            match grantee {
                "wamn_app" => ["SELECT"].into_iter().map(str::to_string).collect(),
                RUN_PROJECTION_WRITER_ROLE => ["SELECT", "INSERT", "UPDATE", "DELETE"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                _ => BTreeSet::new(),
            }
        };
        let fixed_grantees = [
            "PUBLIC",
            "wamn_app",
            SCENARIO_AUTHOR_ROLE,
            EFFECT_WRITER_ROLE,
            RUN_PROJECTION_WRITER_ROLE,
        ];
        let direct_drifted = fixed_grantees.into_iter().any(|grantee| {
            obs.node_runs_table_privileges
                .get(grantee)
                .cloned()
                .unwrap_or_default()
                != expected(grantee)
        }) || obs.node_runs_table_privileges.iter().any(
            |(grantee, privileges)| {
                !fixed_grantees.contains(&grantee.as_str()) && !privileges.is_empty()
            },
        ) || obs
            .node_runs_column_privileges
            .values()
            .any(|privileges| !privileges.is_empty());
        let expected_effective = |grantee: &str| -> BTreeSet<String> {
            if is_effect_writer_generation_role(grantee) {
                ["SELECT", "INSERT", "UPDATE", "DELETE"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            } else if obs.node_runs_app_members.contains(grantee) {
                ["SELECT"].into_iter().map(str::to_string).collect()
            } else {
                expected(grantee)
            }
        };
        let effective_drifted = obs
            .node_runs_effective_privileges
            .iter()
            .any(|(grantee, privileges)| privileges != &expected_effective(grantee))
            || fixed_grantees[1..].iter().any(|grantee| {
                obs.node_runs_effective_privileges
                    .get(*grantee)
                    .cloned()
                    .unwrap_or_default()
                    != expected_effective(grantee)
            });
        let expected_column = |grantee: &str| -> BTreeSet<String> {
            if is_effect_writer_generation_role(grantee) {
                ["SELECT", "INSERT", "UPDATE"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            } else if obs.node_runs_app_members.contains(grantee) {
                ["SELECT"].into_iter().map(str::to_string).collect()
            } else {
                match grantee {
                    "wamn_app" => ["SELECT"].into_iter().map(str::to_string).collect(),
                    RUN_PROJECTION_WRITER_ROLE => ["SELECT", "INSERT", "UPDATE"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    _ => BTreeSet::new(),
                }
            }
        };
        let effective_column_drifted = obs
            .node_runs_effective_column_privileges
            .iter()
            .any(|(grantee, privileges)| privileges != &expected_column(grantee))
            || fixed_grantees[1..].iter().any(|grantee| {
                obs.node_runs_effective_column_privileges
                    .get(*grantee)
                    .cloned()
                    .unwrap_or_default()
                    != expected_column(grantee)
            });
        let boundary_owned = obs.node_runs_owner.as_deref().is_some_and(|owner| {
            [
                "wamn_app",
                SCENARIO_AUTHOR_ROLE,
                EFFECT_WRITER_ROLE,
                RUN_PROJECTION_WRITER_ROLE,
            ]
            .contains(&owner)
        });
        if direct_drifted || effective_drifted || effective_column_drifted || boundary_owned {
            let qualified = format!("{}.node_runs", schema.quoted());
            let columns = obs.tables["node_runs"]
                .iter()
                .map(|column| quote_ident(column))
                .collect::<Vec<_>>()
                .join(", ");
            let revoke_grantees = fixed_grantees
                .into_iter()
                .map(str::to_string)
                .chain(obs.node_runs_table_privileges.keys().cloned())
                .chain(obs.node_runs_column_privileges.keys().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|grantee| {
                    if grantee == "PUBLIC" {
                        grantee
                    } else {
                        quote_ident(&grantee)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairEffectWriterPrivilege,
                target: format!("{}.node_runs", schema.as_str()),
                sql: format!(
                    "REVOKE SELECT ({columns}), INSERT ({columns}), UPDATE ({columns}), \
                            REFERENCES ({columns}) ON TABLE {qualified} \
                       FROM {revoke_grantees}; \
                     REVOKE ALL PRIVILEGES ON TABLE {qualified} \
                       FROM {revoke_grantees}; \
                     GRANT SELECT ON TABLE {qualified} TO wamn_app; \
                     GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE {qualified} \
                       TO {RUN_PROJECTION_WRITER_ROLE}; \
                     DO $node_runs_projection_acl$ BEGIN \
                       IF EXISTS (SELECT 1 FROM unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('wamn_app', '{qualified}', privilege)) \
                          OR EXISTS (SELECT 1 FROM unnest(ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('{SCENARIO_AUTHOR_ROLE}', '{qualified}', privilege) \
                                      OR pg_catalog.has_table_privilege('{EFFECT_WRITER_ROLE}', '{qualified}', privilege)) \
                          OR EXISTS (SELECT 1 FROM unnest(ARRAY['TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('{RUN_PROJECTION_WRITER_ROLE}', '{qualified}', privilege)) \
                          OR pg_catalog.has_any_column_privilege('wamn_app', '{qualified}', 'INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{SCENARIO_AUTHOR_ROLE}', '{qualified}', 'SELECT,INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{EFFECT_WRITER_ROLE}', '{qualified}', 'SELECT,INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{RUN_PROJECTION_WRITER_ROLE}', '{qualified}', 'REFERENCES') \
                          OR EXISTS ( \
                               SELECT 1 FROM pg_catalog.pg_roles AS actor \
                               CROSS JOIN pg_catalog.pg_class AS relation \
                               WHERE relation.oid = pg_catalog.to_regclass('{qualified}') \
                                 AND NOT actor.rolsuper AND actor.oid <> relation.relowner \
                                 AND actor.rolname !~ '^pg_' \
                                 AND actor.rolname NOT IN ('wamn_app', '{SCENARIO_AUTHOR_ROLE}', \
                                                           '{EFFECT_WRITER_ROLE}', '{RUN_PROJECTION_WRITER_ROLE}') \
                                 AND actor.rolname !~ '^wamn_effect_writer_[0-9a-f]{{40}}_[ab]$' \
                                 AND (NOT pg_catalog.pg_has_role(actor.oid, 'wamn_app', 'USAGE') \
                                      OR pg_catalog.has_table_privilege(actor.oid, relation.oid, \
                                           'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') \
                                      OR pg_catalog.has_any_column_privilege(actor.oid, relation.oid, \
                                           'INSERT,UPDATE,REFERENCES')) \
                                 AND (pg_catalog.has_table_privilege(actor.oid, relation.oid, \
                                         'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') \
                                      OR pg_catalog.has_any_column_privilege(actor.oid, relation.oid, \
                                         'SELECT,INSERT,UPDATE,REFERENCES'))) \
                          OR (SELECT owner.rolname FROM pg_catalog.pg_class relation \
                              JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner \
                             WHERE relation.oid = pg_catalog.to_regclass('{qualified}')) \
                             IN ('wamn_app', '{SCENARIO_AUTHOR_ROLE}', '{EFFECT_WRITER_ROLE}', '{RUN_PROJECTION_WRITER_ROLE}') \
                       THEN RAISE EXCEPTION USING ERRCODE = '42501', \
                            MESSAGE = 'node-runs-projection-privilege-out-of-bounds'; \
                       END IF; \
                     END $node_runs_projection_acl$"
                ),
            });
        }
    }
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        if !obs.tables.contains_key(table) {
            continue;
        }
        let expected = |grantee: &str| -> BTreeSet<String> {
            match grantee {
                "wamn_app" => ["SELECT"].into_iter().map(str::to_string).collect(),
                EFFECT_WRITER_ROLE => ["SELECT", "INSERT"]
                    .into_iter()
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
                     GRANT SELECT, INSERT ON TABLE {qualified} TO {EFFECT_WRITER_ROLE}; \
                     DO $effect_ledger_acl$ BEGIN \
                       IF EXISTS (SELECT 1 FROM unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('wamn_app', '{qualified}', privilege)) \
                          OR EXISTS (SELECT 1 FROM unnest(ARRAY['INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('{SCENARIO_AUTHOR_ROLE}', '{qualified}', privilege)) \
                          OR EXISTS (SELECT 1 FROM unnest(ARRAY['UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER']) privilege \
                                   WHERE pg_catalog.has_table_privilege('{EFFECT_WRITER_ROLE}', '{qualified}', privilege)) \
                          OR pg_catalog.has_any_column_privilege('wamn_app', '{qualified}', 'INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{SCENARIO_AUTHOR_ROLE}', '{qualified}', 'SELECT,INSERT,UPDATE,REFERENCES') \
                          OR pg_catalog.has_any_column_privilege('{EFFECT_WRITER_ROLE}', '{qualified}', 'UPDATE,REFERENCES') \
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

    if execution_pin_cutover_needed {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::ExecutionPinCutover,
            target: format!("catalog.release_flows+{}.runs", schema.as_str()),
            sql: execution_pin_cutover_sql(schema),
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
                RUN_PROJECTION_WRITER_ROLE,
            ]
        } else {
            &["PUBLIC", "wamn_app", SCENARIO_AUTHOR_ROLE]
        };
        let effective_grantees: &[&str] = if is_environment_policy {
            &[
                "wamn_app",
                SCENARIO_AUTHOR_ROLE,
                EFFECT_WRITER_ROLE,
                RUN_PROJECTION_WRITER_ROLE,
            ]
        } else {
            &["wamn_app", SCENARIO_AUTHOR_ROLE]
        };
        let expected_for = |grantee: &str| -> BTreeSet<String> {
            let privileges = match grantee {
                "wamn_app" => spec.app,
                SCENARIO_AUTHOR_ROLE => spec.author,
                "PUBLIC" | EFFECT_WRITER_ROLE | RUN_PROJECTION_WRITER_ROLE => &[],
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
                EFFECT_WRITER_ROLE | RUN_PROJECTION_WRITER_ROLE => &[],
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
                if table == "runs" && col == "execution_bundle_hash" {
                    continue;
                }
                // The pin cutover installs the claim-time record columns itself,
                // because the trigger it recreates names them. Skipping here is
                // what keeps that from colliding with a plain ADD COLUMN. The
                // admission pins the trigger also names — `capture_mode` and
                // `durability_class` — ride the same rule (wamn-0h0g.20.9): a
                // plain ADD COLUMN is planned AFTER the cutover, so a legacy
                // database missing either one aborted the cutover at CREATE
                // TRIGGER before the column could ever be added.
                if execution_pin_cutover_needed
                    && table == "runs"
                    && matches!(
                        col.as_str(),
                        "release_version" | "manifest_digest" | "capture_mode" | "durability_class"
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
                if capture_output_size_rename_needed && table == "node_runs" && col == "output_size"
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
                                !child_run_cutover_needed
                                    || !RETIRED_CHILD_RUN_COLUMNS.contains(&column.as_str())
                            });
                        format!(
                            "LOCK TABLE {}.runs IN ACCESS EXCLUSIVE MODE; {add_column_sql}; {}",
                            schema.quoted(),
                            repair_run_capture_privilege_sql(schema, available_columns),
                        )
                    } else if let Some(column_grants) = (table == "runs"
                        && col != "capture_mode"
                        && (capture_mode_present
                            || (!capture_mode_present
                                && obs.app_run_capture_privileges.0
                                && record_cols[..record_column_index]
                                    .iter()
                                    .any(|(column, _)| column == "capture_mode"))))
                    .then(|| runs_app_column_grants(col))
                    .flatten()
                    {
                        // A column added to `runs` earns ONLY the grants its
                        // ratified sets name (wamn-0h0g.12.40). Granting every
                        // new column INSERT + UPDATE unconditionally is how
                        // `release_version` and `manifest_digest` became
                        // app-writable with no decision behind it.
                        format!(
                            "{add_column_sql}; GRANT {column_grants} ON TABLE {}.runs TO wamn_app",
                            schema.quoted(),
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
                if retired_node_attempt_columns_present(obs)
                    && table == "node_runs"
                    && RETIRED_NODE_ATTEMPT_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if capture_projection_cutover_needed
                    && table == "node_runs"
                    && RETIRED_CAPTURE_PROJECTION_COLUMNS.contains(&col.as_str())
                {
                    continue;
                }
                if (capture_output_size_rename_needed || capture_output_size_conflict)
                    && table == "node_runs"
                    && col == LEGACY_OUTPUT_SIZE_COLUMN
                {
                    continue;
                }
                if invocation_retention_cutover_needed
                    && table == "invocation_admissions"
                    && col == "expires_at"
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

    // A broad legacy grant is normally narrowed by the `capture_mode` AddColumn
    // branch above. The pin cutover now owns that ADD (wamn-0h0g.20.9), so on
    // that path the branch never runs and the narrowing has to happen here
    // instead — otherwise the cutover would hand `wamn_app` write authority over
    // the capture carrier it just created. Every record column exists by the
    // time this action executes: the cutover and the AddColumn pass both precede
    // it.
    if !capture_mode_present
        && run_capture_privileges_drifted
        && (!obs.app_run_capture_privileges.0 || execution_pin_cutover_needed)
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
            && !(retired_node_attempt_columns_present(obs)
                && table == "node_runs"
                && retired_node_attempt_check(definition))
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

    // 2d. The durable orchestration and effect-ledger FKs remain exact. A
    // missing table's canonical CREATE section carries these, so repair only
    // observed tables.
    for (table, name, definition, sql) in [
        (
            "authoring_test_case_runs",
            TEST_CASE_RESERVATION_FK_NAME,
            TEST_CASE_RESERVATION_FK_DEF,
            TEST_CASE_RESERVATION_FK_SQL,
        ),
        (
            "authoring_test_reports",
            TEST_REPORT_RESERVATION_FK_NAME,
            TEST_REPORT_RESERVATION_FK_DEF,
            TEST_REPORT_RESERVATION_FK_SQL,
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
        if execution_pin_cutover_needed
            && spec.table == "runs"
            && spec.name == "runs_admission_pins_immutable"
        {
            continue;
        }
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

/// A present index is STALE when the record definition names a record column of
/// its table that the live definition does not (word-boundary token match, so
/// `run_id` never matches inside `event_root_run_id`). This is deliberately the
/// narrow, real drift class — notably the pre-E4 `run_queue_claimable` without
/// `stream_seq` — not a general definition difference.
fn index_definition_stale(file: &str, table: &str, record_stmt: &str, live_def: &str) -> bool {
    // Unit observations intentionally use the schema-of-record statement as
    // the live definition. PostgreSQL's `pg_indexes` rendering is checked
    // below; the record itself is already canonical by construction.
    if live_def == record_stmt {
        return false;
    }

    let live = live_def.split_whitespace().collect::<Vec<_>>().join(" ");
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
           FOR SELECT USING (tenant_id = NULLIF(current_setting('app.tenant', true), ''))"
    )
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

/// Security attributes of the host-only scenario-author role (zero or one row).
pub fn select_scenario_author_role_sql() -> &'static str {
    "SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolinherit, \
            rolreplication, rolbypassrls \
       FROM pg_catalog.pg_roles WHERE rolname = 'wamn_scenario_author'"
}

/// Provisioning-owned writer role attributes plus ownership/membership/CONNECT.
pub fn select_effect_writer_role_sql() -> String {
    format!(
        "SELECT role.rolcanlogin, role.rolsuper, role.rolcreatedb, role.rolcreaterole, \
            role.rolinherit, role.rolreplication, role.rolbypassrls, \
            pg_catalog.has_database_privilege(role.oid, current_database(), 'CONNECT'), \
            EXISTS (SELECT 1 FROM pg_catalog.pg_class WHERE relowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_proc WHERE proowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datdba = role.oid), \
            EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members WHERE member = role.oid) \
              OR EXISTS ( \
                   SELECT 1 FROM pg_catalog.pg_auth_members AS membership \
                   JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member \
                   WHERE membership.roleid = role.oid \
                     AND (member.rolname !~ '^wamn_effect_writer_[0-9a-f]{{40}}_[ab]$' \
                          OR NOT member.rolcanlogin OR member.rolsuper \
                          OR member.rolcreatedb OR member.rolcreaterole \
                          OR NOT member.rolinherit OR member.rolreplication \
                          OR member.rolbypassrls)) \
              OR {generation_contract} \
       FROM pg_catalog.pg_roles AS role \
      WHERE role.rolname = 'wamn_effect_writer'",
        generation_contract = generation_role_contract_violation_sql(),
    )
}

/// Projection-writer role attributes plus ownership/membership/CONNECT.
pub fn select_run_projection_writer_role_sql() -> String {
    format!(
        "SELECT role.rolcanlogin, role.rolsuper, role.rolcreatedb, role.rolcreaterole, \
            role.rolinherit, role.rolreplication, role.rolbypassrls, \
            pg_catalog.has_database_privilege(role.oid, current_database(), 'CONNECT'), \
            EXISTS (SELECT 1 FROM pg_catalog.pg_class WHERE relowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_proc WHERE proowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datdba = role.oid), \
            EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members WHERE member = role.oid) \
              OR EXISTS ( \
                   SELECT 1 FROM pg_catalog.pg_auth_members AS membership \
                   JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member \
                   WHERE membership.roleid = role.oid \
                     AND (member.rolname !~ '^wamn_effect_writer_[0-9a-f]{{40}}_[ab]$' \
                          OR NOT member.rolcanlogin OR member.rolsuper \
                          OR member.rolcreatedb OR member.rolcreaterole \
                          OR NOT member.rolinherit OR member.rolreplication \
                          OR member.rolbypassrls)) \
              OR {generation_contract} \
       FROM pg_catalog.pg_roles AS role \
      WHERE role.rolname = 'wamn_run_projection_writer'",
        generation_contract = generation_role_contract_violation_sql(),
    )
}

/// Exact direct writer USAGE/no-PUBLIC boundary plus effective CREATE.
pub fn select_effect_writer_schema_privileges_sql() -> &'static str {
    "SELECT COALESCE( \
              EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE( \
                        namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                       WHERE acl.grantee = role.oid AND acl.privilege_type = 'USAGE') \
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE( \
                        namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                       WHERE acl.grantee = 0 \
                         AND acl.privilege_type IN ('USAGE', 'CREATE')), false), \
            COALESCE(pg_catalog.has_schema_privilege(role.oid, namespace.oid, 'CREATE'), false) \
       FROM (SELECT 1) AS singleton \
       LEFT JOIN pg_catalog.pg_roles AS role \
         ON role.rolname = 'wamn_effect_writer' \
       LEFT JOIN pg_catalog.pg_namespace AS namespace ON namespace.nspname = $1"
}

/// Exact direct projection-writer USAGE/no-PUBLIC boundary plus effective CREATE.
pub fn select_run_projection_schema_privileges_sql() -> &'static str {
    "SELECT COALESCE( \
              EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE( \
                        namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                       WHERE acl.grantee = role.oid AND acl.privilege_type = 'USAGE') \
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE( \
                        namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                       WHERE acl.grantee = 0 \
                         AND acl.privilege_type IN ('USAGE', 'CREATE')), false), \
            COALESCE(pg_catalog.has_schema_privilege(role.oid, namespace.oid, 'CREATE'), false) \
       FROM (SELECT 1) AS singleton \
       LEFT JOIN pg_catalog.pg_roles AS role \
         ON role.rolname = 'wamn_run_projection_writer' \
       LEFT JOIN pg_catalog.pg_namespace AS namespace ON namespace.nspname = $1"
}

/// Direct table grants on the mutable run projection.
pub fn select_node_runs_table_privileges_sql() -> &'static str {
    "SELECT CASE WHEN acl.grantee = 0 THEN 'PUBLIC' ELSE grantee.rolname END, \
            acl.privilege_type \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       CROSS JOIN LATERAL pg_catalog.aclexplode(relation.relacl) AS acl \
       LEFT JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = acl.grantee \
      WHERE namespace.nspname = $1 AND relation.relname = 'node_runs' \
        AND (acl.grantee = 0 OR (acl.grantee <> relation.relowner AND NOT grantee.rolsuper)) \
      ORDER BY 1, 2"
}

/// Direct column grants on the mutable run projection, across every grantee.
pub fn select_node_runs_column_privileges_sql() -> &'static str {
    "SELECT CASE WHEN acl.grantee = 0 THEN 'PUBLIC' ELSE grantee.rolname END, \
            acl.privilege_type \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
       CROSS JOIN LATERAL pg_catalog.aclexplode(attribute.attacl) AS acl \
       LEFT JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = acl.grantee \
      WHERE namespace.nspname = $1 AND relation.relname = 'node_runs' \
        AND attribute.attnum > 0 AND NOT attribute.attisdropped \
        AND (acl.grantee = 0 OR (acl.grantee <> relation.relowner AND NOT grantee.rolsuper)) \
      ORDER BY 1, 2"
}

/// Effective table grants and owner on the mutable run projection.
pub fn select_node_runs_effective_privileges_sql() -> &'static str {
    "SELECT actor.rolname, privilege.name, owner.rolname, \
            pg_catalog.pg_has_role(actor.oid, 'wamn_app', 'USAGE') \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner \
       CROSS JOIN pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('DELETE'::text), ('TRUNCATE'::text), \
                          ('REFERENCES'::text), ('TRIGGER'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname = 'node_runs' \
        AND NOT actor.rolsuper AND actor.oid <> owner.oid \
        AND actor.rolname !~ '^pg_' \
        AND pg_catalog.has_table_privilege(actor.oid, relation.oid, privilege.name) \
      ORDER BY actor.rolname, privilege.name"
}

/// Effective column grants on the mutable run projection.
pub fn select_node_runs_effective_column_privileges_sql() -> &'static str {
    "SELECT actor.rolname, privilege.name \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner \
       CROSS JOIN pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('REFERENCES'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname = 'node_runs' \
        AND NOT actor.rolsuper AND actor.oid <> owner.oid \
        AND actor.rolname !~ '^pg_' \
        AND pg_catalog.has_any_column_privilege(actor.oid, relation.oid, privilege.name) \
      ORDER BY actor.rolname, privilege.name"
}

/// Direct grants on the three immutable effect-writer ledgers.
pub fn select_effect_ledger_table_privileges_sql() -> &'static str {
    "SELECT table_name, grantee, privilege_type \
       FROM information_schema.table_privileges \
      WHERE table_schema = $1 \
        AND table_name IN ('effect_attempts', 'effect_attempt_dispatches', \
                           'effect_attempt_outcomes') \
        AND grantee IN ('PUBLIC', 'wamn_app', 'wamn_scenario_author', \
                        'wamn_effect_writer') \
      ORDER BY table_name, grantee, privilege_type"
}

/// Effective grants and owners on the effect-writer ledger boundary.
pub fn select_effect_ledger_effective_privileges_sql() -> &'static str {
    "SELECT relation.relname, actor.rolname, privilege.name, owner.rolname \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner \
       CROSS JOIN pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('DELETE'::text), ('TRUNCATE'::text), \
                          ('REFERENCES'::text), ('TRIGGER'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname IN ('effect_attempts', 'effect_attempt_dispatches', \
                                 'effect_attempt_outcomes') \
        AND actor.rolname IN ('wamn_app', 'wamn_scenario_author', 'wamn_effect_writer') \
        AND pg_catalog.has_table_privilege(actor.oid, relation.oid, privilege.name) \
      ORDER BY relation.relname, actor.rolname, privilege.name"
}

/// Effective column grants on the effect-writer ledger boundary.
pub fn select_effect_ledger_effective_column_privileges_sql() -> &'static str {
    "SELECT relation.relname, actor.rolname, privilege.name \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       CROSS JOIN pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('REFERENCES'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname IN ('effect_attempts', 'effect_attempt_dispatches', \
                                 'effect_attempt_outcomes') \
        AND actor.rolname IN ('wamn_app', 'wamn_scenario_author', 'wamn_effect_writer') \
        AND pg_catalog.has_any_column_privilege(actor.oid, relation.oid, privilege.name) \
      ORDER BY relation.relname, actor.rolname, privilege.name"
}

/// Effective table privileges of the private writer on run-authority tables.
pub fn select_effect_writer_run_table_privileges_sql() -> &'static str {
    "SELECT relation.relname, privilege.name \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS actor ON actor.rolname = 'wamn_effect_writer' \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('DELETE'::text), ('TRUNCATE'::text), \
                          ('REFERENCES'::text), ('TRIGGER'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname IN ('runs', 'run_queue') \
        AND pg_catalog.has_table_privilege(actor.oid, relation.oid, privilege.name) \
      ORDER BY relation.relname, privilege.name"
}

/// Effective column privileges of the private writer on run-authority tables.
pub fn select_effect_writer_run_column_privileges_sql() -> &'static str {
    "SELECT relation.relname, attribute.attname, privilege.name \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
       JOIN pg_catalog.pg_roles AS actor ON actor.rolname = 'wamn_effect_writer' \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('REFERENCES'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname IN ('runs', 'run_queue') \
        AND attribute.attnum > 0 AND NOT attribute.attisdropped \
        AND pg_catalog.has_column_privilege( \
              actor.oid, relation.oid, attribute.attname, privilege.name) \
      ORDER BY relation.relname, attribute.attnum, privilege.name"
}

// --- Dispatcher read-principal observation (wamn-0h0g.12.123) ----------------
//
// `$2` carries the ROLE NAME instead of the inline literal every neighbouring
// query uses. The name and the grant text both belong to
// `wamn_control_provision`, which this pure crate deliberately does not depend
// on; parameterizing keeps ONE encoding of the principal rather than a second
// copy that can drift from the builder that grants it.
//
// **Both queries read DIRECT `aclitem` entries, never `has_*_privilege`.** The
// repair is `REVOKE ALL … FROM <reader>` followed by narrow `GRANT`s, which can
// only move the reader's OWN acl entries. An effective-privilege observation
// also sees whatever the reader reaches through `PUBLIC` or through a group,
// which the repair cannot revoke — so it would encode a state the grant can
// never satisfy, drift would stay true forever, and the reconciler would plan
// the repair on every pass without converging. That is wamn-0h0g.12.40
// exactly, and it was only ever caught by a live gate.

/// Direct schema-level privileges held by the dispatcher read principal, plus
/// whether that cluster-global role exists at all. `$1` is the run-plane
/// schema, `$2` the role name.
pub fn select_dispatch_reader_schema_privileges_sql() -> &'static str {
    "SELECT reader.oid IS NOT NULL, \
            ARRAY( \
              SELECT acl.privilege_type \
                FROM pg_catalog.pg_namespace AS namespace \
                CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE( \
                  namespace.nspacl, \
                  pg_catalog.acldefault('n', namespace.nspowner))) AS acl \
               WHERE namespace.nspname = $1 \
                 AND acl.grantee = reader.oid \
               ORDER BY 1) \
       FROM (SELECT 1) AS singleton \
       LEFT JOIN pg_catalog.pg_roles AS reader ON reader.rolname = $2"
}

/// Direct table-level privileges held by the dispatcher read principal in the
/// run-plane schema. `$1` is the schema, `$2` the role name.
///
/// The `relkind` filter is the OTHER half of "observe only what the repair can
/// reach": `REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA` covers tables,
/// partitioned tables, views, materialized views and foreign tables — and NOT
/// sequences. Observing a sequence grant here would be drift no `GRANT`/`REVOKE`
/// in the repair could ever clear.
pub fn select_dispatch_reader_table_privileges_sql() -> &'static str {
    "SELECT relation.relname, acl.privilege_type \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS reader ON reader.rolname = $2 \
       CROSS JOIN LATERAL pg_catalog.aclexplode(relation.relacl) AS acl \
      WHERE namespace.nspname = $1 \
        AND relation.relkind IN ('r', 'p', 'v', 'm', 'f') \
        AND acl.grantee = reader.oid \
      ORDER BY 1, 2"
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
      WHERE grantee IN ('PUBLIC', 'wamn_app', 'wamn_scenario_author', \
                        'wamn_effect_writer', 'wamn_run_projection_writer') \
        AND ((table_schema = 'catalog' AND table_name IN \
              ('catalogs', 'flow_artifacts', 'execution_bundles', 'release_manifests', \
               'release_flows', 'catalog_heads', \
               'flow_drafts', 'validated_flow_drafts', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings', \
               'draft_safe_connection_grants', 'authoring_command_audit')) \
          OR (table_schema = $1 AND table_name IN \
              ('environment_policies', 'runs', 'authoring_test_run_reservations', \
               'authoring_test_case_runs', 'authoring_test_reports'))) \
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
      WHERE actor.rolname IN ('wamn_app', 'wamn_scenario_author', \
                              'wamn_effect_writer', 'wamn_run_projection_writer') \
        AND ((namespace.nspname = 'catalog' AND relation.relname IN \
              ('catalogs', 'flow_artifacts', 'execution_bundles', 'release_manifests', \
               'release_flows', 'catalog_heads', \
               'flow_drafts', 'validated_flow_drafts', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings', \
               'draft_safe_connection_grants', 'authoring_command_audit')) \
          OR (namespace.nspname = $1 AND relation.relname IN \
              ('environment_policies', 'runs', 'authoring_test_run_reservations', \
               'authoring_test_case_runs', 'authoring_test_reports'))) \
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
      WHERE actor.rolname IN ('wamn_app', 'wamn_scenario_author', \
                              'wamn_effect_writer', 'wamn_run_projection_writer') \
        AND ((namespace.nspname = 'catalog' AND relation.relname IN \
              ('catalogs', 'flow_artifacts', 'execution_bundles', 'release_manifests', \
               'release_flows', 'catalog_heads', \
               'flow_drafts', 'validated_flow_drafts', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings', \
               'draft_safe_connection_grants', 'authoring_command_audit')) \
          OR (namespace.nspname = $1 AND relation.relname IN \
              ('environment_policies', 'runs', 'authoring_test_run_reservations', \
               'authoring_test_case_runs', 'authoring_test_reports'))) \
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
              ('catalogs', 'flow_artifacts', 'execution_bundles', 'release_manifests', \
               'release_flows', 'catalog_heads', \
               'flow_drafts', 'validated_flow_drafts', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings', \
               'draft_safe_connection_grants', 'authoring_command_audit')) \
          OR (namespace.nspname = $1 AND relation.relname IN \
              ('environment_policies', 'runs', 'authoring_test_run_reservations', \
               'authoring_test_case_runs', 'authoring_test_reports'))) \
      ORDER BY namespace.nspname, relation.relname"
}

/// Effective guest authority on the admission-owned run capture carrier.
///
/// A table-level INSERT/UPDATE grant covers every present and future column,
/// so it is observed independently from the named-column check. Both values
/// are false when either the role, table, or capture column is absent.
///
/// The third value answers "do the live column grants MATCH THE RATIFIED SETS"
/// (wamn-0h0g.12.40). It is checked PER PRIVILEGE against the two sets, not as
/// one `bool_and` over a single blanket list: INSERT and UPDATE no longer share
/// a column set, so a shared list can never be satisfied by a correctly
/// confined table and the reconcile plan would never converge.
pub fn select_run_capture_privileges_sql() -> String {
    let quoted = |columns: &[&str]| {
        columns
            .iter()
            .map(|column| format!("'{column}'"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let insert_columns = quoted(RUNS_APP_INSERT_COLUMNS);
    let update_columns = quoted(RUNS_APP_UPDATE_COLUMNS);
    format!(
        "WITH target AS ( \
           SELECT pg_catalog.to_regclass(pg_catalog.format('%I.runs', $1::text)) AS oid \
         ), boundary_roles AS ( \
           SELECT oid FROM pg_catalog.pg_roles \
            WHERE rolname IN ('wamn_app','wamn_scenario_author') \
         ), app AS ( \
           SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app' \
         ), capture AS ( \
           SELECT attribute.attnum \
             FROM target \
             JOIN pg_catalog.pg_attribute AS attribute \
               ON attribute.attrelid = target.oid \
              AND attribute.attname = 'capture_mode' \
              AND attribute.attnum > 0 AND NOT attribute.attisdropped \
         ) \
         SELECT \
           (EXISTS ( \
             SELECT 1 \
               FROM boundary_roles actor \
               CROSS JOIN unnest(ARRAY['INSERT','UPDATE']) privilege \
              WHERE pg_catalog.has_table_privilege( \
                actor.oid, (SELECT oid FROM target), privilege)) \
            OR EXISTS ( \
              SELECT 1 \
                FROM target \
                JOIN pg_catalog.pg_class relation ON relation.oid = target.oid \
                CROSS JOIN LATERAL pg_catalog.aclexplode( \
                  COALESCE(relation.relacl, \
                           pg_catalog.acldefault('r', relation.relowner))) acl \
               WHERE acl.grantee = 0 \
                 AND acl.privilege_type IN ('INSERT','UPDATE'))), \
           (EXISTS ( \
              SELECT 1 \
                FROM boundary_roles actor \
                CROSS JOIN unnest(ARRAY['INSERT','UPDATE']) privilege \
               WHERE pg_catalog.has_column_privilege( \
                 actor.oid, (SELECT oid FROM target), \
                 (SELECT attnum FROM capture), privilege)) \
            OR EXISTS ( \
              SELECT 1 \
                FROM target \
                JOIN pg_catalog.pg_attribute attribute \
                  ON attribute.attrelid = target.oid \
                 AND attribute.attnum = (SELECT attnum FROM capture) \
                CROSS JOIN LATERAL \
                  pg_catalog.aclexplode(attribute.attacl) acl \
               WHERE acl.grantee = 0 \
                 AND acl.privilege_type IN ('INSERT','UPDATE'))), \
           COALESCE(( \
             SELECT bool_and( \
               (CASE \
                 WHEN attribute.attname = ANY (ARRAY[{insert_columns}]::text[]) THEN \
                   pg_catalog.has_column_privilege( \
                     (SELECT oid FROM app), attribute.attrelid, attribute.attnum, 'INSERT') \
                 ELSE \
                   NOT pg_catalog.has_column_privilege( \
                     (SELECT oid FROM app), attribute.attrelid, attribute.attnum, 'INSERT') \
                 END) \
               AND (CASE \
                 WHEN attribute.attname = ANY (ARRAY[{update_columns}]::text[]) THEN \
                   pg_catalog.has_column_privilege( \
                     (SELECT oid FROM app), attribute.attrelid, attribute.attnum, 'UPDATE') \
                 ELSE \
                   NOT pg_catalog.has_column_privilege( \
                     (SELECT oid FROM app), attribute.attrelid, attribute.attnum, 'UPDATE') \
                 END)) \
               FROM pg_catalog.pg_attribute AS attribute \
              WHERE attribute.attrelid = (SELECT oid FROM target) \
                AND attribute.attnum > 0 AND NOT attribute.attisdropped), false)"
    )
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

/// Every ordinary table + column in `$1`: `(relname, attname, not-null,
/// has-default, formatted-type)` in attnum order.
pub fn select_schema_columns_sql() -> &'static str {
    "SELECT c.relname, a.attname, a.attnotnull, ad.adbin IS NOT NULL, \
            pg_catalog.format_type(a.atttypid, a.atttypmod) FROM pg_class c \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     JOIN pg_attribute a ON a.attrelid = c.oid \
     LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
     WHERE n.nspname = $1 AND c.relkind = 'r' AND a.attnum > 0 AND NOT a.attisdropped \
     ORDER BY c.relname, a.attnum"
}

/// ENABLE/FORCE flags on the projected environment-policy relation.
pub fn select_environment_policy_row_security_sql() -> &'static str {
    "SELECT relation.relrowsecurity, relation.relforcerowsecurity \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
      WHERE namespace.nspname = $1 \
        AND relation.relname = 'environment_policies' \
        AND relation.relkind = 'r'"
}

/// Every policy on the projected environment-policy relation, including the
/// fields whose widening can change the visible tenant set.
pub fn select_environment_policy_policies_sql() -> &'static str {
    "SELECT policy.polname, \
            CASE policy.polcmd WHEN 'r' THEN 'select' WHEN 'a' THEN 'insert' \
              WHEN 'w' THEN 'update' WHEN 'd' THEN 'delete' ELSE 'all' END, \
            policy.polpermissive, \
            ARRAY(SELECT CASE role_oid WHEN 0 THEN 'PUBLIC' \
                              ELSE pg_catalog.pg_get_userbyid(role_oid) END \
                    FROM unnest(policy.polroles) AS role_oid ORDER BY 1), \
            pg_catalog.pg_get_expr(policy.polqual, policy.polrelid, true), \
            pg_catalog.pg_get_expr(policy.polwithcheck, policy.polrelid, true) \
       FROM pg_catalog.pg_policy AS policy \
       JOIN pg_catalog.pg_class AS relation ON relation.oid = policy.polrelid \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
      WHERE namespace.nspname = $1 \
        AND relation.relname = 'environment_policies' \
        AND relation.relkind = 'r' \
      ORDER BY policy.polname"
}

/// Count rows in the selected run table before planning the empty-only pin
/// cutover. The cutover repeats this check while holding both table locks.
pub fn count_run_rows_sql(schema: &BareSchemaName) -> String {
    format!("SELECT count(*) FROM {}.runs", schema.quoted())
}

/// Count release membership rows before planning the empty-only pin cutover.
pub fn count_release_flow_rows_sql() -> &'static str {
    "SELECT count(*) FROM catalog.release_flows"
}

/// Count persisted flow graphs that carry either retired top-level queue key.
pub fn count_retired_authored_ordering_rows_sql(schema: &BareSchemaName) -> String {
    format!(
        "SELECT count(*) FROM {}.flows \
          WHERE graph_json ? 'ordering' OR graph_json ? 'partition-policy'",
        schema.quoted()
    )
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

/// Retained helper definitions plus the three retired names needed to observe
/// the stored-suite cutover in `$1`. A retired helper the observation cannot
/// name is a helper the cutover can never be planned for, so every entry of
/// `RETIRED_STORED_SUITE_FUNCTIONS` must appear here too.
pub fn select_run_plane_helper_functions_sql() -> &'static str {
    "SELECT p.proname, pg_get_functiondef(p.oid) \
     FROM pg_proc p \
     JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = $1 \
       AND p.proname IN ('lock_catalog_head', 'pin_run_durability_class', \
                         'guard_event_lineage_immutable', \
                         'reject_immutable_effect_fact_change', \
                         'reject_immutable_operator_run_action_change', \
                         'reject_immutable_authoring_test_orchestration_change', \
                         'guard_authoring_test_orchestration_write', \
                         'reject_immutable_authoring_report_change', \
                         'guard_authoring_report_write', \
                         'reject_immutable_authoring_test_set_change', \
                         'guard_effect_disposition_append', \
                         'guard_run_admission_pins_immutable', \
                         'guard_terminal_run_delete') \
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

/// Rows in `catalog.event_registrations` carrying a retired declaration key.
///
/// The shell runs this only when the table was observed present.
pub fn count_stale_registration_keys_sql() -> &'static str {
    "SELECT count(*) FROM catalog.event_registrations \
     WHERE registration ?| ARRAY['state', 'partition-key']"
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

fn table_section_carries_trigger(table: &str, trigger: &str) -> bool {
    RUN_PLANE_FILES.iter().any(|file| {
        record_tables(file, "wamn_run")
            .iter()
            .any(|name| name == table)
            && table_section(file, "wamn_run", table).contains(&format!("CREATE TRIGGER {trigger}"))
    })
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
GRANT SELECT ON catalog.event_registrations TO wamn_app;
-- wamn-0h0g.12.29: callable-flow admission locks the live registration with
-- `FOR KEY SHARE` as wamn_app, and PostgreSQL demands UPDATE on at least one
-- column for ANY row-locking clause. `tenant_id` is the only column whose
-- FORCE-RLS WITH CHECK admits nothing but the value already in the row, so this
-- grant buys the lock and carries no semantic rewrite authority.
GRANT UPDATE (tenant_id) ON catalog.event_registrations TO wamn_app;
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
            run_projection_writer_role: Some(EffectWriterRoleObservation {
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
            run_projection_schema_privileges: (true, false),
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
        obs.app_run_capture_privileges = (false, false, true);
        obs.authoring_effective_column_privileges
            .entry((
                "demo".to_string(),
                "runs".to_string(),
                "wamn_app".to_string(),
            ))
            .or_default()
            .extend(["INSERT".to_string(), "UPDATE".to_string()]);
        for table in [
            "effect_attempts",
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
        ] {
            obs.effect_ledger_owners
                .insert(table.to_string(), "platform_admin".to_string());
            for (grantee, privileges) in [
                ("wamn_app", &["SELECT"][..]),
                (EFFECT_WRITER_ROLE, &["SELECT", "INSERT"][..]),
            ] {
                let key = (table.to_string(), grantee.to_string());
                let privileges: BTreeSet<String> = privileges
                    .iter()
                    .map(|privilege| (*privilege).to_string())
                    .collect();
                obs.effect_ledger_table_privileges
                    .insert(key.clone(), privileges.clone());
                obs.effect_ledger_effective_privileges
                    .insert(key.clone(), privileges.clone());
                obs.effect_ledger_effective_column_privileges
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
        obs.node_runs_owner = Some("platform_admin".to_string());
        for (grantee, table_privileges, column_privileges) in [
            ("wamn_app", &['S'][..], &['S'][..]),
            (
                RUN_PROJECTION_WRITER_ROLE,
                &['S', 'I', 'U', 'D'][..],
                &['S', 'I', 'U'][..],
            ),
        ] {
            let expand = |privileges: &[char]| {
                privileges
                    .iter()
                    .map(|privilege| match privilege {
                        'S' => "SELECT".to_string(),
                        'I' => "INSERT".to_string(),
                        'U' => "UPDATE".to_string(),
                        'D' => "DELETE".to_string(),
                        _ => unreachable!("closed privilege abbreviation"),
                    })
                    .collect()
            };
            obs.node_runs_table_privileges
                .insert(grantee.to_string(), expand(table_privileges));
            obs.node_runs_effective_privileges
                .insert(grantee.to_string(), expand(table_privileges));
            obs.node_runs_effective_column_privileges
                .insert(grantee.to_string(), expand(column_privileges));
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
        obs.catalog_checks.insert(
            (
                "release_flows".to_string(),
                "release_flows_execution_bundle_hash_check".to_string(),
            ),
            RELEASE_FLOWS_BUNDLE_CHECK_DEF.to_string(),
        );
        obs.catalog_checks.insert(
            (
                "authoring_command_audit".to_string(),
                AUTHORING_COMMAND_KIND_CHECK_NAME.to_string(),
            ),
            AUTHORING_COMMAND_KIND_CHECK_DEF.to_string(),
        );
        for (name, definition) in [
            (
                AUTHORING_COMMAND_REQUEST_HASH_CHECK_NAME,
                AUTHORING_COMMAND_REQUEST_HASH_CHECK_DEF,
            ),
            (
                AUTHORING_COMMAND_OUTCOME_PRESENT_CHECK_NAME,
                AUTHORING_COMMAND_OUTCOME_PRESENT_CHECK_DEF,
            ),
        ] {
            obs.catalog_checks.insert(
                ("authoring_command_audit".to_string(), name.to_string()),
                definition.to_string(),
            );
        }
        for (column, column_type) in [("request_hash", "text"), ("outcome_bytes", "bytea")] {
            let key = ("authoring_command_audit".to_string(), column.to_string());
            obs.catalog_non_nullable_columns.insert(key.clone());
            obs.catalog_column_types
                .insert(key, column_type.to_string());
        }
        obs.catalog_indexes.insert(
            "authoring_command_audit_pkey".to_string(),
            AUTHORING_COMMAND_PRIMARY_INDEX_DEF.to_string(),
        );
        obs.catalog_indexes.insert(
            "authoring_command_audit_audit_id_key".to_string(),
            AUTHORING_COMMAND_AUDIT_ID_INDEX_DEF.to_string(),
        );
        obs.catalog_non_nullable_columns.insert((
            "release_flows".to_string(),
            "execution_bundle_hash".to_string(),
        ));
        obs.catalog_column_types.insert(
            (
                "release_flows".to_string(),
                "execution_bundle_hash".to_string(),
            ),
            "text".to_string(),
        );
        obs.catalog_indexes.insert(
            "release_flows_execution_bundle".to_string(),
            RELEASE_FLOWS_EXECUTION_BUNDLE_INDEX_DEF.to_string(),
        );
        obs.catalog_foreign_keys.insert(
            (
                "release_flows".to_string(),
                "release_flows_execution_bundle_fk".to_string(),
            ),
            RELEASE_FLOWS_EXECUTION_BUNDLE_FK_DEF.to_string(),
        );
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
        obs.indexes
            .insert("node_runs_pkey".to_string(), NODE_RUNS_PKEY_DEF.to_string());
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
            ("node_runs", "frame_id", "bigint", true),
            ("node_runs", "parent_frame_id", "bigint", false),
            ("node_runs", "call_site_id", "text", false),
            ("node_runs", "current_plan_hash", "text", true),
            ("node_runs", "local_node_id", "text", true),
            ("node_runs", "seq", "bigint", true),
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
            ("catalog_id", "text"),
            ("catalog_version", "integer"),
            ("environment", "text"),
            ("execution_bundle_hash", "text"),
        ] {
            let key = ("runs".to_string(), column.to_string());
            obs.non_nullable_columns.insert(key.clone());
            obs.column_types.insert(key, ty.to_string());
        }
        obs.indexes.insert(
            "runs_release".to_string(),
            RUNS_RELEASE_INDEX_DEF.to_string(),
        );
        obs.indexes.insert(
            "runs_execution_bundle".to_string(),
            RUNS_EXECUTION_BUNDLE_INDEX_DEF.to_string(),
        );
        obs.foreign_keys.insert(
            ("runs".to_string(), "runs_release_fk".to_string()),
            RUNS_RELEASE_FK_DEF.to_string(),
        );
        obs.foreign_keys.insert(
            ("runs".to_string(), "runs_execution_bundle_fk".to_string()),
            RUNS_EXECUTION_BUNDLE_FK_DEF.to_string(),
        );
        for (table, name, definition) in [
            (
                "authoring_test_case_runs",
                TEST_CASE_RESERVATION_FK_NAME,
                TEST_CASE_RESERVATION_FK_DEF,
            ),
            (
                "authoring_test_reports",
                TEST_REPORT_RESERVATION_FK_NAME,
                TEST_REPORT_RESERVATION_FK_DEF,
            ),
        ] {
            obs.foreign_keys.insert(
                (table.to_string(), name.to_string()),
                definition.to_string(),
            );
        }
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
    fn record_tables_are_pinned() {
        assert_eq!(
            record_tables(RUN_STATE_SQL, "wamn_run"),
            [
                "environment_policies",
                "runs",
                "invocation_admissions",
                "node_runs",
                "effect_attempts",
                "effect_attempt_dispatches",
                "effect_attempt_outcomes",
                "operator_run_actions",
            ]
        );
        assert_eq!(
            record_tables(AUTHORING_TESTS_SQL, "wamn_run"),
            [
                "authoring_test_run_reservations",
                "authoring_test_case_runs",
                "authoring_test_reports",
            ]
        );
        for table in RETIRED_STORED_SUITE_TABLES {
            assert!(
                !AUTHORING_TESTS_SQL.contains(&format!("CREATE TABLE wamn_run.{table}")),
                "retired table {table} remains in the schema of record"
            );
        }
        for function in RETIRED_STORED_SUITE_FUNCTIONS {
            assert!(
                !AUTHORING_TESTS_SQL
                    .contains(&format!("CREATE OR REPLACE FUNCTION wamn_run.{function}")),
                "retired helper {function} remains in the schema of record"
            );
        }
        assert_eq!(record_tables(RUN_QUEUE_SQL, "wamn_run"), ["run_queue"]);
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
            "execution_bundles",
            "validated_flow_drafts",
            "authoring_command_audit",
        ] {
            assert!(catalog.contains(&authoring_table.to_string()));
        }
        assert_eq!(
            catalog.len(),
            34,
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
            "GRANT UPDATE (tenant_id) ON catalog.event_registrations TO wamn_app;\n",
            "",
        );
        assert!(!catalog_tail_is_complete(&without_grant));

        let grant = "GRANT UPDATE (tenant_id) ON catalog.event_registrations TO wamn_app;\n";
        let before_tail = CATALOG_SCHEMA_SQL
            .strip_suffix(EVENT_REGISTRATIONS_TAIL)
            .expect("canonical schema has the guarded tail");
        let reordered_tail = format!("{}{grant}", EVENT_REGISTRATIONS_TAIL.replace(grant, ""));
        let reordered = format!("{before_tail}{reordered_tail}");
        assert!(!catalog_tail_is_complete(&reordered));
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
    fn execution_pin_schema_of_record_is_exact_and_complete() {
        let runs = table_section(RUN_STATE_SQL, "wamn_run", "runs");
        for column in [
            "catalog_id      text NOT NULL",
            "catalog_version int NOT NULL",
            "environment     text NOT NULL",
            "execution_bundle_hash text NOT NULL",
        ] {
            assert!(runs.contains(column), "runs contract missing {column}");
        }
        assert!(runs.contains("catalog_version > 0"));
        assert!(runs.contains("execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$'"));
        assert!(runs.contains("CONSTRAINT runs_release_fk"));
        assert!(runs.contains(
            "FOREIGN KEY (tenant_id, catalog_id, catalog_version)\n        REFERENCES catalog.release_manifests"
        ));
        assert!(runs.contains("CONSTRAINT runs_execution_bundle_fk"));
        assert!(runs.contains(
            "FOREIGN KEY (tenant_id, execution_bundle_hash)\n        REFERENCES catalog.execution_bundles"
        ));
        assert!(runs.contains(
            "CREATE INDEX runs_release ON wamn_run.runs (tenant_id, catalog_id, catalog_version)"
        ));
        assert!(runs.contains(
            "CREATE INDEX runs_execution_bundle ON wamn_run.runs (tenant_id, execution_bundle_hash)"
        ));
        assert!(runs.contains(
            "BEFORE UPDATE OF catalog_id, catalog_version, environment, execution_bundle_hash"
        ));
        assert!(RUN_STATE_SQL.contains("MESSAGE = 'run-admission-pin-immutable'"));

        // The claim-time release record: two nullable columns, one paired
        // CHECK, and both named in the column-scoped guard's trigger so the
        // write-once arm can fire at all.
        assert!(runs.contains("release_version int"));
        assert!(runs.contains("manifest_digest text"));
        assert!(runs.contains("CONSTRAINT runs_release_record_check"));
        assert!(runs.contains(
            "(release_version IS NULL AND manifest_digest IS NULL)\n      OR (release_version IS NOT NULL AND manifest_digest IS NOT NULL"
        ));
        // The durability-class carrier joins the same column-scoped guard
        // (wamn-0h0g.20.1 rider 1): a column the trigger does not NAME never
        // fires its transition arm, so the class would be silently mutable.
        assert!(runs.contains(
            "capture_mode,\n                 durability_class, release_version, manifest_digest"
        ));
        assert!(runs.contains("durability_class text NOT NULL DEFAULT 'standard'"));
        assert!(runs.contains("CHECK (durability_class IN ('standard', 'durable'))"));
        assert!(RUN_STATE_SQL.contains("MESSAGE = 'run-release-record-immutable'"));
        assert!(RUN_STATE_SQL.contains("OLD.release_version IS NOT NULL"));
        assert!(RUN_STATE_SQL.contains("OLD.manifest_digest IS NOT NULL"));

        let release_flows = table_section(CATALOG_SCHEMA_SQL, "catalog", "release_flows");
        assert!(release_flows.contains("execution_bundle_hash text NOT NULL"));
        assert!(release_flows.contains(
            "CONSTRAINT release_flows_execution_bundle_hash_check\n        CHECK (execution_bundle_hash ~ '^sha256:[0-9a-f]{64}$')"
        ));
        assert!(release_flows.contains("CONSTRAINT release_flows_execution_bundle_fk"));
        assert!(
            release_flows.contains(
                "REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)"
            )
        );
        assert!(release_flows.contains(
            "CREATE INDEX release_flows_execution_bundle\n    ON catalog.release_flows (tenant_id, execution_bundle_hash)"
        ));
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
        assert!(
            rq.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON wamn_run.run_queue TO wamn_app")
        );
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

        let cat = table_section(CATALOG_SCHEMA_SQL, "catalog", "catalogs");
        assert!(cat.contains("catalogs_one_applied_per_env"));
        let artifacts = table_section(CATALOG_SCHEMA_SQL, "catalog", "flow_artifacts");
        assert!(artifacts.contains("register_flow_artifact"));

        let actions = table_section(RUN_STATE_SQL, "wamn_run", "operator_run_actions");
        assert!(actions.contains("operator_run_actions_delete_immutable"));
        assert!(actions.contains("REVOKE ALL PRIVILEGES"));
        assert!(!actions.contains("REFERENCES"));

        // Unlike the deleted test-set table, whose triggers sat directly beneath
        // it, the orchestration tables share one guard function and declare their
        // triggers after it — so a report's record section carries its RLS and
        // grant, never its trigger. Trigger presence is proven separately, by the
        // RepairTrigger leg over `authoring_test_reports_delete_immutable`.
        let reports = table_section(AUTHORING_TESTS_SQL, "wamn_run", "authoring_test_reports");
        assert!(reports.contains("authoring_test_reports_tenant"));
        assert!(reports.contains("GRANT SELECT, INSERT"));
        assert!(!reports.contains("reject_immutable_authoring_report_change"));

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
                "effect_attempts_bulk_scope",
                "invocation_admissions_run",
                "node_runs_seq",
                "run_queue_claimable",
                "runs_event_root",
                "runs_execution_bundle",
                "runs_flow",
                "runs_idempotency",
                "runs_release",
                "runs_response_deadline",
                "runs_run_deadline",
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
            12,
            "all twelve retained run-plane tables are at target"
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
                .get_mut(table)
                .expect("retained record table is observed")
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
                .expect("retained record table is observed")
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
    fn retired_validation_dimension_is_empty_only_and_idempotent() {
        let mut legacy = observation_at_record();
        legacy
            .catalog_columns
            .get_mut("validated_flow_drafts")
            .expect("record table exists")
            .insert("suite_flow_version".to_string());

        let plan = plan_run_plane(&schema("demo"), &legacy);
        assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
        let cutover = &plan.actions[0];
        assert_eq!(cutover.kind, RunPlaneActionKind::StoredSuiteCutover);
        for required in [
            "LOCK TABLE catalog.validated_flow_drafts IN ACCESS EXCLUSIVE MODE",
            "retired-validation-identity-requires-reprovision",
            "AND EXISTS (SELECT 1 FROM catalog.validated_flow_drafts)",
            "DROP CONSTRAINT IF EXISTS validated_flow_drafts_exact_pin",
            "DROP COLUMN IF EXISTS suite_flow_version",
            "ADD CONSTRAINT validated_flow_drafts_exact_pin UNIQUE",
        ] {
            assert!(
                cutover.sql.contains(required),
                "missing {required}: {}",
                cutover.sql
            );
        }
        assert!(cutover.sql.contains("pg_catalog.pg_attribute"));
        assert!(!cutover.sql.contains("CASCADE"));
        assert!(
            cutover
                .sql
                .find("retired-validation-identity-requires-reprovision")
                < cutover.sql.find("DROP COLUMN IF EXISTS suite_flow_version")
        );

        legacy
            .catalog_columns
            .get_mut("validated_flow_drafts")
            .expect("record table exists")
            .remove("suite_flow_version");
        assert!(plan_run_plane(&schema("demo"), &legacy).is_noop());
    }

    #[test]
    fn authoring_retry_ledger_cutover_is_empty_only_exact_and_idempotent() {
        let mut legacy = observation_at_record();
        legacy
            .catalog_columns
            .get_mut("authoring_command_audit")
            .unwrap()
            .retain(|column| !["request_hash", "outcome_bytes"].contains(&column.as_str()));
        for column in ["request_hash", "outcome_bytes"] {
            let key = ("authoring_command_audit".to_string(), column.to_string());
            legacy.catalog_non_nullable_columns.remove(&key);
            legacy.catalog_column_types.remove(&key);
        }
        for name in [
            AUTHORING_COMMAND_KIND_CHECK_NAME,
            AUTHORING_COMMAND_REQUEST_HASH_CHECK_NAME,
            AUTHORING_COMMAND_OUTCOME_PRESENT_CHECK_NAME,
        ] {
            legacy
                .catalog_checks
                .remove(&("authoring_command_audit".to_string(), name.to_string()));
        }
        legacy.catalog_indexes.insert(
            "authoring_command_audit_pkey".to_string(),
            "legacy".to_string(),
        );
        legacy
            .catalog_indexes
            .remove("authoring_command_audit_audit_id_key");

        let plan = plan_run_plane(&schema("demo"), &legacy);
        let cutover = plan
            .actions
            .iter()
            .find(|action| action.kind == RunPlaneActionKind::StoredSuiteCutover)
            .expect("legacy audit shape plans a retry-ledger cutover");
        for required in [
            "LOCK TABLE catalog.authoring_command_audit IN ACCESS EXCLUSIVE MODE",
            "IF EXISTS (SELECT 1 FROM catalog.authoring_command_audit)",
            "authoring-command-retry-ledger-cutover-requires-empty-audit-or-archive-and-reprovision",
            "DROP CONSTRAINT IF EXISTS authoring_command_audit_pkey",
            "DROP COLUMN IF EXISTS request_hash",
            "ADD COLUMN request_hash text NOT NULL",
            "ADD COLUMN outcome_bytes bytea NOT NULL",
            "PRIMARY KEY (tenant_id, principal_id, command_id)",
            "UNIQUE (tenant_id, audit_id)",
            "ADD CONSTRAINT authoring_command_audit_command_kind_check",
            "ADD CONSTRAINT authoring_command_audit_request_hash_check",
            "ADD CONSTRAINT authoring_command_audit_outcome_present",
        ] {
            assert!(
                cutover.sql.contains(required),
                "missing {required}: {}",
                cutover.sql
            );
        }
        assert!(
            cutover
                .sql
                .find(
                    "authoring-command-retry-ledger-cutover-requires-empty-audit-or-archive-and-reprovision",
                )
                < cutover
                    .sql
                    .find("DROP CONSTRAINT IF EXISTS authoring_command_audit_pkey")
        );
        assert!(
            !cutover
                .sql
                .contains("DELETE FROM catalog.authoring_command_audit")
        );
        assert!(
            !cutover
                .sql
                .contains("UPDATE catalog.authoring_command_audit")
        );
        assert!(plan_run_plane(&schema("demo"), &observation_at_record()).is_noop());
    }

    #[test]
    fn invocation_admission_retention_cutover_is_exact_and_idempotent() {
        let mut legacy = observation_at_record();
        legacy
            .tables
            .get_mut("invocation_admissions")
            .unwrap()
            .insert("expires_at".to_string());
        legacy.non_nullable_columns.insert((
            "invocation_admissions".to_string(),
            "client_key_digest".to_string(),
        ));
        legacy.indexes.insert(
            "invocation_admissions_expiry".to_string(),
            "CREATE INDEX invocation_admissions_expiry ON wamn_run.invocation_admissions USING btree (tenant_id, expires_at)".to_string(),
        );

        let plan = plan_run_plane(&schema("demo"), &legacy);
        let cutovers = plan
            .actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::InvocationAdmissionRetentionCutover)
            .collect::<Vec<_>>();
        assert_eq!(cutovers.len(), 1, "actions: {:#?}", plan.actions);
        let sql = &cutovers[0].sql;
        assert!(sql.contains("DROP INDEX IF EXISTS \"demo\".invocation_admissions_expiry"));
        assert!(sql.contains("DROP COLUMN IF EXISTS expires_at"));
        assert!(sql.contains("ALTER COLUMN client_key_digest DROP NOT NULL"));
        assert!(
            !plan.extra_columns.iter().any(|(table, column)| {
                table == "invocation_admissions" && column == "expires_at"
            })
        );

        let at_target = plan_run_plane(&schema("demo"), &observation_at_record());
        assert!(
            !at_target.actions.iter().any(|action| {
                action.kind == RunPlaneActionKind::InvocationAdmissionRetentionCutover
            }),
            "at-target schema repeated retention cutover: {:#?}",
            at_target.actions
        );
    }

    #[test]
    fn empty_execution_pin_drift_plans_one_atomic_cutover() {
        let mut obs = observation_at_record();
        obs.column_types.insert(
            ("runs".to_string(), "catalog_version".to_string()),
            "bigint".to_string(),
        );

        let plan = plan_run_plane(&schema("demo"), &obs);
        let cutovers: Vec<&RunPlaneAction> = plan
            .actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::ExecutionPinCutover)
            .collect();
        assert_eq!(cutovers.len(), 1, "actions: {:#?}", plan.actions);
        assert!(!plan.actions.iter().any(|action| {
            matches!(
                action.kind,
                RunPlaneActionKind::AddColumn
                    | RunPlaneActionKind::RepairConstraint
                    | RunPlaneActionKind::RepairForeignKey
                    | RunPlaneActionKind::CreateIndex
                    | RunPlaneActionKind::RecreateIndex
                    | RunPlaneActionKind::RepairTrigger
            ) && (action.target.starts_with("runs.")
                || action.target.starts_with("runs_")
                || action.target.starts_with("release_flows"))
        }));

        let sql = &cutovers[0].sql;
        let lock = sql
            .find("LOCK TABLE catalog.release_flows, \"demo\".runs IN ACCESS EXCLUSIVE MODE")
            .unwrap();
        let release_count = sql
            .find("EXISTS (SELECT 1 FROM catalog.release_flows)")
            .unwrap();
        let run_count = sql.find("EXISTS (SELECT 1 FROM \"demo\".runs)").unwrap();
        let first_ddl = sql.find("ALTER TABLE catalog.release_flows").unwrap();
        assert!(lock < release_count && release_count < first_ddl);
        assert!(lock < run_count && run_count < first_ddl);
        assert!(sql.contains("execution-pin-cutover-requires-empty-run-and-release-membership"));
        assert!(sql.contains("ALTER COLUMN catalog_version TYPE integer"));
        assert!(sql.contains("REFERENCES catalog.release_manifests"));
        assert!(sql.contains("runs_admission_pins_immutable"));
        assert!(!sql.contains("BEGIN;"));
        assert!(!sql.contains("COMMIT;"));
    }

    /// The cutover ADDs every admission pin its trigger NAMES, and its guard and
    /// trigger are the canonical ones rather than a private copy
    /// (wamn-0h0g.20.9).
    ///
    /// PostgreSQL validates `BEFORE UPDATE OF <columns>` at CREATE TRIGGER time,
    /// so a column the trigger names but the cutover never adds aborts the whole
    /// migration on a legacy database; and a column the trigger does NOT name is
    /// silently unguarded (the wamn-0h0g.20.1 unnamed-arm class).
    #[test]
    fn the_execution_pin_cutover_adds_every_admission_pin_its_trigger_names() {
        let schema = schema("demo");
        let sql = execution_pin_cutover_sql(&schema);

        for column in ["capture_mode", "durability_class"] {
            let clause = format!(
                "ADD COLUMN IF NOT EXISTS {}",
                runs_record_column_def(column)
            );
            let added = sql
                .find(&clause)
                .unwrap_or_else(|| panic!("cutover must add runs.{column} canonically: {sql}"));
            let named = sql
                .find(&format!("{column}, "))
                .unwrap_or_else(|| panic!("pin trigger must name {column}: {sql}"));
            assert!(
                added < named,
                "{column} is added before the trigger names it: {sql}"
            );
        }
        // Added WITH the record's inline named CHECK. An inline CHECK spec is
        // skipped by the exact-CHECK pass while the OBSERVATION lacks its
        // column, so a bare ADD would leave the column unconstrained until the
        // NEXT reconcile turn — the same non-convergence this bead closes.
        assert!(sql.contains("CONSTRAINT runs_capture_mode_check"));
        assert!(sql.contains("CONSTRAINT runs_durability_class_check"));

        // One encoding: the cutover's guard and trigger ARE the steady-state
        // repair's, so the reconciler can never observe drift it just wrote.
        let guard = helper_specs()
            .into_iter()
            .find(|spec| spec.name == "guard_run_admission_pins_immutable")
            .expect("the admission-pin guard is a helper of record");
        assert!(sql.contains(&rewrite_schema(&guard.sql, &schema)));
        let trigger = trigger_specs()
            .into_iter()
            .find(|spec| spec.name == "runs_admission_pins_immutable")
            .expect("the admission-pin trigger is a trigger of record");
        assert!(sql.contains(&rewrite_schema(&trigger.sql, &schema)));
    }

    /// A legacy database missing BOTH admission-pin carriers converges in ONE
    /// pass: the cutover owns the ADD, no plain `AddColumn` races it, and the
    /// broad legacy grant is still narrowed off the capture carrier.
    #[test]
    fn legacy_runs_missing_both_pin_carriers_take_them_from_the_cutover() {
        let mut obs = observation_at_record();
        obs.column_types.insert(
            ("runs".to_string(), "catalog_version".to_string()),
            "bigint".to_string(),
        );
        obs.app_run_capture_privileges = (true, false, true);
        let runs = obs.tables.get_mut("runs").expect("runs table");
        runs.remove("capture_mode");
        runs.remove("durability_class");
        for check in [
            "runs_capture_mode_check",
            "runs_durability_class_check",
            "runs_capture_mode_source_check",
        ] {
            obs.checks.remove(&("runs".to_string(), check.to_string()));
        }

        let plan = plan_run_plane(&schema("demo"), &obs);
        let cutover = plan
            .actions
            .iter()
            .position(|action| action.kind == RunPlaneActionKind::ExecutionPinCutover)
            .unwrap_or_else(|| panic!("pin cutover planned; actions: {:#?}", plan.actions));

        for column in ["capture_mode", "durability_class"] {
            assert!(
                !plan.actions.iter().any(|action| {
                    action.kind == RunPlaneActionKind::AddColumn
                        && action.target == format!("runs.{column}")
                }),
                "{column} must not be added twice: {:#?}",
                plan.actions
            );
            assert!(
                plan.actions[cutover]
                    .sql
                    .contains(&format!("ADD COLUMN IF NOT EXISTS {column} "))
            );
            // The inline CHECK rides the ADD, so no separate repair is planned
            // for it — which is only true because the ADD is not bare.
            assert!(
                !plan.actions.iter().any(|action| {
                    action.kind == RunPlaneActionKind::RepairConstraint
                        && action.target == format!("runs.runs_{column}_check")
                }),
                "the inline CHECK rides the cutover's ADD: {:#?}",
                plan.actions
            );
        }
        // The table-origin check that NAMES capture_mode is still repaired
        // separately; it is not skipped by the absent-column rule.
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairConstraint
                && action.target == "runs.runs_capture_mode_source_check"
        }));
        // The narrowing the AddColumn branch used to do still happens, after the
        // cutover has created the carrier.
        let narrowing = plan
            .actions
            .iter()
            .position(|action| action.kind == RunPlaneActionKind::RepairRunCapturePrivilege)
            .unwrap_or_else(|| {
                panic!(
                    "broad legacy grant is narrowed off capture_mode: {:#?}",
                    plan.actions
                )
            });
        assert_eq!(plan.actions[narrowing].target, "runs.capture_mode");
        assert!(cutover < narrowing);
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
    fn populated_execution_pin_drift_refuses_before_every_other_action() {
        let mut obs = observation_at_record();
        obs.catalog_indexes.remove("release_flows_execution_bundle");
        obs.run_rows = 1;
        obs.release_flow_rows = 1;
        obs.scenario_author_role = None;

        let plan = plan_run_plane(&schema("demo"), &obs);
        assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
        assert_eq!(
            plan.actions[0].kind,
            RunPlaneActionKind::ExecutionPinCutover
        );
        assert!(plan.actions[0].sql.starts_with(
            "LOCK TABLE catalog.release_flows, \"demo\".runs IN ACCESS EXCLUSIVE MODE;"
        ));
    }

    #[test]
    fn populated_partial_execution_pin_schema_refuses_before_every_other_action() {
        let mut runs_only = observation_at_record();
        runs_only.catalog_tables.remove("release_flows");
        runs_only.column_types.insert(
            ("runs".to_string(), "catalog_version".to_string()),
            "bigint".to_string(),
        );
        runs_only.run_rows = 1;
        runs_only.scenario_author_role = None;

        let plan = plan_run_plane(&schema("demo"), &runs_only);
        assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
        let sql = &plan.actions[0].sql;
        assert_eq!(
            plan.actions[0].kind,
            RunPlaneActionKind::ExecutionPinCutover
        );
        assert!(sql.starts_with("LOCK TABLE \"demo\".runs IN ACCESS EXCLUSIVE MODE;"));
        assert!(sql.contains("EXISTS (SELECT 1 FROM \"demo\".runs)"));
        assert!(!sql.contains("ALTER TABLE") && !sql.contains("CREATE TABLE"));

        let mut release_only = observation_at_record();
        release_only.tables.remove("runs");
        release_only
            .catalog_indexes
            .remove("release_flows_execution_bundle");
        release_only.release_flow_rows = 1;
        release_only.scenario_author_role = None;

        let plan = plan_run_plane(&schema("demo"), &release_only);
        assert_eq!(plan.actions.len(), 1, "actions: {:#?}", plan.actions);
        let sql = &plan.actions[0].sql;
        assert_eq!(
            plan.actions[0].kind,
            RunPlaneActionKind::ExecutionPinCutover
        );
        assert!(sql.starts_with("LOCK TABLE catalog.release_flows IN ACCESS EXCLUSIVE MODE;"));
        assert!(sql.contains("EXISTS (SELECT 1 FROM catalog.release_flows)"));
        assert!(!sql.contains("ALTER TABLE") && !sql.contains("CREATE TABLE"));
    }

    #[test]
    fn empty_partial_execution_pin_schema_converges_in_one_plan() {
        let mut release_missing = observation_at_record();
        release_missing.catalog_tables.remove("release_flows");
        release_missing.column_types.insert(
            ("runs".to_string(), "catalog_version".to_string()),
            "bigint".to_string(),
        );
        let plan = plan_run_plane(&schema("demo"), &release_missing);
        let create = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::CreateCatalogTable
                    && action.target == "release_flows"
            })
            .expect("missing release_flows is created");
        let cutover = plan
            .actions
            .iter()
            .position(|action| action.kind == RunPlaneActionKind::ExecutionPinCutover)
            .expect("the existing runs table is converged after catalog creation");
        assert!(create < cutover, "actions: {:#?}", plan.actions);

        let mut runs_missing = observation_at_record();
        runs_missing.tables.remove("runs");
        runs_missing
            .catalog_indexes
            .remove("release_flows_execution_bundle");
        let plan = plan_run_plane(&schema("demo"), &runs_missing);
        let create = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::CreateTable && action.target == "runs"
            })
            .expect("missing runs is created");
        let cutover = plan
            .actions
            .iter()
            .position(|action| action.kind == RunPlaneActionKind::ExecutionPinCutover)
            .expect("the existing release membership is converged after run creation");
        assert!(create < cutover, "actions: {:#?}", plan.actions);
    }

    #[test]
    fn populated_current_single_target_creates_missing_peer_without_refusal() {
        let mut release_missing = observation_at_record();
        release_missing.catalog_tables.remove("release_flows");
        release_missing.run_rows = 1;
        let plan = plan_run_plane(&schema("demo"), &release_missing);
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::CreateCatalogTable
                && action.target == "release_flows"
        }));
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == RunPlaneActionKind::ExecutionPinCutover)
        );

        let mut runs_missing = observation_at_record();
        runs_missing.tables.remove("runs");
        runs_missing.release_flow_rows = 1;
        let plan = plan_run_plane(&schema("demo"), &runs_missing);
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::CreateTable && action.target == "runs"
        }));
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == RunPlaneActionKind::ExecutionPinCutover)
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
        policy.roles.insert("wamn_app".to_string());
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
        assert!(repair.sql.contains("FOR SELECT USING"));
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

    #[test]
    fn guest_column_select_on_authoring_test_reports_never_plans_false_clean() {
        let mut obs = observation_at_record();
        obs.authoring_effective_column_privileges
            .entry((
                "demo".to_string(),
                "authoring_test_reports".to_string(),
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
                    && action.target == "demo.authoring_test_reports"
            })
            .expect("column-level guest read authority must be surfaced");
        assert!(repair.sql.contains("DO $effective_acl$"));
        assert!(repair.sql.contains("pg_catalog.has_any_column_privilege"));
        assert!(
            repair
                .sql
                .contains("'wamn_app', '\"demo\".\"authoring_test_reports\"', 'SELECT'")
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
        let node_columns = obs.tables.get_mut("node_runs").expect("node table");
        for column in RETIRED_NODE_ATTEMPT_COLUMNS {
            node_columns.insert((*column).to_string());
        }
        obs.checks.insert(
            (
                "node_runs".to_string(),
                "node_runs_selected_recovery_class_check".to_string(),
            ),
            "CHECK (selected_recovery_class IS NULL OR selected_recovery_class <> ''::text)"
                .to_string(),
        );
        let attempt_columns = obs
            .tables
            .get_mut("effect_attempts")
            .expect("attempt table");
        for column in RETIRED_EFFECT_ATTEMPT_COLUMNS {
            attempt_columns.insert((*column).to_string());
        }
        for (table, column) in [
            ("node_runs", "attempt"),
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
                "node_runs".to_string(),
                "node_runs_current_effect_attempt_fk".to_string(),
            ),
            "legacy current pointer".to_string(),
        );
        obs.foreign_keys.insert(
            (
                "effect_attempts".to_string(),
                "effect_attempts_predecessor_fk".to_string(),
            ),
            "legacy predecessor".to_string(),
        );

        let plan = plan_run_plane(&schema("demo"), &obs);
        for column in RETIRED_NODE_ATTEMPT_COLUMNS {
            assert!(
                !plan
                    .extra_columns
                    .contains(&("node_runs".to_string(), (*column).to_string())),
                "cutover-owned projection column {column} must not be reported as preserved"
            );
        }
        assert!(!plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::DropExtraConstraint
                && action.target == "node_runs.node_runs_selected_recovery_class_check"
        }));
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
        assert!(action.sql.contains("LOCK TABLE \"demo\".\"node_runs\""));
        assert!(
            action
                .sql
                .contains("DROP CONSTRAINT IF EXISTS node_runs_current_effect_attempt_fk")
        );
        assert!(
            action
                .sql
                .contains("DROP CONSTRAINT IF EXISTS node_runs_selected_recovery_class_check")
        );
        assert!(
            action
                .sql
                .contains("DROP CONSTRAINT IF EXISTS effect_attempts_key_check")
        );
        assert!(action.sql.contains(r#"DROP COLUMN "attempt_index""#));
        for column in RETIRED_NODE_ATTEMPT_COLUMNS {
            assert!(
                action
                    .sql
                    .contains(&format!("DROP COLUMN {}", quote_ident(column))),
                "retired node projection column {column} survives: {}",
                action.sql
            );
        }
        assert!(!action.sql.contains("UPDATE "));
        assert!(!action.sql.contains("DELETE "));
        assert!(!action.sql.contains("INSERT INTO "));
    }

    #[test]
    fn populated_current_ledgers_do_not_block_projection_only_cleanup() {
        let mut obs = observation_at_record();
        obs.tables
            .get_mut("node_runs")
            .expect("node table")
            .insert("attempt_key".to_string());
        obs.effect_ledger_rows = 3;

        let plan = plan_run_plane(&schema("demo"), &obs);
        let action = plan
            .actions
            .iter()
            .find(|action| action.kind == RunPlaneActionKind::EffectWriterCutover)
            .expect("projection cleanup action");
        assert!(action.sql.contains("LOCK TABLE \"demo\".\"node_runs\""));
        assert!(
            !action
                .sql
                .contains("LOCK TABLE \"demo\".\"effect_attempts\"")
        );
        assert!(action.sql.contains("IF false THEN"));
        assert!(action.sql.contains(r#"DROP COLUMN "attempt_key""#));
        assert!(!action.sql.contains("ALTER TABLE \"demo\".effect_attempts"));
    }

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

    #[test]
    fn durable_test_report_foreign_key_drift_is_repaired() {
        let mut obs = observation_at_record();
        obs.foreign_keys.remove(&(
            "authoring_test_reports".to_string(),
            TEST_REPORT_RESERVATION_FK_NAME.to_string(),
        ));

        let repair = plan_run_plane(&schema("demo"), &obs)
            .actions
            .into_iter()
            .find(|action| {
                action.kind == RunPlaneActionKind::RepairForeignKey
                    && action.target
                        == "authoring_test_reports.authoring_test_report_reservation_fk"
            })
            .expect("report-to-reservation FK is repaired");
        assert!(
            repair
                .sql
                .contains("REFERENCES demo.authoring_test_run_reservations")
        );
    }

    #[test]
    fn check_and_index_only_attempt_residue_still_retires() {
        let mut obs = observation_at_record();
        obs.checks.insert(
            ("node_runs".to_string(), "node_runs_check3".to_string()),
            "CHECK (true)".to_string(),
        );

        let action = plan_run_plane(&schema("demo"), &obs)
            .actions
            .into_iter()
            .find(|action| action.kind == RunPlaneActionKind::DropExtraConstraint)
            .expect("residual retired node check is dropped independently");
        assert_eq!(action.target, "node_runs.node_runs_check3");
    }

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
        for column in NODE_FRAME_COLUMNS {
            obs.tables
                .get_mut("node_runs")
                .expect("node table")
                .remove(*column);
        }
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
        assert_eq!(action.target, "node_runs.effect_attempts.frame-identity");
        assert!(
            action
                .sql
                .contains("LOCK TABLE \"demo\".node_runs IN ACCESS EXCLUSIVE MODE")
        );
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
            action.sql.contains(
                "MESSAGE = 'frame-identity-cutover-requires-empty-node-and-effect-facts'"
            )
        );
        assert!(
            action.sql.find("RAISE EXCEPTION").expect("refusal")
                < action.sql.find("ALTER TABLE").expect("ddl"),
            "refusal must precede all DDL: {}",
            action.sql
        );
        assert!(
            action.sql.contains(
                "ADD PRIMARY KEY (tenant_id, run_id, frame_id, local_node_id, occurrence)"
            )
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
        for frame_column in NODE_FRAME_COLUMNS.iter().chain(EFFECT_FRAME_COLUMNS.iter()) {
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
    fn frame_identity_cutover_locks_only_present_drifted_tables() {
        for (case, remove_table, expected_lock, absent_lock) in [
            (
                "node-only",
                "effect_attempts",
                "LOCK TABLE \"demo\".node_runs",
                "LOCK TABLE \"demo\".effect_attempts",
            ),
            (
                "effect-only",
                "node_runs",
                "LOCK TABLE \"demo\".effect_attempts",
                "LOCK TABLE \"demo\".node_runs",
            ),
        ] {
            let mut obs = observation_at_record();
            obs.tables.remove(remove_table);
            if remove_table != "node_runs" {
                for column in NODE_FRAME_COLUMNS {
                    obs.tables
                        .get_mut("node_runs")
                        .expect("node table")
                        .remove(*column);
                }
            }
            if remove_table != "effect_attempts" {
                for column in EFFECT_FRAME_COLUMNS {
                    obs.tables
                        .get_mut("effect_attempts")
                        .expect("attempt table")
                        .remove(*column);
                }
            }

            let action = plan_run_plane(&schema("demo"), &obs)
                .actions
                .into_iter()
                .next()
                .expect(case);
            assert_eq!(action.kind, RunPlaneActionKind::FrameIdentityCutover);
            assert!(action.sql.contains(expected_lock), "{case}: {}", action.sql);
            assert!(
                !action.sql.contains(absent_lock),
                "{case} locks missing peer: {}",
                action.sql
            );
            assert!(
                action
                    .sql
                    .contains("frame-identity-cutover-requires-empty-node-and-effect-facts")
            );
            assert!(
                action.sql.find("RAISE EXCEPTION").expect("refusal")
                    < action.sql.find("ALTER TABLE").expect("ddl"),
                "{case}: refusal must precede DDL"
            );
        }
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
            "current node_runs plus missing peer must not false-refuse: {:#?}",
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
            reason = "the table-driven proof pairs each drift label with one noncapturing mutation"
        )]
        let cases: [(&str, fn(&mut RunPlaneObservation)); 5] = [
            ("wrong-type", |obs| {
                obs.column_types.insert(
                    ("node_runs".to_string(), "frame_id".to_string()),
                    "integer".to_string(),
                );
            }),
            ("wrong-nullability", |obs| {
                obs.non_nullable_columns.remove(&(
                    "effect_attempts".to_string(),
                    "requirement_name".to_string(),
                ));
            }),
            ("wrong-node-pk", |obs| {
                obs.indexes.insert(
                    "node_runs_pkey".to_string(),
                    "CREATE UNIQUE INDEX node_runs_pkey ON demo.node_runs USING btree (tenant_id, run_id, node_id, occurrence)".to_string(),
                );
            }),
            ("wrong-frame-check", |obs| {
                obs.checks.insert(
                    (
                        "node_runs".to_string(),
                        "node_runs_frame_relation_check".to_string(),
                    ),
                    "CHECK (frame_id >= 0)".to_string(),
                );
            }),
            ("legacy-node-id", |obs| {
                obs.tables
                    .get_mut("node_runs")
                    .expect("node table")
                    .insert("node_id".to_string());
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
            let node_target = matches!(
                case,
                "wrong-type" | "wrong-node-pk" | "wrong-frame-check" | "legacy-node-id"
            );
            let effect_target = matches!(case, "wrong-nullability" | "legacy-node-id");
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
            assert_eq!(
                action.sql.contains("LOCK TABLE \"demo\".node_runs"),
                node_target,
                "{case}: wrong node target: {}",
                action.sql
            );
            assert_eq!(
                action.sql.contains("LOCK TABLE \"demo\".effect_attempts"),
                effect_target,
                "{case}: wrong effect target: {}",
                action.sql
            );
            assert_eq!(
                action
                    .sql
                    .contains("ADD CONSTRAINT node_runs_frame_relation_check"),
                node_target,
                "{case}: wrong node repair: {}",
                action.sql
            );
            assert_eq!(
                action
                    .sql
                    .contains("ADD CONSTRAINT effect_attempts_source_artifact_check"),
                effect_target,
                "{case}: wrong effect repair: {}",
                action.sql
            );
            assert!(!plan.actions.iter().any(|planned| {
                planned.kind == RunPlaneActionKind::AddColumn
                    && matches!(
                        planned.target.as_str(),
                        "node_runs.frame_id" | "effect_attempts.requirement_name"
                    )
            }));
            assert!(!plan.actions.iter().any(|planned| {
                planned.kind == RunPlaneActionKind::RepairConstraint
                    && matches!(
                        planned.target.as_str(),
                        "node_runs.node_runs_frame_relation_check"
                            | "effect_attempts.effect_attempts_source_artifact_check"
                    )
            }));
            assert!(
                !plan.extra_columns.iter().any(|(table, column)| matches!(
                    table.as_str(),
                    "node_runs" | "effect_attempts"
                ) && column == "node_id"),
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
        obs.effect_ledger_rows = 1;
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
    fn effect_lineage_and_temporal_fks_are_repaired_on_existing_ledgers() {
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

    /// From zero (an empty database): the full run-plane set in FK order behind
    /// the schema ensure, plus the whole catalog schema — the fixture-wipe
    /// restore path (manifestations 3 + 5).
    #[test]
    fn from_zero_plans_the_full_set_in_order() {
        let obs = RunPlaneObservation::default();
        let plan = plan_run_plane(&schema("wamn_runner_demo"), &obs);
        let kinds: Vec<RunPlaneActionKind> = plan.actions.iter().map(|a| a.kind).collect();
        assert_eq!(kinds[0], RunPlaneActionKind::VerifyEffectWriterRole);
        assert_eq!(kinds[1], RunPlaneActionKind::VerifyRunProjectionWriterRole);
        assert_eq!(kinds[2], RunPlaneActionKind::EnsureScenarioAuthorRole);
        assert_eq!(kinds[3], RunPlaneActionKind::EnsureSchema);
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
                "invocation_admissions",
                "node_runs",
                "effect_attempts",
                "effect_attempt_dispatches",
                "effect_attempt_outcomes",
                "operator_run_actions",
                "authoring_test_run_reservations",
                "authoring_test_case_runs",
                "authoring_test_reports",
                "run_queue"
            ]
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == RunPlaneActionKind::EnsureCatalogSchema)
        );
        let orchestration_helper = plan
            .actions
            .iter()
            .position(|action| {
                action.kind == RunPlaneActionKind::RepairHelperFunction
                    && action.target == "guard_authoring_test_orchestration_write"
            })
            .expect("durable test orchestration helper is provisioned");
        for (table, trigger) in [
            (
                "authoring_test_run_reservations",
                "authoring_test_run_reservations_controlled_insert",
            ),
            (
                "authoring_test_case_runs",
                "authoring_test_case_runs_controlled_update",
            ),
            (
                "authoring_test_reports",
                "authoring_test_reports_update_immutable",
            ),
        ] {
            let table_position = plan
                .actions
                .iter()
                .position(|action| {
                    action.kind == RunPlaneActionKind::CreateTable && action.target == table
                })
                .expect("durable test orchestration table is provisioned");
            let trigger_position = plan
                .actions
                .iter()
                .position(|action| {
                    action.kind == RunPlaneActionKind::RepairTrigger
                        && action.target == format!("{table}.{trigger}")
                })
                .expect("post-helper durable orchestration trigger is provisioned");
            assert!(orchestration_helper < table_position);
            assert!(table_position < trigger_position);
        }
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
        obs.run_rows = 1;
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
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| { action.kind == RunPlaneActionKind::ExecutionPinCutover })
        );
        let add = plan
            .actions
            .iter()
            .find(|action| {
                action.kind == RunPlaneActionKind::AddColumn && action.target == "runs.capture_mode"
            })
            .expect("capture mode added independently of execution-pin cutover");
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
    fn broad_app_run_grants_are_narrowed_away_from_capture_mode() {
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
        let replacement_grant = repair
            .sql
            .split_once("GRANT INSERT (")
            .expect("replacement INSERT grant")
            .1
            .split_once("DO $run_capture_acl$")
            .expect("postcondition follows replacement grant")
            .0;
        assert!(!replacement_grant.contains("capture_mode"));
        assert!(!replacement_grant.contains("legacy_extra"));
        assert!(
            repair
                .sql
                .contains("run-capture-author-sql-write-authority")
        );
    }

    #[test]
    fn retired_node_capture_projection_is_cut_over_without_rewriting_rows() {
        let mut obs = observation_at_record();
        let columns = obs.tables.get_mut("node_runs").expect("node table");
        columns.remove("output_size");
        columns.insert(LEGACY_OUTPUT_SIZE_COLUMN.to_string());
        columns.extend(
            RETIRED_CAPTURE_PROJECTION_COLUMNS
                .iter()
                .map(|column| (*column).to_string()),
        );

        let plan = plan_run_plane(&schema("demo"), &obs);
        let cutover = plan
            .actions
            .iter()
            .find(|action| action.kind == RunPlaneActionKind::CaptureProjectionCutover)
            .expect("retired capture projection cutover");
        assert!(cutover.sql.contains("LOCK TABLE \"demo\".node_runs"));
        assert!(
            cutover
                .sql
                .contains("RENAME COLUMN payload_size TO output_size")
        );
        assert!(!plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::AddColumn && action.target == "node_runs.output_size"
        }));
        assert!(!plan.extra_columns.contains(&(
            "node_runs".to_string(),
            LEGACY_OUTPUT_SIZE_COLUMN.to_string()
        )));
        for column in RETIRED_CAPTURE_PROJECTION_COLUMNS {
            assert!(
                cutover
                    .sql
                    .contains(&format!("DROP COLUMN IF EXISTS {column}"))
            );
            assert!(
                !plan
                    .extra_columns
                    .contains(&("node_runs".to_string(), (*column).to_string()))
            );
        }
        assert!(!cutover.sql.contains("UPDATE"));
    }

    #[test]
    fn capture_projection_refuses_ambiguous_output_size_columns() {
        let mut obs = observation_at_record();
        obs.tables
            .get_mut("node_runs")
            .expect("node table")
            .insert(LEGACY_OUTPUT_SIZE_COLUMN.to_string());

        let plan = plan_run_plane(&schema("demo"), &obs);
        let cutover = plan
            .actions
            .iter()
            .find(|action| action.kind == RunPlaneActionKind::CaptureProjectionCutover)
            .expect("ambiguous output-size projection refuses");
        assert!(cutover.sql.contains("capture-output-size-columns-conflict"));
        assert!(!cutover.sql.contains("RENAME COLUMN"));
        assert!(!plan.extra_columns.contains(&(
            "node_runs".to_string(),
            LEGACY_OUTPUT_SIZE_COLUMN.to_string()
        )));
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
    fn cancelled_node_error_check_is_repaired() {
        let mut obs = observation_at_record();
        obs.checks.insert(
            (
                "node_runs".to_string(),
                "node_runs_error_kind_check".to_string(),
            ),
            "CHECK (error_kind = ANY (ARRAY['retryable'::text, 'rate-limited'::text, 'terminal'::text, 'invalid-input'::text, 'cancelled'::text]))"
                .to_string(),
        );

        let plan = plan_run_plane(&schema("demo"), &obs);
        let repairs = plan
            .actions
            .iter()
            .filter(|action| action.kind == RunPlaneActionKind::RepairConstraint)
            .collect::<Vec<_>>();
        assert_eq!(repairs.len(), 1, "only the node error CHECK: {repairs:#?}");
        let repair = repairs[0];
        assert_eq!(repair.target, "node_runs.node_runs_error_kind_check");
        assert!(
            repair
                .sql
                .contains("DROP CONSTRAINT \"node_runs_error_kind_check\"")
        );
        assert!(repair.sql.contains("'invalid-input'::text"));
        assert!(!repair.sql.contains("'cancelled'::text"));
    }

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
        for table in RETIRED_TEST_SET_REFERENCE_TABLES {
            assert!(
                !record_columns(AUTHORING_TESTS_SQL, "wamn_run", table)
                    .iter()
                    .any(|(column, _)| column == RETIRED_TEST_SET_REFERENCE_COLUMN),
                "{table} still records {RETIRED_TEST_SET_REFERENCE_COLUMN}"
            );
        }
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
        for source in [AUTHORING_TESTS_SQL, select_run_plane_helper_functions_sql()] {
            assert!(!source.contains("authoring_test_sets"), "{source}");
        }
    }

    #[test]
    fn durable_test_orchestration_is_in_the_schema_control_record() {
        for (table, check_count, author_privileges) in [
            (
                "authoring_test_run_reservations",
                10,
                &["SELECT", "INSERT", "UPDATE"][..],
            ),
            (
                "authoring_test_case_runs",
                13,
                &["SELECT", "INSERT", "UPDATE"][..],
            ),
            ("authoring_test_reports", 6, &["SELECT", "INSERT"][..]),
        ] {
            assert_eq!(
                CHECK_SPECS
                    .iter()
                    .filter(|spec| spec.table == table)
                    .count(),
                check_count,
                "{table} CHECK inventory drifted"
            );
            let privileges = AUTHORING_PRIVILEGE_SPECS
                .iter()
                .find(|spec| {
                    matches!(spec.schema, AuthoringTableSchema::RunPlane) && spec.table == table
                })
                .unwrap_or_else(|| panic!("missing privilege record for {table}"));
            assert!(privileges.app.is_empty());
            assert_eq!(privileges.author, author_privileges);
        }

        let helper_names = helper_specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<BTreeSet<_>>();
        assert!(helper_names.contains("guard_authoring_test_orchestration_write"));
        assert!(helper_names.contains("reject_immutable_authoring_test_orchestration_change"));

        let triggers = trigger_specs();
        for name in [
            "authoring_test_run_reservations_controlled_insert",
            "authoring_test_case_runs_controlled_update",
            "authoring_test_reports_update_immutable",
        ] {
            assert!(triggers.iter().any(|trigger| trigger.name == name));
        }
        assert_eq!(
            TEST_REPORT_RESERVATION_FK_DEF,
            "FOREIGN KEY (tenant_id, report_id) REFERENCES wamn_run.authoring_test_run_reservations(tenant_id, report_id)"
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
            9
        );
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairHelperFunction
                && action.target == "pin_run_durability_class"
        }));
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
                && action.target == "runs.runs_pin_durability_class"
        }));
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
            21
        );
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairTrigger
                && action.target == "authoring_test_reports.authoring_test_reports_delete_immutable"
        }));
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
        assert!(
            RUN_STATE_SQL
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
        obs.effect_ledger_effective_privileges.insert(
            (
                "effect_attempts".to_string(),
                SCENARIO_AUTHOR_ROLE.to_string(),
            ),
            ["SELECT".to_string()].into_iter().collect(),
        );
        obs.effect_ledger_effective_column_privileges
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
        assert!(table_action.sql.contains("GRANT SELECT, INSERT"));
    }

    #[test]
    fn node_projection_acl_repair_removes_app_and_indirect_mutation() {
        let mut obs = observation_at_record();
        obs.run_projection_schema_privileges = (true, true);
        obs.node_runs_table_privileges
            .entry("wamn_app".to_string())
            .or_default()
            .insert("UPDATE".to_string());
        obs.node_runs_effective_privileges
            .entry(SCENARIO_AUTHOR_ROLE.to_string())
            .or_default()
            .insert("DELETE".to_string());
        obs.node_runs_effective_column_privileges
            .entry(EFFECT_WRITER_ROLE.to_string())
            .or_default()
            .insert("UPDATE".to_string());
        obs.node_runs_table_privileges.insert(
            "rogue_direct".to_string(),
            ["INSERT".to_string()].into_iter().collect(),
        );
        obs.node_runs_column_privileges.insert(
            "rogue_column".to_string(),
            ["UPDATE".to_string()].into_iter().collect(),
        );
        obs.node_runs_effective_privileges.insert(
            "rogue_member".to_string(),
            ["DELETE".to_string()].into_iter().collect(),
        );
        obs.node_runs_effective_column_privileges.insert(
            "rogue_member".to_string(),
            ["UPDATE".to_string()].into_iter().collect(),
        );

        let plan = plan_run_plane(&schema("demo"), &obs);
        let schema_action = plan
            .actions
            .iter()
            .find(|action| action.target == "demo.projection-usage")
            .expect("projection schema ACL repair");
        assert!(schema_action.sql.contains("wamn_run_projection_writer"));
        let table_action = plan
            .actions
            .iter()
            .find(|action| action.target == "demo.node_runs")
            .expect("node projection ACL repair");
        assert!(table_action.sql.contains("GRANT SELECT ON TABLE"));
        assert!(table_action.sql.contains("TO wamn_app"));
        assert!(
            table_action
                .sql
                .contains("GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE")
        );
        assert!(table_action.sql.contains("TO wamn_run_projection_writer"));
        assert!(table_action.sql.contains("\"rogue_direct\""));
        assert!(table_action.sql.contains("\"rogue_column\""));
        assert!(
            table_action
                .sql
                .contains("FROM pg_catalog.pg_roles AS actor")
        );
        assert!(table_action.sql.contains("actor.rolname !~"));
        assert!(
            table_action
                .sql
                .contains("node-runs-projection-privilege-out-of-bounds")
        );
    }

    #[test]
    fn node_projection_sequence_widens_from_integer_to_bigint() {
        let mut obs = observation_at_record();
        obs.column_types.insert(
            ("node_runs".to_string(), "seq".to_string()),
            "integer".to_string(),
        );

        let action = plan_run_plane(&schema("demo"), &obs)
            .actions
            .into_iter()
            .find(|action| action.kind == RunPlaneActionKind::WidenNodeRunSequence)
            .expect("legacy projection sequence widening");
        assert_eq!(action.target, "node_runs.seq");
        assert_eq!(
            action.sql,
            "ALTER TABLE \"demo\".node_runs ALTER COLUMN seq TYPE bigint USING seq::bigint"
        );
    }

    #[test]
    fn projection_role_is_acl_only_and_accepts_only_scoped_generation_members() {
        let mut obs = observation_at_record();
        obs.run_projection_writer_role = None;
        let action = plan_run_plane(&schema("demo"), &obs)
            .actions
            .into_iter()
            .find(|action| action.kind == RunPlaneActionKind::VerifyRunProjectionWriterRole)
            .expect("projection role verification");
        assert!(action.sql.contains("NOT rolcanlogin"));
        assert!(action.sql.contains("NOT rolinherit"));
        assert!(action.sql.contains("has_database_privilege"));
        assert!(action.sql.contains("relowner = role_oid"));
        assert!(
            action
                .sql
                .contains("^wamn_effect_writer_[0-9a-f]{40}_[ab]$")
        );
        for evidence in [
            "'wamn_effect_writer', 'wamn_run_projection_writer'",
            "edge.admin_option OR NOT edge.inherit_option",
            "WHERE edge.roleid = generation.oid",
            "dependency.deptype = 'o'",
            "current_database(), 'CONNECT')) > 2",
            "count(DISTINCT substring(",
        ] {
            assert!(action.sql.contains(evidence), "missing {evidence}");
        }
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
        obs.effect_ledger_effective_column_privileges
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

    /// The dot-anchored rewrite (relocated from publish_catalog as the single
    /// owner): qualified names + the schema header rewrite; prose does not.
    #[test]
    fn schema_rewrite_is_dot_anchored() {
        let schema = schema("poc_f1");
        // `run-state.sql` is the ONLY record file production rewrites whole
        // (`publish_catalog::ensure_runstate`) and the only one carrying the
        // schema header; the legacy flow registry (fixture-only) was the second
        // input to this sweep until wamn-0h0g.12.102 (e45ca35b) deleted it with
        // its call site.
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
        assert!(rewrite_schema(RUN_STATE_SQL, &schema).contains("CREATE TABLE poc_f1.node_runs"));
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
        assert!(select_run_capture_privileges_sql().contains("has_table_privilege"));
        assert!(select_run_capture_privileges_sql().contains("has_column_privilege"));
        assert!(select_run_capture_privileges_sql().contains("capture_mode"));
        let writer_tables = select_effect_writer_run_table_privileges_sql();
        assert!(writer_tables.contains("has_table_privilege"));
        assert!(writer_tables.contains("relation.relname IN ('runs', 'run_queue')"));
        let writer_columns = select_effect_writer_run_column_privileges_sql();
        assert!(writer_columns.contains("has_column_privilege"));
        assert!(writer_columns.contains("attribute.attnum > 0"));
        assert!(select_run_projection_writer_role_sql().contains("wamn_run_projection_writer"));
        assert!(select_run_projection_schema_privileges_sql().contains("has_schema_privilege"));
        let direct_projection = select_node_runs_table_privileges_sql();
        assert!(direct_projection.contains("node_runs"));
        assert!(!direct_projection.contains("grantee IN"));
        assert!(select_node_runs_column_privileges_sql().contains("attribute.attacl"));
        let effective_projection = select_node_runs_effective_privileges_sql();
        assert!(effective_projection.contains("has_table_privilege"));
        assert!(effective_projection.contains("NOT actor.rolsuper"));
        assert!(!effective_projection.contains("actor.rolname IN"));
        assert!(
            select_node_runs_effective_column_privileges_sql().contains("has_any_column_privilege")
        );
        assert!(select_authoring_table_privileges_sql().contains("draft_safe_connection_grants"));
        assert!(!select_authoring_table_privileges_sql().contains("authoring_report_reservations"));
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
            assert!(
                observation.contains("authoring_test_reports"),
                "{observation}"
            );
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
        assert!(select_run_plane_helper_functions_sql().contains("guard_authoring_report_write"));
        assert!(
            select_run_plane_helper_functions_sql()
                .contains("reject_immutable_authoring_test_orchestration_change")
        );
        assert!(
            select_run_plane_helper_functions_sql()
                .contains("guard_authoring_test_orchestration_write")
        );
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
}
