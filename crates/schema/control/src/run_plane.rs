//! Plan run-plane creation and repair from observed database facts.
//!
//! The reconciler creates each plane from the current deploy SQL.
//! Current planes retain their rows while schema and privilege repairs converge.

mod declarations;
mod plan;

#[doc(inline)]
pub use plan::plan_run_plane;

#[cfg(test)]
use declarations::{
    AUTHORING_PRIVILEGE_SPECS, AuthoringTableSchema, CHECK_SPECS,
    EFFECT_ATTEMPTS_DISPATCH_IDENTITY_KEY_DEF, EFFECT_DISPATCH_ATTEMPT_FK_DEF,
    EFFECT_DISPATCH_ATTEMPT_FK_NAME, EFFECT_DISPATCHES_OCCURRENCE_KEY_DEF,
    EFFECT_OUTCOME_DISPATCH_FK_DEF, EFFECT_OUTCOME_DISPATCH_FK_NAME,
    RUNS_ADMISSION_PINS_TRIGGER_DEF, RUNS_ADMISSION_PINS_TRIGGER_SQL, RUNS_RELEASE_FK_DEF,
    helper_specs, trigger_specs,
};
#[cfg(test)]
use plan::environment_policy_row_security_at_record;
#[cfg(test)]
use schema::{index_statements, record_columns, record_tables, table_section};
#[cfg(test)]
use std::collections::BTreeSet;
#[cfg(test)]
use wamn_catalog::CATALOG_SCHEMA_SQL;

mod observation;
mod observation_sql;
mod schema;

#[doc(inline)]
pub use observation::{
    RowPolicyObservation, RowSecurityObservation, RunPlaneObservation,
    ScenarioAuthorRoleObservation,
};
#[doc(inline)]
pub use observation_sql::{
    catalog_schema_present_sql, select_app_run_queue_authority_sql,
    select_app_scenario_author_membership_sql, select_authoring_effective_column_privileges_sql,
    select_authoring_effective_table_privileges_sql, select_authoring_table_owners_sql,
    select_authoring_table_privileges_sql, select_effect_table_effective_column_privileges_sql,
    select_effect_table_effective_privileges_sql, select_effect_table_privileges_sql,
    select_environment_policy_policies_sql, select_environment_policy_row_security_sql,
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

/// Reserved non-login project-author identity. No production credential inherits
/// it, and the guest-visible `wamn_app` role is never a member.
pub const SCENARIO_AUTHOR_ROLE: &str = "wamn_scenario_author";
/// What one plan action does (for reporting; the SQL is on the action).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPlaneActionKind {
    /// Refuse incompatible drift on a populated current effect ledger.
    RefuseSchemaDrift,
    /// Create or harden the host-only `wamn_scenario_author` NOLOGIN role.
    EnsureScenarioAuthorRole,
    /// `CREATE SCHEMA IF NOT EXISTS` + role usage grant (the run-state.sql
    /// header, rewritten) — emitted once when any run-plane table is missing.
    EnsureSchema,
    /// Create a missing run-plane table from its record section.
    CreateTable,
    /// Add a record column missing from a present table.
    AddColumn,
    /// Converge exact effect-table ACLs and deny guest and author writes.
    RepairEffectTablePrivilege,
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
    /// column.
    RecreateIndex,
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
    /// Remove every guest-visible table and column privilege on `run_queue`.
    RemoveAppRunQueueAuthority,
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
/// not know (SURFACED and preserved). Idempotent: planning against
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

/// Current helper definitions in `$1`.
pub fn select_run_plane_helper_functions_sql() -> &'static str {
    "SELECT p.proname, pg_get_functiondef(p.oid) \
     FROM pg_proc p \
     JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = $1 \
       AND p.proname IN ('guard_event_lineage_immutable', \
                         'reject_immutable_effect_fact_change', \
                         'reject_immutable_operator_run_action_change', \
                         'guard_run_admission_pins_immutable', \
                         'guard_terminal_run_delete') \
     ORDER BY p.proname"
}

#[cfg(test)]
mod tests;
