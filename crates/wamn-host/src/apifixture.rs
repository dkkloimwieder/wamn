//! Shared 4.1 / 4.1b demo fixture.
//!
//! The `apibench` gate (drives the api-gateway component in-process via
//! `ProxyPre`), the `publish-catalog` tool (writes the catalog snapshot the
//! gateway reads), and the `apiproof` gate (drives the *deployed* gateway over
//! real HTTP) all address the same tiny catalog, tenant floor, and seed rows.
//! Keeping that one definition here means the three commands can never drift:
//! the snapshot a `publish-catalog` Job writes is exactly the catalog `apiproof`
//! then queries, using exactly the row ids the assertions expect.
//!
//! The catalog is `suppliers ← receipts ← receipt_lines` with a to-one relation
//! `supplier` (receipts→suppliers) and a to-many relation `lines`
//! (receipt_lines→receipts). It is committed verbatim as `deploy/proof-catalog.json`
//! (a drift-guard test asserts they match) so `publish-catalog` can read it as a
//! file, exactly as it would read a real project's applied catalog.

use serde_json::Value;

/// The demo catalog, stored as the snapshot the gateway loads. Kept byte-for-byte
/// identical to `deploy/proof-catalog.json` (see the drift-guard test).
pub const CATALOG_JSON: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "apibench",
  "version": 1,
  "entities": [
    { "id": "suppliers", "name": "suppliers", "fields": [
      { "id": "name", "name": "name", "type": { "kind": "text" } },
      { "id": "standard_cost", "name": "standard_cost", "type": { "kind": "numeric", "precision": 12, "scale": 2 }, "nullable": true }
    ] },
    { "id": "receipts", "name": "receipts", "fields": [
      { "id": "receipt_no", "name": "receipt_no", "type": { "kind": "text", "max-len": 64 } },
      { "id": "supplier_id", "name": "supplier_id", "type": { "kind": "reference", "entity": "suppliers" } },
      { "id": "received_at", "name": "received_at", "type": { "kind": "timestamptz" } }
    ] },
    { "id": "receipt_lines", "name": "receipt_lines", "fields": [
      { "id": "receipt_id", "name": "receipt_id", "type": { "kind": "reference", "entity": "receipts" } },
      { "id": "quantity", "name": "quantity", "type": { "kind": "numeric", "precision": 12, "scale": 3 } }
    ] }
  ],
  "relations": [
    { "id": "receipt_supplier", "name": "supplier", "cardinality": "one-to-many", "from": "receipts", "to": "suppliers", "from-field": "supplier_id" },
    { "id": "receipt_lines_rel", "name": "lines", "cardinality": "one-to-many", "from": "receipt_lines", "to": "receipts", "from-field": "receipt_id" }
  ]
}"#;

/// The tenant the gateway is scoped to.
pub const TENANT_A: &str = "tenant-a";
/// A second tenant whose rows must stay invisible (the RLS witness).
pub const TENANT_B: &str = "tenant-b";

// Deterministic seed ids so the gates can address rows directly.
pub const S_ACME: &str = "a0000000-0000-0000-0000-000000000001";
pub const S_GLOBEX: &str = "a0000000-0000-0000-0000-000000000002";
pub const S_OTHER: &str = "b0000000-0000-0000-0000-000000000003";
pub const R1: &str = "c0000000-0000-0000-0000-000000000001";
pub const L1: &str = "d0000000-0000-0000-0000-000000000001";
pub const L2: &str = "d0000000-0000-0000-0000-000000000002";

/// Parse the demo catalog.
pub fn catalog() -> anyhow::Result<wamn_catalog::Catalog> {
    wamn_catalog::Catalog::from_json(CATALOG_JSON)
        .map_err(|e| anyhow::anyhow!("demo catalog parse: {e}"))
}

/// The 3.2-generated tenant floor for the demo catalog (CREATE TABLE + FORCE RLS
/// + `app.tenant` policy + grants), tenant-scoped uniqueness/indexes and all.
pub fn floor_ddl() -> anyhow::Result<String> {
    let cat = catalog()?;
    wamn_ddl::Migration::create(&cat)
        .map_err(|e| anyhow::anyhow!("floor compile: {e}"))?
        .sql(wamn_ddl::Confirmation::None)
        .map_err(|e| anyhow::anyhow!("floor sql: {e}"))
}

/// Additive INSERTs seeding the two-tenant demo rows. tenant-a gets Acme, Globex,
/// receipt R-001, and its two lines; tenant-b gets the OtherTenantCo RLS witness.
/// `ON CONFLICT (id) DO NOTHING` makes re-seeding a no-op (the floor's managed
/// `id` is the PK), so this is safe to run against a durable, additive schema.
/// Seeded as a superuser (RLS is bypassed), matching every other wamn seed path.
pub fn entity_seed_sql() -> String {
    // Numeric literals are unquoted so Postgres parses them as `numeric` (exact
    // decimal, never a float); `CATALOG_JSON` has no single quotes elsewhere.
    format!(
        "INSERT INTO suppliers (id, tenant_id, name, standard_cost) VALUES \
           ('{S_ACME}', '{TENANT_A}', 'Acme', 12.50), \
           ('{S_GLOBEX}', '{TENANT_A}', 'Globex', 99.99), \
           ('{S_OTHER}', '{TENANT_B}', 'OtherTenantCo', 5.00) \
         ON CONFLICT (id) DO NOTHING; \
         INSERT INTO receipts (id, tenant_id, receipt_no, supplier_id, received_at) VALUES \
           ('{R1}', '{TENANT_A}', 'R-001', '{S_ACME}', '2026-01-01T00:00:00Z') \
         ON CONFLICT (id) DO NOTHING; \
         INSERT INTO receipt_lines (id, tenant_id, receipt_id, quantity) VALUES \
           ('{L1}', '{TENANT_A}', '{R1}', 3.000), \
           ('{L2}', '{TENANT_A}', '{R1}', 5.500) \
         ON CONFLICT (id) DO NOTHING;"
    )
}

// ---- shared assertion helpers (used by both gates) ------------------------

/// Print a check line and fold it into the running pass flag.
pub fn check(pass: &mut bool, label: &str, ok: bool) {
    println!("  [{}] {label}", if ok { "PASS" } else { "FAIL" });
    *pass &= ok;
}

/// A JSON value as an array of values (empty if not an array).
pub fn as_array(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// Whether any row has `.name == name`.
pub fn has_name(rows: &[Value], name: &str) -> bool {
    rows.iter()
        .any(|r| r.get("name").and_then(Value::as_str) == Some(name))
}

#[cfg(test)]
mod tests {
    use super::{CATALOG_JSON, catalog, floor_ddl};

    /// The committed `deploy/proof-catalog.json` that `publish-catalog` reads must
    /// stay in lockstep with the in-code `CATALOG_JSON` the gates address — else a
    /// snapshot written from the file would not match what `apiproof` queries.
    #[test]
    fn proof_catalog_file_matches_const() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/proof-catalog.json"
        );
        let file = std::fs::read_to_string(path).expect("read deploy/proof-catalog.json");
        let from_file = wamn_catalog::Catalog::from_json(&file).expect("parse proof-catalog.json");
        let from_const = wamn_catalog::Catalog::from_json(CATALOG_JSON).expect("parse const");
        // Canonical JSON equality is robust to incidental whitespace differences.
        assert_eq!(from_file.to_json(), from_const.to_json());
    }

    #[test]
    fn demo_catalog_and_floor_compile() {
        let cat = catalog().expect("catalog parses + validates");
        assert_eq!(cat.entities.len(), 3);
        let ddl = floor_ddl().expect("floor compiles");
        assert!(ddl.contains("CREATE TABLE \"suppliers\""));
        assert!(ddl.contains("FORCE ROW LEVEL SECURITY"));
    }
}
