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
        definition: "CHECK (status = ANY (ARRAY['dispatched'::text, 'running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text, 'infrastructure-failure'::text]))",
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
        table: "cron_anchor",
        name: "cron_anchor_tenant_id_check",
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
        definition: "CHECK (status = ANY (ARRAY['started'::text, 'parked'::text, 'success'::text, 'error'::text]))",
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

const LOCK_CATALOG_HEAD_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.lock_catalog_head(p_tenant_id text, p_catalog_id text, p_environment text)\n RETURNS integer\n LANGUAGE plpgsql\n SECURITY DEFINER\n SET search_path TO 'pg_catalog', 'catalog'\nAS $function$\nDECLARE\n    applied_version int;\nBEGIN\n    SELECT head.applied_catalog_version INTO applied_version\n    FROM catalog.catalog_heads AS head\n    WHERE p_tenant_id = NULLIF(current_setting('app.tenant', true), '')\n      AND head.tenant_id = p_tenant_id\n      AND head.catalog_id = p_catalog_id\n      AND head.environment = p_environment\n    FOR KEY SHARE OF head;\n    RETURN applied_version;\nEND\n$function$\n";

const GUARD_EVENT_LINEAGE_DEF: &str = "CREATE OR REPLACE FUNCTION wamn_run.guard_event_lineage_immutable()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n    IF NEW.event_source_run_id IS DISTINCT FROM OLD.event_source_run_id\n       OR NEW.event_root_run_id IS DISTINCT FROM OLD.event_root_run_id\n       OR NEW.event_depth IS DISTINCT FROM OLD.event_depth THEN\n        RAISE EXCEPTION 'event causation lineage is immutable';\n    END IF;\n    RETURN NEW;\nEND\n$function$\n";

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
    FOR KEY SHARE OF head;
    RETURN applied_version;
END
$$;
REVOKE ALL ON FUNCTION wamn_run.lock_catalog_head(text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION wamn_run.lock_catalog_head(text, text, text) TO wamn_app;"#;

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

const RUNS_EVENT_LINEAGE_TRIGGER_SQL: &str = "CREATE TRIGGER runs_event_lineage_immutable \
    BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth \
    ON wamn_run.runs FOR EACH ROW EXECUTE FUNCTION \
    wamn_run.guard_event_lineage_immutable();";

/// The run-plane record files in APPLY ORDER: run-state first (schema header +
/// `runs`, which everything FKs), then the flow registry, then the 11.2 flow
/// test-suite tables (FK to `flows`, so AFTER it), then the queue.
const RUN_PLANE_FILES: [&str; 4] = [RUN_STATE_SQL, FLOWS_SQL, FLOW_TESTS_SQL, RUN_QUEUE_SQL];

/// The outbox-era tables the l5i9.19 teardown retired. A pre-teardown schema
/// (or one restored from a pre-teardown snapshot) still carries them.
pub const LEGACY_OUTBOX_TABLES: [&str; 2] = ["outbox", "evt_shadow"];

/// The constant trigger AND function name the retired wamn-schema-compiler outbox emission
/// used (`CREATE OR REPLACE TRIGGER wamn_outbox_event … EXECUTE FUNCTION
/// wamn_outbox_event()`, one trigger per entity table, the function unqualified
/// so it landed in the apply-time schema).
pub const OUTBOX_TRIGGER_NAME: &str = "wamn_outbox_event";

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
    /// Rows in `catalog.event_registrations` still carrying the legacy `state`
    /// key (0 when the table is absent — nothing to strip).
    pub stale_registration_state_rows: i64,
    /// Every CHECK constraint on a record table, keyed by `(table, name)`, with
    /// PostgreSQL's canonical `pg_get_constraintdef(..., true)` definition.
    pub checks: BTreeMap<(String, String), String>,
    /// Every non-internal trigger on a record table, keyed by `(table, name)`,
    /// with PostgreSQL's canonical `pg_get_triggerdef(..., true)` definition.
    pub triggers: BTreeMap<(String, String), String>,
    /// Canonical `pg_get_functiondef` output for the two run-state helper
    /// functions, keyed by function name.
    pub helper_functions: BTreeMap<String, String>,
}

/// What one plan action does (for reporting; the SQL is on the action).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPlaneActionKind {
    /// `CREATE SCHEMA IF NOT EXISTS` + role usage grant (the run-state.sql
    /// header, rewritten) — emitted once when any run-plane table is missing.
    EnsureSchema,
    /// Create a missing run-plane table from its record section.
    CreateTable,
    /// Add a record column missing from a present table.
    AddColumn,
    /// Drop/re-add a drifted record CHECK, or add it when absent.
    RepairConstraint,
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
    plan.actions.extend(creates);

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

    // 2c. The functions and trigger are part of the run-state contract, not an
    // incidental side effect of creating a missing table. CREATE OR REPLACE
    // repairs function-body drift without dropping dependants. A missing runs
    // table gets the guard + trigger from its canonical table section.
    if obs
        .helper_functions
        .get("lock_catalog_head")
        .is_none_or(|def| normalize_observed_schema(def, schema) != LOCK_CATALOG_HEAD_DEF)
    {
        plan.actions.push(RunPlaneAction {
            kind: RunPlaneActionKind::RepairHelperFunction,
            target: "lock_catalog_head".to_string(),
            sql: rewrite_schema(LOCK_CATALOG_HEAD_SQL, schema),
        });
    }
    if obs.tables.contains_key("runs") {
        if obs
            .helper_functions
            .get("guard_event_lineage_immutable")
            .is_none_or(|def| normalize_observed_schema(def, schema) != GUARD_EVENT_LINEAGE_DEF)
        {
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairHelperFunction,
                target: "guard_event_lineage_immutable".to_string(),
                sql: rewrite_schema(GUARD_EVENT_LINEAGE_SQL, schema),
            });
        }
        let trigger_key = (
            "runs".to_string(),
            "runs_event_lineage_immutable".to_string(),
        );
        if obs.triggers.get(&trigger_key).is_none_or(|def| {
            normalize_observed_schema(def, schema) != RUNS_EVENT_LINEAGE_TRIGGER_DEF
        }) {
            let drop = if obs.triggers.contains_key(&trigger_key) {
                format!(
                    "DROP TRIGGER {} ON {}.{}; ",
                    quote_ident("runs_event_lineage_immutable"),
                    schema.quoted(),
                    quote_ident("runs"),
                )
            } else {
                String::new()
            };
            plan.actions.push(RunPlaneAction {
                kind: RunPlaneActionKind::RepairTrigger,
                target: "runs.runs_event_lineage_immutable".to_string(),
                sql: format!(
                    "{drop}{}",
                    rewrite_schema(RUNS_EVENT_LINEAGE_TRIGGER_SQL, schema)
                ),
            });
        }
    }
    for (table, name) in obs.triggers.keys() {
        let is_record_trigger = table == "runs" && name == "runs_event_lineage_immutable";
        if record_table_names().contains(table.as_str())
            && !is_record_trigger
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

    // 5. The `catalog` metadata schema (per-database, NOT schema-rewritten):
    //    absent → the whole record file (its CREATE SCHEMA is unguarded);
    //    present → per-table sections for what is missing, in file order.
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
/// `stream_seq` — not a general definition differ.
fn index_definition_stale(file: &str, table: &str, record_stmt: &str, live_def: &str) -> bool {
    let record_tokens = ident_tokens(record_stmt);
    let live_tokens = ident_tokens(live_def);
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
    definition.replace(&format!("{}.", schema.as_str()), "wamn_run.")
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

/// Every non-internal trigger in `$1`: `(table, name, canonical def)`.
pub fn select_schema_triggers_sql() -> &'static str {
    "SELECT c.relname, t.tgname, pg_get_triggerdef(t.oid, true) \
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
       AND p.proname IN ('lock_catalog_head', 'guard_event_lineage_immutable') \
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
    ddl.replace("wamn_run.", &format!("{schema}."))
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
/// `CREATE TABLE <qualifier>.` line or EOF — the table body plus its indexes,
/// RLS enablement, policy, and grants. Leading comment banners belong to the
/// PREVIOUS section (they are comments; nothing is lost).
fn table_section(src: &str, qualifier: &str, table: &str) -> String {
    let head = format!("CREATE TABLE {qualifier}.{table} (");
    let any_head = format!("CREATE TABLE {qualifier}.");
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
        if t.starts_with(&any_head) {
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
            ..Default::default()
        };
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
        obs.catalog_tables = record_tables(CATALOG_SCHEMA_SQL, "catalog")
            .into_iter()
            .collect();
        for spec in CHECK_SPECS {
            obs.checks.insert(
                (spec.table.to_string(), spec.name.to_string()),
                spec.definition.to_string(),
            );
        }
        obs.helper_functions.insert(
            "lock_catalog_head".to_string(),
            LOCK_CATALOG_HEAD_DEF.to_string(),
        );
        obs.helper_functions.insert(
            "guard_event_lineage_immutable".to_string(),
            GUARD_EVENT_LINEAGE_DEF.to_string(),
        );
        obs.triggers.insert(
            (
                "runs".to_string(),
                "runs_event_lineage_immutable".to_string(),
            ),
            RUNS_EVENT_LINEAGE_TRIGGER_DEF.to_string(),
        );
        obs
    }

    #[test]
    fn record_tables_are_pinned() {
        assert_eq!(
            record_tables(RUN_STATE_SQL, "wamn_run"),
            ["runs", "invocation_admissions", "cron_anchor", "node_runs"]
        );
        assert_eq!(record_tables(FLOWS_SQL, "wamn_run"), ["flows"]);
        assert_eq!(
            record_tables(FLOW_TESTS_SQL, "wamn_run"),
            ["test_suites", "test_cases"]
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
        ] {
            assert!(catalog.contains(&connection_table.to_string()));
        }
        assert_eq!(
            catalog.len(),
            25,
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
        assert!(
            status.ends_with("))"),
            "CHECK closes inside the definition: {status}"
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
                "flows_active",
                "flows_active_webhook_path",
                "invocation_admissions_expiry",
                "invocation_admissions_run",
                "node_runs_seq",
                "run_queue_claimable",
                "run_queue_partition",
                "runs_cancel_requested",
                "runs_cron_anchor",
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
            10,
            "all ten run-plane tables at target (incl. invocation admission and test suites)"
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

    /// From zero (an empty database): the full run-plane set in FK order behind
    /// the schema ensure, plus the whole catalog schema — the fixture-wipe
    /// restore path (manifestations 3 + 5).
    #[test]
    fn from_zero_plans_the_full_set_in_order() {
        let obs = RunPlaneObservation::default();
        let plan = plan_run_plane(&schema("wamn_runner_demo"), &obs);
        let kinds: Vec<RunPlaneActionKind> = plan.actions.iter().map(|a| a.kind).collect();
        assert_eq!(kinds[0], RunPlaneActionKind::EnsureSchema);
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
                "cron_anchor",
                "node_runs",
                "flows",
                "test_suites",
                "test_cases",
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
        // No column/index repairs on tables being created (sections carry them).
        assert!(!kinds.contains(&RunPlaneActionKind::AddColumn));
        assert!(!kinds.contains(&RunPlaneActionKind::CreateIndex));
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
        obs.checks
            .remove(&("node_runs".to_string(), "node_runs_check".to_string()));

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
            action.target == "node_runs.node_runs_check"
                && !action.sql.contains("DROP CONSTRAINT")
                && action.sql.contains("attempt_input_ref IS NOT NULL")
        }));
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
    fn missing_helpers_and_trigger_are_repaired_for_present_runs() {
        let mut obs = observation_at_record();
        obs.helper_functions.clear();
        obs.triggers.clear();
        let plan = plan_run_plane(&schema("demo"), &obs);
        assert_eq!(
            plan.actions
                .iter()
                .filter(|action| action.kind == RunPlaneActionKind::RepairHelperFunction)
                .count(),
            2
        );
        assert!(plan.actions.iter().any(|action| {
            action.kind == RunPlaneActionKind::RepairTrigger
                && action.target == "runs.runs_event_lineage_immutable"
        }));
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
        assert!(select_schema_triggers_sql().contains("NOT t.tgisinternal"));
        assert!(select_run_plane_helper_functions_sql().contains("pg_get_functiondef"));
        assert_eq!(
            strip_registration_state_sql(),
            "UPDATE catalog.event_registrations SET registration = registration - 'state' \
             WHERE registration ? 'state'"
        );
    }
}
