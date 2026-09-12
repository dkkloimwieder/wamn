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

mod declarations;
mod schema_changes;
mod plan;

#[doc(inline)]
pub use plan::plan_run_plane;

#[cfg(test)]
use declarations::{
    AUTHORING_PRIVILEGE_SPECS, AuthoringTableSchema, CHECK_SPECS,
    EFFECT_ATTEMPTS_DISPATCH_IDENTITY_KEY_DEF, EFFECT_DISPATCHES_OCCURRENCE_KEY_DEF,
    EFFECT_DISPATCH_ATTEMPT_FK_DEF, EFFECT_DISPATCH_ATTEMPT_FK_NAME, EFFECT_FRAME_COLUMNS,
    EFFECT_OUTCOME_DISPATCH_FK_DEF, EFFECT_OUTCOME_DISPATCH_FK_NAME,
    EFFECT_WRITER_RUN_READ_COLUMNS, REJECT_IMMUTABLE_OPERATOR_RUN_ACTION_CHANGE_SQL,
    RETIRED_EFFECT_ATTEMPT_COLUMNS, RUNS_ADMISSION_PINS_TRIGGER_DEF,
    RUNS_ADMISSION_PINS_TRIGGER_SQL, RUNS_RELEASE_FK_DEF, RUNS_RELEASE_INDEX_DEF,
    RUNS_ROOT_INDEX_DEF, helper_specs, trigger_specs,
};
#[cfg(test)]
use schema_changes::{
    RETIRED_AUTHORED_ORDERING_REFUSAL, RETIRED_CHILD_RUN_COLUMNS, RETIRED_CHILD_RUN_INDEXES,
    RETIRED_DEAD_LETTER_REFUSAL, RETIRED_EXECUTION_BUNDLE_COLUMN,
    RETIRED_FAILURE_DETAIL_COLUMNS, RETIRED_PARTITION_CHECK, RETIRED_PARTITION_COLUMNS,
    RETIRED_PARTITION_INDEX, RETIRED_RERUN_LINEAGE_COLUMNS, RETIRED_STORED_SUITE_CATALOG_TABLE,
    RETIRED_STORED_SUITE_FUNCTIONS, RETIRED_STORED_SUITE_TABLES,
    RETIRED_TEST_SET_REFERENCE_COLUMN, RETIRED_TEST_SET_REFERENCE_TABLES,
    postgres_visible_identifier,
};
#[cfg(test)]
use plan::environment_policy_row_security_at_record;
#[cfg(test)]
use schema::{
    header_section, index_statements, quote_ident, record_columns, record_tables,
    table_section,
};
#[cfg(test)]
use std::collections::BTreeSet;
#[cfg(test)]
use wamn_catalog::CATALOG_SCHEMA_SQL;

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
    select_effect_table_effective_column_privileges_sql,
    select_effect_table_effective_privileges_sql, select_effect_table_privileges_sql,
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

/// The schema of record, compiled in — the same sources provisioning applies
/// (project-environment provisioning and the f1 provisioning Job) and the wamn-9mg8
/// stand-in drift guard pins.
const RUN_STATE_SQL: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../deploy/sql/run-queue.sql");

/// The run-plane record files in APPLY ORDER: run-state first (schema header +
/// `runs`, which everything FKs), then the queue.
const RUN_PLANE_FILES: [&str; 2] = [RUN_STATE_SQL, RUN_QUEUE_SQL];

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
    /// Strict empty-only installation of the coordinate-bound writer tables.
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

impl RunPlanePlan {
    /// Whether there is anything to apply (a no-op reconcile is the expected
    /// steady state and worth reporting as such).
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }
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
