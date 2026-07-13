//! The SQL text the F1 DB nodes execute. Values are ALWAYS `$n` parameters —
//! there is no string-interpolation path (the `wamn:postgres` WIT is
//! parameterized-only, and these statements never embed payload text).
//! Identifiers are PINNED to the `poc-material-receiving` catalog's generated
//! names (drift-guarded against the catalog fixture in this crate's tests);
//! table names are UNQUALIFIED — the host-injected `search_path` (the
//! `wamn.schema` claim) resolves them to the project schema. `tenant_id` on
//! every INSERT is `current_setting('app.tenant', true)` server-side: the
//! guest never chooses its tenant, and the 3.2 RLS floor checks it.

/// Resolve a supplier business key. `$1` = `suppliers.name`.
pub const RESOLVE_SUPPLIER: &str = "SELECT id::text FROM suppliers WHERE name = $1";

/// Resolve a site business key. `$1` = `sites.code`.
pub const RESOLVE_SITE: &str = "SELECT id::text FROM sites WHERE code = $1";

/// Resolve a material business key to its id + the two spec values (canonical
/// numeric text). `$1` = `materials.name`.
pub const RESOLVE_MATERIAL: &str = "SELECT id::text, moisture_max_pct::text, \
     weight_tolerance_kg::text FROM materials WHERE name = $1";

/// Upsert the receipt on its composite natural key — the catalog's
/// `receipts_no_supplier_uniq` unique constraint, tenant-scoped by 3.2 to
/// `(tenant_id, receipt_no, supplier_id)`. A re-POST updates the mutable
/// columns instead of failing. `$1` = receipt_no (text), `$2` = supplier_id
/// (uuid), `$3` = site_id (uuid), `$4` = received_at (timestamptz).
pub const UPSERT_RECEIPT: &str = "INSERT INTO receipts \
       (tenant_id, receipt_no, supplier_id, site_id, received_at) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3, $4) \
     ON CONFLICT (tenant_id, receipt_no, supplier_id) \
       DO UPDATE SET site_id = EXCLUDED.site_id, received_at = EXCLUDED.received_at \
     RETURNING id::text";

/// Replace-style line upsert, step 1: clear the receipt's existing lines (a
/// re-POST replaces the line set; runs inside the same transaction as the
/// receipt upsert and the inserts). `$1` = receipt_id (uuid). NOTE: lines
/// already under a quality hold cannot be deleted (FK from
/// `quality_holds.line_id`) — re-POSTing a receipt that has holds fails the
/// transaction, which is the conservative v1 behavior.
pub const DELETE_LINES: &str = "DELETE FROM receipt_lines WHERE receipt_id = $1";

/// Replace-style line upsert, step 2: insert one line. `$1` = receipt_id
/// (uuid), `$2` = material_id (uuid), `$3` = quantity (numeric).
pub const INSERT_LINE: &str = "INSERT INTO receipt_lines \
       (tenant_id, receipt_id, material_id, quantity) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3) \
     RETURNING id::text";

/// Create one quality hold for an out-of-spec line: status `'open'`, opened
/// server-side `now()`. `$1` = line_id (uuid), `$2` = site_id (uuid).
pub const INSERT_HOLD: &str = "INSERT INTO quality_holds \
       (tenant_id, line_id, site_id, status, opened_at) \
     VALUES (current_setting('app.tenant', true), $1, $2, 'open', now()) \
     RETURNING id::text";
