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
