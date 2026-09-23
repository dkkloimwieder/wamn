//! Describe the schema facts observed from a project database.

use std::collections::{BTreeMap, BTreeSet};

/// Security attributes observed for the host-only scenario author role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the seven fields mirror the seven `pg_roles` attribute columns one for one \
              (rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolinherit, rolreplication, \
              rolbypassrls); PostgreSQL reports them as booleans and a faithful copy of what the \
              server says is worth more here than a state machine"
)]
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
    pub(super) fn is_host_only(self) -> bool {
        !self.can_login
            && !self.is_superuser
            && !self.can_create_database
            && !self.can_create_role
            && !self.inherits_roles
            && !self.can_replicate
            && !self.bypasses_rls
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

/// What the driver observed live, scoped to ONE project-env schema (plus the
/// per-database `catalog` metadata schema). Everything here is a read — the
/// pure planner turns it into the action list.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each boolean is one independent fact the driver read from the live database \
              (a role membership, a granted authority, a present role, a present function, a \
              present schema); they are observations, not a state machine, and grouping them \
              would hide which single query answered which"
)]
pub struct RunPlaneObservation {
    /// Total immutable rows across the three effect tables that exist.
    /// Any nonzero value makes an incompatible structural cutover refuse.
    pub effect_record_rows: i64,
    /// Persisted flow graphs that still carry retired top-level ordering keys.
    pub retired_authored_ordering_rows: i64,
    /// Host-only scenario-author role attributes, or absent when the cluster
    /// has not yet provisioned the role.
    pub scenario_author_role: Option<ScenarioAuthorRoleObservation>,
    /// Direct table grants keyed by `(table, grantee)`.
    pub effect_table_privileges: BTreeMap<(String, String), BTreeSet<String>>,
    /// Effective table grants keyed by `(table, grantee)`.
    pub effect_table_effective_privileges: BTreeMap<(String, String), BTreeSet<String>>,
    /// Effective column grants keyed by `(table, grantee)`.
    pub effect_table_effective_column_privileges: BTreeMap<(String, String), BTreeSet<String>>,
    /// Owners keyed by table.
    pub effect_table_owners: BTreeMap<String, String>,
    /// Whether guest-visible `wamn_app` inherits the host-only author role.
    pub app_is_scenario_author_member: bool,
    /// Revocable table or column authority on `run_queue` held directly by
    /// `wamn_app` or inherited from `PUBLIC`.
    pub app_run_queue_authority: bool,
    /// Effective `wamn_app` authority on the retired run-write surface. The
    /// first value detects table-level INSERT/UPDATE, the second detects the
    /// historical `capture_mode` carrier specifically, and the third shows
    /// no run column remains writable by the app role.
    pub app_run_capture_privileges: (bool, bool, bool),
    /// Direct table grants for the authoring-state security surface, keyed by
    /// `(schema, table, grantee)` and containing uppercase privilege names.
    pub authoring_table_privileges: BTreeMap<(String, String, String), BTreeSet<String>>,
    /// Effective table privileges (direct, inherited, or ownership-derived)
    /// for the two security-boundary roles on every managed authoring table.
    pub authoring_effective_table_privileges: BTreeMap<(String, String, String), BTreeSet<String>>,
    /// Effective mutation/reference authority on any column. PostgreSQL keeps
    /// column ACLs separate from table ACLs, so a table-level REVOKE alone is
    /// not a sufficient boundary check.
    pub authoring_effective_column_privileges: BTreeMap<(String, String, String), BTreeSet<String>>,
    /// Owner of every managed authoring table, keyed by `(schema, table)`.
    /// Ownership is authority to restore revoked ACLs and therefore must stay
    /// outside both the guest-visible and host-only author roles.
    pub authoring_table_owners: BTreeMap<(String, String), String>,
    /// Schemas on which the host-only author role effectively has USAGE.
    pub scenario_author_schema_usage: BTreeSet<String>,
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
    /// Rows in `catalog.event_registrations` still carrying a retired `state`
    /// or `partition-key` key (0 when the table is absent).
    pub stale_registration_key_rows: i64,
    /// Whether `catalog.release_components` exists without its route columns,
    /// so every member still binds a wiring node.
    pub release_components_without_routes: bool,
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
