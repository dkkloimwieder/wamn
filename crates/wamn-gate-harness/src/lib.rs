//! Shared measurement/assertion vocabulary for the gate suite (`wamn-gates`).
//!
//! The gates accreted per-bench copies of the same helpers (`percentile`
//! existed three times host-side); this crate is the single place they live
//! (docs/structure-review.md SR1). Scope: pure, dependency-light helpers —
//! stats over collected samples, the PASS/FAIL check line, and small JSON
//! response asserts. Bench-specific machinery (harness structs, provisioning,
//! stepped clocks with a single consumer) stays in its bench module until a
//! second consumer pulls it here.

pub mod ceiling;

use std::time::Duration;

use serde_json::Value;

/// Percentile over an already-sorted sample set (empty-safe).
pub fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

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
    use super::*;

    #[test]
    fn percentile_is_empty_safe_and_indexes_the_sorted_tail() {
        assert_eq!(percentile(&[], 0.99), Duration::ZERO);
        let s = [1, 2, 3, 4].map(Duration::from_millis).to_vec();
        assert_eq!(percentile(&s, 0.0), Duration::from_millis(1));
        assert_eq!(percentile(&s, 1.0), Duration::from_millis(4));
    }

    #[test]
    fn check_folds_into_the_pass_flag() {
        let mut pass = true;
        check(&mut pass, "ok", true);
        assert!(pass);
        check(&mut pass, "bad", false);
        assert!(!pass);
        check(&mut pass, "ok again", true);
        assert!(!pass, "a failed check must stick");
    }
}

// ---------------------------------------------------------------------------
// PG fixture helpers (SR2): host-side flow-fixture seeding. The gates hold
// the connection (app-role with claims, or superuser); the harness holds the
// single copy of the fixture SQL — values always $n, jsonb via ::text::jsonb.
// ---------------------------------------------------------------------------

/// Scope a fixture session: the tenant claim (RLS) + the schema (search_path),
/// both bound as parameters via set_config — no interpolation path.
pub async fn scope_session(
    client: &tokio_postgres::Client,
    tenant: &str,
    schema: &str,
) -> anyhow::Result<()> {
    client
        .query(
            "SELECT set_config('app.tenant', $1, false), set_config('search_path', $2, false)",
            &[&tenant, &schema],
        )
        .await?;
    Ok(())
}

/// Seed one flow-registry version row (idempotent). `activate_on_conflict`
/// mirrors the two guest-era shapes: the S3 seed kept a re-seed from touching
/// `active`; the S6 seed forced the row active.
pub async fn seed_flow_version(
    client: &tokio_postgres::Client,
    tenant: &str,
    flow_id: &str,
    version: i32,
    active: bool,
    graph_json: &str,
    activate_on_conflict: bool,
) -> anyhow::Result<()> {
    let tail = if activate_on_conflict {
        "DO UPDATE SET graph_json = excluded.graph_json, active = true"
    } else {
        "DO UPDATE SET graph_json = excluded.graph_json"
    };
    let sql = format!(
        "INSERT INTO flows (tenant_id, flow_id, version, active, graph_json) \
         VALUES ($1, $2, $3, $4, $5::text::jsonb) \
         ON CONFLICT (tenant_id, flow_id, version) {tail}"
    );
    client
        .execute(&sql, &[&tenant, &flow_id, &version, &active, &graph_json])
        .await?;
    Ok(())
}

/// Flip the active flow version: exactly one active version per flow.
pub async fn set_active_flow_version(
    client: &tokio_postgres::Client,
    tenant: &str,
    flow_id: &str,
    version: i32,
) -> anyhow::Result<()> {
    client
        .execute(
            "UPDATE flows SET active = (version = $3) WHERE tenant_id = $1 AND flow_id = $2",
            &[&tenant, &flow_id, &version],
        )
        .await?;
    Ok(())
}
