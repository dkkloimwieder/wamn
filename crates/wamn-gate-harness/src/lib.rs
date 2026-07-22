//! Shared measurement/assertion vocabulary for the gate suite (`wamn-gates`).
//!
//! The gates accreted per-bench copies of the same helpers (`percentile`
//! existed three times host-side); this crate is the single place they live
//! (docs/archive/structure-review.md SR1). Scope: pure, dependency-light helpers —
//! stats over collected samples, the PASS/FAIL check line, and small JSON
//! response asserts. Bench-specific machinery (harness structs, provisioning,
//! stepped clocks with a single consumer) stays in its bench module until a
//! second consumer pulls it here.

pub mod ceiling;

use std::path::PathBuf;
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

/// Print a measurement CSV between grep-able markers (the job-log extraction
/// path the ceiling campaigns use: `=== BEGIN CSV <name> ===` … `=== END CSV
/// <name> ===`) and optionally write it to `out_dir/<name>.csv`. A write
/// failure is reported but never fails the campaign — stdout always carries
/// the data.
pub fn emit_csv(name: &str, csv: &str, out_dir: &Option<PathBuf>) {
    println!("=== BEGIN CSV {name} ===");
    print!("{csv}");
    println!("=== END CSV {name} ===");
    if let Some(dir) = out_dir
        && let Err(e) = std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(dir.join(format!("{name}.csv")), csv))
    {
        println!("(could not write {name}.csv: {e})");
    }
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

/// Seed one 11.2 test-suite row (idempotent) for a flow version. Version-bound:
/// the `(tenant, flow_id, flow_version)` must already exist in `flows` (the FK).
/// Table names are unqualified — the caller's `search_path` (scope_session)
/// selects the schema.
pub async fn seed_test_suite(
    client: &tokio_postgres::Client,
    tenant: &str,
    flow_id: &str,
    flow_version: i32,
    suite_id: &str,
    name: &str,
) -> anyhow::Result<()> {
    client
        .execute(
            "INSERT INTO test_suites (tenant_id, flow_id, flow_version, suite_id, name) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (tenant_id, flow_id, flow_version, suite_id) \
               DO UPDATE SET name = excluded.name, updated_at = now()",
            &[&tenant, &flow_id, &flow_version, &suite_id, &name],
        )
        .await?;
    Ok(())
}

/// Seed one 11.2 test-case row (idempotent). The `case_json` BODY is opaque
/// jsonb (bound via `::text::jsonb`); FKs into the suite of the same
/// `(tenant, flow_id, flow_version, suite_id)`.
#[allow(clippy::too_many_arguments)]
pub async fn seed_test_case(
    client: &tokio_postgres::Client,
    tenant: &str,
    flow_id: &str,
    flow_version: i32,
    suite_id: &str,
    case_id: &str,
    ordinal: i32,
    case_json: &str,
) -> anyhow::Result<()> {
    client
        .execute(
            "INSERT INTO test_cases \
               (tenant_id, flow_id, flow_version, suite_id, case_id, ordinal, case_body) \
             VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb) \
             ON CONFLICT (tenant_id, flow_id, flow_version, suite_id, case_id) \
               DO UPDATE SET ordinal = excluded.ordinal, case_body = excluded.case_body",
            &[
                &tenant,
                &flow_id,
                &flow_version,
                &suite_id,
                &case_id,
                &ordinal,
                &case_json,
            ],
        )
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

// The test module lives at the end so no items follow it
// (clippy::items_after_test_module).
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
