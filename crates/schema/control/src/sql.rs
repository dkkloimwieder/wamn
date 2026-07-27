//! Pure `$n`-parameterized SQL builders for the lifecycle + history writes the
//! engine composes with the wamn-schema-compiler DDL (SR3: text builders; the driver holds
//! the connection and executes). Identifiers are pinned — the fixed `catalog`
//! metadata schema (`deploy/sql/catalog-schema.sql`) — and values are always `$n`.
//!
//! The lifecycle state literals (`applied` / `superseded`) come from
//! [`crate::lifecycle::State`], the single source they share with the
//! `catalog.catalogs` `CHECK`, so the SQL cannot drift from the model.

use crate::lifecycle::State;
use wamn_schema_compiler::Confirmation;

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
// Immutable catalog releases (FLOW-SPEC rev18 §5.1–§5.4a).
// ---------------------------------------------------------------------------

/// Register one fully resolved immutable artifact. The database function makes
/// an identical retry a no-op and raises `flow-version-content-conflict` when
/// the identity tuple already names different content.
pub fn register_flow_artifact_sql() -> &'static str {
    "SELECT catalog.register_flow_artifact(\
       $1, $2, $3, $4, $5::text::jsonb, $6, $7, $8, $9, $10::text::jsonb)"
}

/// Seal the exact canonical member set for one release.
pub fn register_release_manifest_sql() -> &'static str {
    "SELECT catalog.register_release_manifest($1, $2, $3, $4::text::jsonb)"
}

/// Traverse a named, superuser-only publication fault boundary. This is a
/// no-op unless the release gate set `wamn.test.publication_fault` locally.
pub fn publication_boundary_sql() -> &'static str {
    "SELECT catalog.publication_boundary($1)"
}

/// Read the stable head, falling back to an applied legacy catalog when release
/// heads have not yet been initialized.
pub fn select_publication_base_sql() -> &'static str {
    "SELECT COALESCE(\
       (SELECT applied_catalog_version FROM catalog.catalog_heads \
        WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3), \
       (SELECT version FROM catalog.catalogs \
        WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 \
          AND state = 'applied'))"
}

/// Lock an applied legacy catalog while initializing its first stable head.
pub fn lock_current_applied_version_sql() -> &'static str {
    "SELECT version FROM catalog.catalogs \
     WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 \
       AND state = 'applied' FOR UPDATE"
}

/// Lock the stable head row before checking its applied release.
pub fn lock_catalog_head_sql() -> &'static str {
    "SELECT applied_catalog_version FROM catalog.catalog_heads \
     WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 \
     FOR UPDATE"
}

/// Count runs which still pin the release being replaced. `schema` is a
/// validated bare identifier owned by the caller.
pub fn count_nonterminal_release_runs_sql(schema: &str) -> String {
    format!(
        "SELECT count(*) FROM {schema}.runs \
         WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
           AND status IN ('dispatched', 'running')"
    )
}

/// Persist one release member. A missing artifact is rejected by the FK; an
/// identical retry converges through `DO NOTHING`.
pub fn insert_release_flow_sql() -> &'static str {
    "INSERT INTO catalog.release_flows \
       (tenant_id, catalog_id, catalog_version, flow_id, flow_version) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (tenant_id, catalog_id, catalog_version, flow_id) DO NOTHING"
}

/// Seal the canonical authored exposure document for a release.
pub fn register_release_exposure_manifest_sql() -> &'static str {
    "SELECT catalog.register_release_exposure_manifest($1, $2, $3, $4::text::jsonb)"
}

/// Persist one immutable source definition.
pub fn insert_release_source_sql() -> &'static str {
    "INSERT INTO catalog.release_sources \
       (tenant_id, catalog_id, catalog_version, source_id, source_kind, \
        definition_json, source_hash) \
     VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7) \
     ON CONFLICT (tenant_id, catalog_id, catalog_version, source_id) DO NOTHING"
}

/// Persist one fully resolved immutable attachment definition.
pub fn insert_release_attachment_sql() -> &'static str {
    "INSERT INTO catalog.release_attachments \
       (tenant_id, catalog_id, catalog_version, attachment_id, attachment_kind, \
        flow_id, source_id, definition_hash, definition_json, route_host, \
        route_path, route_template, route_method) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::jsonb, $10, $11, $12, $13) \
     ON CONFLICT (tenant_id, catalog_id, catalog_version, attachment_id) DO NOTHING"
}

/// Carry activation for unchanged definitions and tombstone removed IDs.
pub fn apply_release_exposure_sql() -> &'static str {
    "SELECT catalog.apply_release_exposure($1, $2, $3, $4, $5)"
}

/// Read every source definition copied by `copy-project-env`.
pub fn select_release_sources_sql() -> &'static str {
    "SELECT source_id, source_kind, definition_json::text, source_hash \
     FROM catalog.release_sources \
     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
     ORDER BY source_id"
}

/// Read every resolved attachment definition copied by `copy-project-env`.
pub fn select_release_attachments_sql() -> &'static str {
    "SELECT attachment_id, attachment_kind, flow_id, source_id, definition_hash, \
            definition_json::text, route_host, route_path, route_template, route_method \
     FROM catalog.release_attachments \
     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
     ORDER BY attachment_id"
}

/// Advance (or initialize) the stable head after every other release write.
pub fn advance_catalog_head_sql() -> &'static str {
    "INSERT INTO catalog.catalog_heads \
       (tenant_id, catalog_id, environment, applied_catalog_version) \
     VALUES ($1, $2, $3, $4) \
     ON CONFLICT (tenant_id, catalog_id, environment) DO UPDATE SET \
       applied_catalog_version = EXCLUDED.applied_catalog_version, updated_at = now()"
}

/// Append the publication journal row, converging on an identical retry.
pub fn record_release_publication_sql() -> &'static str {
    "INSERT INTO catalog.schema_migrations \
       (tenant_id, catalog_id, environment, from_version, to_version, confirmation, \
        statement_count, destructive, checksum) \
     VALUES ($1, $2, $3, $4, $5, 'none', 0, false, $6) \
     ON CONFLICT (tenant_id, catalog_id, environment, to_version) DO NOTHING"
}

/// Read the immutable artifacts and memberships copied by `copy-project-env`.
pub fn select_release_artifacts_sql() -> &'static str {
    "SELECT a.flow_id, a.flow_version, a.schema_version, a.graph_json::text, \
            a.graph_hash, a.artifact_hash, a.interface_bundle_json, \
            a.interface_bundle_hash, a.component_digests::text \
     FROM catalog.release_flows r \
     JOIN catalog.flow_artifacts a \
       ON a.tenant_id = r.tenant_id AND a.flow_id = r.flow_id \
      AND a.flow_version = r.flow_version \
     WHERE r.tenant_id = $1 AND r.catalog_id = $2 AND r.catalog_version = $3 \
     ORDER BY a.flow_id"
}

// ---------------------------------------------------------------------------
// Schema-impact analysis (11.8, wamn-wvb): the dependency-edge reads the
// `impact-report` / `migrate-catalog` shell folds through `wamn_schema_control::impact::analyze`.
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

    const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
    const FLOWS_SCHEMA: &str = include_str!("../../../../deploy/sql/flows.sql");
    const FLOW_TESTS_SCHEMA: &str = include_str!("../../../../deploy/sql/flow-tests.sql");

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

    #[test]
    fn immutable_release_sql_tracks_catalog_schema() {
        for table in [
            "flow_artifacts",
            "release_manifests",
            "release_flows",
            "catalog_heads",
            "release_exposure_manifests",
            "release_sources",
            "release_attachments",
            "attachment_activation",
            "attachment_activation_events",
            "attachment_tombstones",
        ] {
            assert!(
                CATALOG_SCHEMA.contains(&format!("CREATE TABLE catalog.{table}")),
                "missing catalog.{table}"
            );
        }
        assert_eq!(
            super::register_flow_artifact_sql(),
            "SELECT catalog.register_flow_artifact(\
       $1, $2, $3, $4, $5::text::jsonb, $6, $7, $8, $9, $10::text::jsonb)"
        );
        assert_eq!(
            super::select_release_artifacts_sql(),
            "SELECT a.flow_id, a.flow_version, a.schema_version, a.graph_json::text, \
            a.graph_hash, a.artifact_hash, a.interface_bundle_json, \
            a.interface_bundle_hash, a.component_digests::text \
     FROM catalog.release_flows r \
     JOIN catalog.flow_artifacts a \
       ON a.tenant_id = r.tenant_id AND a.flow_id = r.flow_id \
      AND a.flow_version = r.flow_version \
     WHERE r.tenant_id = $1 AND r.catalog_id = $2 AND r.catalog_version = $3 \
     ORDER BY a.flow_id"
        );
        assert!(
            super::register_release_manifest_sql().contains("catalog.register_release_manifest")
        );
        assert!(super::publication_boundary_sql().contains("catalog.publication_boundary"));
        assert!(super::lock_catalog_head_sql().contains("FOR UPDATE"));
        assert!(super::count_nonterminal_release_runs_sql("app").contains("catalog_version = $3"));
        assert!(super::insert_release_flow_sql().contains("DO NOTHING"));
        assert!(super::insert_release_source_sql().contains("DO NOTHING"));
        assert!(super::insert_release_attachment_sql().contains("DO NOTHING"));
        assert!(super::apply_release_exposure_sql().contains("apply_release_exposure"));
        assert!(super::advance_catalog_head_sql().contains("DO UPDATE"));
    }
}
