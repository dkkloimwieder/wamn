//! Deterministic seed-row ids.
//!
//! A row's managed `id` is `uuidv5(SEED_NS, "tenant:entity:key")`. Including the
//! tenant keeps ids unique across tenants (the `id` primary key is not itself
//! tenant-scoped), and the derivation is pure and reproducible — re-seeding a
//! project or cloning a schema into a test host yields the *same* ids every time,
//! which is what makes the `ON CONFLICT (id) DO NOTHING` load idempotent and
//! lets reference fields resolve to a target row's id at compile time.

use uuid::Uuid;

/// The platform seed-id namespace (a fixed v4-shaped constant; only its bytes
/// matter as a uuidv5 namespace).
const SEED_NS: Uuid = Uuid::from_u128(0x0a1b_2c3d_4e5f_6071_8293_a4b5_c6d7_e8f9);

/// The deterministic id for a seed row.
pub fn row_id(tenant: &str, entity: &str, key: &str) -> Uuid {
    // ':' is a plain separator; tenant / entity / key are opaque strings.
    let name = format!("{tenant}:{entity}:{key}");
    Uuid::new_v5(&SEED_NS, name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic_and_scoped() {
        let a = row_id("t1", "suppliers", "acme");
        assert_eq!(a, row_id("t1", "suppliers", "acme"), "reproducible");
        assert_ne!(a, row_id("t2", "suppliers", "acme"), "tenant-scoped");
        assert_ne!(a, row_id("t1", "sites", "acme"), "entity-scoped");
        assert_ne!(a, row_id("t1", "suppliers", "other"), "key-scoped");
    }
}
