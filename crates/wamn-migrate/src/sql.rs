//! Pure `$n`-parameterized SQL builders for the lifecycle + history writes the
//! engine composes with the wamn-ddl DDL (SR3: text builders; the driver holds
//! the connection and executes). Identifiers are pinned — the fixed `catalog`
//! metadata schema (`deploy/sql/catalog-schema.sql`) — and values are always `$n`.
//!
//! The lifecycle state literals (`applied` / `superseded`) come from
//! [`wamn_schema::State`], the single source they share with the
//! `catalog.catalogs` `CHECK`, so the SQL cannot drift from the model.

use wamn_ddl::Confirmation;
use wamn_schema::State;

/// The value written to `schema_migrations.confirmation` — the single source the
/// history write, the driver, and the DDL `CHECK` share.
pub fn confirmation_sql(confirm: Confirmation) -> &'static str {
    match confirm {
        Confirmation::None => "none",
        Confirmation::ConfirmedWithBackup => "confirmed-with-backup",
    }
}

/// Read the current applied catalog for `(tenant, catalog, environment)`,
/// locking the row for the apply transaction. Returns `version` and the stored
/// `document` (the applied `Catalog` JSON) the engine diffs a target against.
pub fn select_current_applied_sql() -> String {
    "SELECT version, document::text FROM catalog.catalogs \
     WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 AND state = 'applied' \
     FOR UPDATE"
        .to_string()
}

/// Enumerate every applied catalog for `(tenant, environment)` — the unified
/// copy's definition pass (wamn-8df.5) promotes each of the src env's applied
/// catalogs into the dst env. Returns `catalog_id, version, document::text`.
pub fn select_applied_catalogs_sql() -> String {
    format!(
        "SELECT catalog_id, version, document::text FROM catalog.catalogs \
         WHERE tenant_id = $1 AND environment = $2 AND state = '{applied}' \
         ORDER BY catalog_id",
        applied = State::Applied.as_sql(),
    )
}

/// Demote whichever version is currently `applied` in `(tenant, catalog,
/// environment)` to `superseded`. Run before promoting the target so the
/// `catalogs_one_applied_per_env` single-applied index is never transiently
/// violated (unique indexes are checked at statement end).
pub fn demote_current_applied_sql() -> String {
    format!(
        "UPDATE catalog.catalogs SET state = '{superseded}' \
         WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 AND state = '{applied}'",
        superseded = State::Superseded.as_sql(),
        applied = State::Applied.as_sql(),
    )
}

/// Record the target version as the live `applied` schema, storing its catalog
/// `document` (the diff source for the next migration). Upsert because the row
/// may already exist as a `draft`/`staged` candidate.
pub fn upsert_applied_version_sql() -> String {
    format!(
        "INSERT INTO catalog.catalogs \
           (tenant_id, catalog_id, version, environment, schema_version, name, state, base_version, document) \
         VALUES ($1, $2, $3, $4, $5, $6, '{applied}', $7, $8::text::jsonb) \
         ON CONFLICT (tenant_id, catalog_id, version) DO UPDATE SET \
           environment = EXCLUDED.environment, schema_version = EXCLUDED.schema_version, \
           name = EXCLUDED.name, state = '{applied}', base_version = EXCLUDED.base_version, \
           document = EXCLUDED.document",
        applied = State::Applied.as_sql(),
    )
}

/// Append the immutable history row for this apply (`from -> to`, destructive
/// flag, operation count, checksum). The `schema_migrations` PK forbids recording
/// the same `(catalog, environment, to_version)` twice — forward-only.
pub fn record_migration_sql() -> String {
    "INSERT INTO catalog.schema_migrations \
       (tenant_id, catalog_id, environment, from_version, to_version, confirmation, statement_count, destructive, checksum) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
        .to_string()
}

/// Select every event registration for `catalog_id` across ALL tenants (the
/// driver connects as a superuser, so RLS is bypassed and every tenant's row is
/// returned), projecting the columns the D24 orphan guard needs
/// ([`crate::check_registration_orphans`]). Cross-tenant on purpose: a shared
/// entity table's removal orphans every tenant's registration on it, and the
/// refusal must name each. Ordered for a deterministic error listing. SR12: the
/// pure decision has no RLS/superuser — the throwaway-PG orphan-guard gate
/// (wamn-ctl `tests/orphan_guard_live.rs`) covers that this really sees all
/// tenants' rows.
pub fn select_registrations_for_catalog_sql() -> String {
    "SELECT registration_id, tenant_id, entity_id FROM catalog.event_registrations \
     WHERE catalog_id = $1 ORDER BY tenant_id, registration_id"
        .to_string()
}

/// Select every event registration's stored DOCUMENT for `catalog_id` across ALL
/// tenants (superuser, RLS bypassed) — the REPLICA IDENTITY reconciler
/// (wamn-l5i9.31) folds the parsed `EventRegistration`s (condition + ops, not the
/// denormalized columns the D24 guard reads) to derive which entities need FULL.
/// Cross-tenant on purpose: RI is per-TABLE and tables are shared, so the FULL
/// requirement is the union of every tenant's registrations on the entity.
/// Ordered for a deterministic scan. SR12: the pure decision has no
/// RLS/superuser — the throwaway-PG live gate covers that this sees all tenants.
pub fn select_registration_docs_for_catalog_sql() -> String {
    "SELECT registration::text FROM catalog.event_registrations \
     WHERE catalog_id = $1 ORDER BY tenant_id, registration_id"
        .to_string()
}

/// The 11.2 suite-orphan guard read (wamn-828): the test suites a definition
/// copy carries for `$1` (tenant), from `<schema>.test_suites`, projecting
/// `(suite_id, tenant_id, flow_id, flow_version)` — what [`crate::check_suite_orphans`]
/// folds against the flow versions the copy will install. Unlike the pinned
/// `catalog`-schema builders above, `schema` is the copy verb's `--flow-schema`
/// (the `wamn_run` → project-schema convention): it is interpolated, so the
/// caller passes a VALIDATED bare identifier (`is_bare_ident`); the value is `$1`.
/// Ordered for a deterministic refusal listing.
pub fn select_suites_for_tenant_sql(schema: &str) -> String {
    format!(
        "SELECT suite_id, tenant_id, flow_id, flow_version FROM {schema}.test_suites \
         WHERE tenant_id = $1 ORDER BY flow_id, flow_version, suite_id"
    )
}

/// The 11.2 suite-orphan guard read: the `(flow_id, version)` pairs present in
/// `<schema>.flows` for `$1` (tenant) — the versions a suite may pin. Same
/// validated-bare-`schema` contract as [`select_suites_for_tenant_sql`]; value `$1`.
pub fn select_flow_versions_for_tenant_sql(schema: &str) -> String {
    format!("SELECT flow_id, version FROM {schema}.flows WHERE tenant_id = $1")
}

// ---------------------------------------------------------------------------
// Schema-impact analysis (11.8, wamn-wvb): the dependency-edge reads the
// `impact-report` / `migrate-catalog` shell folds through `wamn_impact::analyze`.
// All cross-tenant (the superuser driver bypasses RLS), like the D24 read above:
// a shared entity's change hits every tenant's flow/suite, so the report must see
// them all. SR12: the pure decision has no RLS/superuser — the throwaway-PG live
// gate (wamn-ctl `tests/impact_report_live.rs`) covers that these see every tenant.
// ---------------------------------------------------------------------------

/// The 11.8 registration edge: like [`select_registrations_for_catalog_sql`] (the
/// D24 read) but also projecting `flow_id`, so impact analysis names the
/// SUBSCRIBING FLOW, not only the orphaned registration. Cross-tenant, ordered for
/// a deterministic report.
pub fn select_registration_flow_refs_for_catalog_sql() -> String {
    "SELECT registration_id, tenant_id, entity_id, flow_id FROM catalog.event_registrations \
     WHERE catalog_id = $1 ORDER BY tenant_id, registration_id"
        .to_string()
}

/// The 11.8 node-config edge: every ACTIVE flow's `graph_json` across ALL tenants,
/// so the analysis can scan each graph for a postgres node's `config["entity"]`
/// (the name-keyed edge). Projects `(tenant_id, flow_id, version, graph_json)`.
/// Same validated-bare-`schema` contract as [`select_suites_for_tenant_sql`]
/// (the flow registry lives in the project schema, not the fixed `catalog` one).
pub fn select_active_flows_sql(schema: &str) -> String {
    format!(
        "SELECT tenant_id, flow_id, version, graph_json::text FROM {schema}.flows \
         WHERE active ORDER BY tenant_id, flow_id, version"
    )
}

/// The 11.8 suite edge: every test suite across ALL tenants, so the analysis can
/// keep the suites of the flows a change touches (all versions — the tuple the
/// parked executor wamn-0lfu would run). Projects
/// `(tenant_id, flow_id, flow_version, suite_id)`; the affected-flow filter is the
/// pure decision's, not SQL's. Same validated-bare-`schema` contract as
/// [`select_suites_for_tenant_sql`].
pub fn select_all_suites_sql(schema: &str) -> String {
    format!(
        "SELECT tenant_id, flow_id, flow_version, suite_id FROM {schema}.test_suites \
         ORDER BY tenant_id, flow_id, flow_version, suite_id"
    )
}

/// A cheap, dependency-free checksum (FNV-1a 64) of the applied DDL script — an
/// integrity/audit fingerprint stored in the history row, not a security hash.
pub fn ddl_checksum(sql: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sql.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    //! Drift guards (11.8, wamn-wvb): pin the dependency-edge reads against the
    //! schema of record so a renamed column fails HERE, not only against a live
    //! PG (the include_str! mirror of the gates `schema_drift` discipline).

    const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
    const FLOWS_SCHEMA: &str = include_str!("../../../deploy/sql/flows.sql");
    const FLOW_TESTS_SCHEMA: &str = include_str!("../../../deploy/sql/flow-tests.sql");

    #[test]
    fn registration_flow_refs_read_tracks_event_registrations() {
        let sql = super::select_registration_flow_refs_for_catalog_sql();
        assert!(CATALOG_SCHEMA.contains("CREATE TABLE catalog.event_registrations"));
        for col in [
            "registration_id",
            "tenant_id",
            "entity_id",
            "flow_id",
            "catalog_id",
        ] {
            assert!(sql.contains(col), "read references column {col}");
            assert!(
                CATALOG_SCHEMA.contains(col),
                "catalog-schema.sql event_registrations no longer has {col}"
            );
        }
    }

    #[test]
    fn active_flows_read_tracks_flows() {
        let sql = super::select_active_flows_sql("app");
        assert!(FLOWS_SCHEMA.contains("CREATE TABLE wamn_run.flows"));
        for col in ["tenant_id", "flow_id", "version", "graph_json", "active"] {
            assert!(sql.contains(col), "read references column {col}");
            assert!(FLOWS_SCHEMA.contains(col), "flows.sql no longer has {col}");
        }
    }

    #[test]
    fn all_suites_read_tracks_test_suites() {
        let sql = super::select_all_suites_sql("app");
        assert!(FLOW_TESTS_SCHEMA.contains("CREATE TABLE wamn_run.test_suites"));
        for col in ["tenant_id", "flow_id", "flow_version", "suite_id"] {
            assert!(sql.contains(col), "read references column {col}");
            assert!(
                FLOW_TESTS_SCHEMA.contains(col),
                "flow-tests.sql no longer has {col}"
            );
        }
    }
}
