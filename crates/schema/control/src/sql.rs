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

/// Enumerate every applied catalog for `(tenant, environment)`: applied-state
/// only, ordered by `catalog_id` for a deterministic read. Returns
/// `catalog_id, version, document::text`.
///
/// The copy definition pass that motivated this builder went with the rest of
/// the unreachable copy-definition path (`5bb69f0d`); no production shell calls
/// it today — `crates/schema/control/tests/migrate.rs` is its only caller.
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

/// Ensure the release identity row for one release coordinate. Insert-or-verify:
/// an identical retry is a no-op and a differing identity row raises
/// `catalog-release-content-conflict`.
pub fn register_release_manifest_sql() -> &'static str {
    "SELECT catalog.register_release_manifest($1, $2, $3)"
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

/// Probe whether a release identity row exists at one coordinate.
pub fn release_manifest_exists_sql() -> &'static str {
    "SELECT EXISTS (SELECT 1 FROM catalog.releases \
     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3)"
}

/// Record the deployment attestation for one six-part coordinate. Insert-or-verify:
/// an identical retry returns the original `attested_at`, and a differing
/// `deployed_manifest_hash` at the same coordinate raises
/// `deployment-attestation-content-conflict`.
///
/// CONTROL-plane only (`deploy/sql/control-portable-store.sql`), unlike the
/// release identity above, which is a separate relation in each plane. `$8` is
/// bound as text and cast because [`crate::Value`] carries no timestamp
/// variant — the same double cast as `$8::text::jsonb` in
/// [`upsert_applied_version_sql`], which also holds the parameter's own inferred
/// type at `text` instead of the cast target. [`crate::attestation`] owns the
/// typed binding and the single translation of this write's failure.
pub fn register_deployment_attestation_sql() -> &'static str {
    "SELECT catalog.register_deployment_attestation(\
     $1, $2, $3, $4, $5, $6, $7, $8::text::timestamptz)"
}

// ---------------------------------------------------------------------------
// Schema-impact analysis (11.8, wamn-wvb): the dependency-edge reads the ops
// `impact-report` shell (`services/ctl/src/impact_report.rs`, the only one)
// folds through `wamn_schema_control::impact::analyze`.
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
    fn immutable_release_sql_tracks_catalog_schema() {
        assert!(!CATALOG_SCHEMA.contains("CREATE TABLE catalog.execution_bundles"));
        assert!(!CATALOG_SCHEMA.contains("execution_bundle_hash"));
        for table in ["releases", "catalog_heads"] {
            assert!(
                CATALOG_SCHEMA.contains(&format!("CREATE TABLE catalog.{table}")),
                "missing catalog.{table}"
            );
        }
        // wamn-0h0g.26.21: the flow-era release plane is gone from the schema,
        // so its relations are asserted ABSENT exactly like the execution
        // bundles above.
        for retired in [
            "flow_artifacts",
            "release_flows",
            "release_exposure_manifests",
            "release_sources",
            "release_attachments",
            "attachment_activation",
            "attachment_activation_events",
            "attachment_tombstones",
        ] {
            assert!(
                !CATALOG_SCHEMA.contains(&format!("CREATE TABLE catalog.{retired}")),
                "retired catalog.{retired} survived"
            );
        }
        assert!(
            super::register_release_manifest_sql().contains("catalog.register_release_manifest")
        );
        assert!(super::publication_boundary_sql().contains("catalog.publication_boundary"));
        assert!(super::lock_catalog_head_sql().contains("FOR UPDATE"));
        assert!(
            super::count_nonterminal_release_runs_sql("app")
                .contains("catalog_version = $3::integer")
        );
        assert!(super::advance_catalog_head_sql().contains("DO UPDATE"));
    }

    /// The deployment-attestation write is CONTROL-plane, so `CATALOG_SCHEMA`
    /// above cannot answer for it, and a text scan over the control DDL could
    /// not tell a declaration from a comment anyway. The drift guard that IS
    /// load-bearing for Rust-built SQL is the generated string itself: pinned
    /// exactly, so a moved `$n` or a dropped cast fails here rather than only
    /// against a live server. `deployment_attestation_rust_binding_holds_on_postgres`
    /// (`crates/control/provision/tests/control_portable_store.rs`) asks
    /// PostgreSQL 18 itself whether this string matches the installed routine.
    #[test]
    fn deployment_attestation_write_pins_its_eight_argument_binding() {
        assert_eq!(
            super::register_deployment_attestation_sql(),
            "SELECT catalog.register_deployment_attestation(\
             $1, $2, $3, $4, $5, $6, $7, $8::text::timestamptz)"
        );
    }
}
