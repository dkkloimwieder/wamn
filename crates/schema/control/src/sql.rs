//! Pure `$n`-parameterized SQL builders for the lifecycle + history writes the
//! engine composes with the wamn-schema-compiler DDL (SR3: text builders; the driver holds
//! the connection and executes). Identifiers are pinned — the fixed `catalog`
//! metadata schema (`deploy/sql/catalog-schema.sql`) — and values are always `$n`.
//!
//! The lifecycle state literals (`applied` / `superseded`) come from
//! [`crate::lifecycle::State`], the single source they share with the
//! `catalog.catalogs` `CHECK`, so the SQL cannot drift from the model.

use crate::lifecycle::State;
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
       (tenant_id, catalog_id, environment, from_version, to_version, statement_count, destructive, checksum) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
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

// ---------------------------------------------------------------------------
// Immutable catalog releases (FLOW-SPEC rev18 §5.1–§5.4a).
// ---------------------------------------------------------------------------

/// Register one fully resolved immutable artifact. The database function makes
/// an identical retry a no-op and raises `flow-version-content-conflict` when
/// the identity tuple already names different content.
pub fn register_flow_artifact_sql() -> &'static str {
    "SELECT catalog.register_flow_artifact(\
       $1, $2, $3, $4, $5::text::jsonb, $6, $7)"
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
         WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3::integer \
           AND status IN ('dispatched', 'running')"
    )
}

/// Persist one release member. Missing artifacts or execution bundles are
/// rejected by tenant-scoped FKs; an identical retry converges through
/// `DO NOTHING`.
pub fn insert_release_flow_sql() -> &'static str {
    "INSERT INTO catalog.release_flows \
       (tenant_id, catalog_id, catalog_version, flow_id, flow_version, \
        execution_bundle_hash) \
     VALUES ($1, $2, $3, $4, $5, $6) \
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
       (tenant_id, catalog_id, environment, from_version, to_version, statement_count, \
        destructive, checksum) \
     VALUES ($1, $2, $3, $4, $5, 0, false, $6) \
     ON CONFLICT (tenant_id, catalog_id, environment, to_version) DO NOTHING"
}

/// Read the immutable artifacts and memberships copied by `copy-project-env`.
pub fn select_release_artifacts_sql() -> &'static str {
    "SELECT a.flow_id, a.flow_version, a.schema_version, a.graph_json::text, \
            a.graph_hash, a.artifact_hash, r.execution_bundle_hash, b.exact_bytes \
     FROM catalog.release_flows r \
     JOIN catalog.flow_artifacts a \
       ON a.tenant_id = r.tenant_id AND a.flow_id = r.flow_id \
      AND a.flow_version = r.flow_version \
     JOIN catalog.execution_bundles b \
       ON b.tenant_id = r.tenant_id \
      AND b.execution_bundle_hash = r.execution_bundle_hash \
     WHERE r.tenant_id = $1 AND r.catalog_id = $2 AND r.catalog_version = $3 \
     ORDER BY a.flow_id"
}

// ---------------------------------------------------------------------------
// Schema-impact analysis (11.8, wamn-wvb): the dependency-edge reads the
// ops `impact-report` / `copy-project-env` shells fold through
// `wamn_schema_control::impact::analyze`.
// All cross-tenant (the superuser driver bypasses RLS), like the D24 read above:
// a shared entity's change hits every tenant's flow, so the report must see
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
/// `schema` is the caller-validated project schema, not the fixed `catalog` one.
pub fn select_active_flows_sql(schema: &str) -> String {
    format!(
        "SELECT tenant_id, flow_id, version, graph_json::text FROM {schema}.flows \
         WHERE active ORDER BY tenant_id, flow_id, version"
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

    use sha2::{Digest as _, Sha256};

    const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
    const FLOWS_SCHEMA: &str = include_str!("../../../../deploy/sql/flows.sql");

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
    fn execution_bundles_ddl_is_byte_for_byte_unchanged() {
        const START: &str = "CREATE TABLE catalog.execution_bundles (";
        const NEXT_SECTION: &str = "\n-- The sealed canonical member set.";
        const EXPECTED_SHA256: &str =
            "960e6f2e9f5db1e6752f32ca7518791f1fdd6e94d23af481bb63559472a5a952";

        assert_eq!(CATALOG_SCHEMA.match_indices(START).count(), 1);

        let start = CATALOG_SCHEMA
            .find(START)
            .expect("catalog schema contains the execution_bundles table");
        let (ddl, _) = CATALOG_SCHEMA[start..]
            .split_once(NEXT_SECTION)
            .expect("execution_bundles DDL precedes the sealed member section");

        assert_eq!(
            ddl.len(),
            1_324,
            "execution_bundles DDL byte length drifted"
        );
        let actual_sha256 = Sha256::digest(ddl.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(actual_sha256, EXPECTED_SHA256);
    }

    #[test]
    fn immutable_release_sql_tracks_catalog_schema() {
        assert!(CATALOG_SCHEMA.contains("CREATE TABLE catalog.execution_bundles"));
        assert!(CATALOG_SCHEMA.contains("execution_bundles_exact_hash"));
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
       $1, $2, $3, $4, $5::text::jsonb, $6, $7)"
        );
        assert_eq!(
            super::select_release_artifacts_sql(),
            "SELECT a.flow_id, a.flow_version, a.schema_version, a.graph_json::text, \
            a.graph_hash, a.artifact_hash, r.execution_bundle_hash, b.exact_bytes \
     FROM catalog.release_flows r \
     JOIN catalog.flow_artifacts a \
       ON a.tenant_id = r.tenant_id AND a.flow_id = r.flow_id \
      AND a.flow_version = r.flow_version \
     JOIN catalog.execution_bundles b \
       ON b.tenant_id = r.tenant_id \
      AND b.execution_bundle_hash = r.execution_bundle_hash \
     WHERE r.tenant_id = $1 AND r.catalog_id = $2 AND r.catalog_version = $3 \
     ORDER BY a.flow_id"
        );
        assert!(
            super::register_release_manifest_sql().contains("catalog.register_release_manifest")
        );
        assert!(super::publication_boundary_sql().contains("catalog.publication_boundary"));
        assert!(super::lock_catalog_head_sql().contains("FOR UPDATE"));
        assert!(
            super::count_nonterminal_release_runs_sql("app")
                .contains("catalog_version = $3::integer")
        );
        assert!(super::insert_release_flow_sql().contains("flow_version, execution_bundle_hash"));
        assert!(super::insert_release_flow_sql().contains("DO NOTHING"));
        assert!(super::insert_release_source_sql().contains("DO NOTHING"));
        assert!(super::insert_release_attachment_sql().contains("DO NOTHING"));
        assert!(super::apply_release_exposure_sql().contains("apply_release_exposure"));
        assert!(super::advance_catalog_head_sql().contains("DO UPDATE"));
    }
}
