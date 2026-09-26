//! Parameterized SQL for the workflow contract: admit, park, release, and list.
//!
//! A park moves the queue row's `available_at` to `infinity`, so the claim's
//! `available_at <= now()` never takes it. The run keeps its status
//! `dispatched`: the run-level `parked` status stays retired. Table names stay
//! unqualified, so the caller's transaction `search_path` selects the run plane,
//! and every statement reads its tenant from the `app.tenant` claim.
//!
//! The pure tests cover the statement text only. The live half is the workflow
//! contract test in `wamn-workflow`, over a disposable database.

use crate::RunStatus;

/// The `available_at` of a parked queue row.
const PARKED_AT: &str = "'infinity'::timestamptz";

/// Admit one automation run. `ON CONFLICT DO NOTHING` on the idempotency key
/// returns no row for a repeated key, and the caller then reads the first run
/// with [`select_automation_run_sql`].
///
/// Binds: `$1` tenant, `$2` package, `$3` effective release, `$4` environment,
/// `$5` wiring id, `$6` wiring version, `$7` wiring hash, `$8` service
/// principal, `$9` idempotency key, `$10` input JSON text, `$11` durability
/// class.
pub fn insert_automation_run_sql() -> String {
    format!(
        "INSERT INTO runs (tenant_id, package_id, effective_release_id, environment, \
             wiring_id, wiring_version, wiring_hash, trigger_source, service_principal_id, \
             idempotency_key, input_json, status, durability_class) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'automation', $8::text::uuid, $9, \
                 $10::text::jsonb, '{dispatched}', $11) \
         ON CONFLICT (tenant_id, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING \
         RETURNING run_id",
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// The run a repeated automation start admitted first. It returns no row when
/// the key belongs to a different request.
///
/// Binds are those of [`insert_automation_run_sql`] without the durability
/// class.
pub fn select_automation_run_sql() -> &'static str {
    "SELECT run_id FROM runs WHERE tenant_id = $1 AND package_id = $2 \
        AND effective_release_id = $3 AND environment = $4 AND wiring_id = $5 \
        AND wiring_version = $6 AND wiring_hash = $7 AND trigger_source = 'automation' \
        AND service_principal_id = $8::text::uuid AND idempotency_key = $9 \
        AND input_json = $10::text::jsonb"
}

/// Queue one admitted run. Binds: `$1` tenant, `$2` run id.
pub fn insert_run_queue_sql() -> &'static str {
    "INSERT INTO run_queue (tenant_id, run_id) VALUES ($1, $2)"
}

/// Park a queued run that no replica holds. It returns the run id, or no row
/// when the run is not queued, is running, or is parked already.
///
/// Binds: `$1` run id, `$2` environment.
pub fn park_queued_run_sql() -> String {
    format!(
        "UPDATE run_queue AS q SET available_at = {PARKED_AT} \
           FROM runs AS r \
          WHERE q.tenant_id = current_setting('app.tenant', true) \
            AND q.run_id = $1 \
            AND r.tenant_id = q.tenant_id AND r.run_id = q.run_id \
            AND r.environment = $2 \
            AND r.status = '{dispatched}' \
            AND (q.lease_expires_at IS NULL OR q.lease_expires_at <= now()) \
            AND q.available_at <> {PARKED_AT} \
          RETURNING q.run_id",
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// Return a parked run to the queue. It returns the run id, or no row when the
/// run is not parked.
///
/// Binds: `$1` run id, `$2` environment.
pub fn release_parked_run_sql() -> String {
    format!(
        "UPDATE run_queue AS q SET available_at = now() \
           FROM runs AS r \
          WHERE q.tenant_id = current_setting('app.tenant', true) \
            AND q.run_id = $1 \
            AND r.tenant_id = q.tenant_id AND r.run_id = q.run_id \
            AND r.environment = $2 \
            AND q.available_at = {PARKED_AT} \
          RETURNING q.run_id"
    )
}

/// The runs of one environment, newest first. `queued` is whether a queue row
/// exists, and `parked` whether that row is parked.
///
/// Binds: `$1` environment, `$2` limit. Columns: run id, package, wiring id,
/// wiring version, trigger source, status, queued, parked, created at (RFC
/// 3339 text).
pub fn list_workflow_runs_sql() -> String {
    format!(
        "SELECT r.run_id, r.package_id, r.wiring_id, r.wiring_version, r.trigger_source, \
                r.status, q.run_id IS NOT NULL AS queued, \
                COALESCE(q.available_at = {PARKED_AT}, false) AS parked, \
                to_char(r.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
           FROM runs AS r \
           LEFT JOIN run_queue AS q ON q.tenant_id = r.tenant_id AND q.run_id = r.run_id \
          WHERE r.tenant_id = current_setting('app.tenant', true) \
            AND r.environment = $1 \
            AND r.wiring_id IS NOT NULL \
          ORDER BY r.created_at DESC, r.run_id DESC \
          LIMIT $2"
    )
}

/// The facts that name why a park or a release changed nothing: the run's
/// status, whether a queue row exists, whether it is parked, and whether a
/// replica holds a live lease. No row means the run does not exist.
///
/// Binds: `$1` run id, `$2` environment.
pub fn select_run_queue_state_sql() -> String {
    format!(
        "SELECT r.status, q.run_id IS NOT NULL AS queued, \
                COALESCE(q.available_at = {PARKED_AT}, false) AS parked, \
                COALESCE(q.lease_expires_at > now(), false) AS leased \
           FROM runs AS r \
           LEFT JOIN run_queue AS q ON q.tenant_id = r.tenant_id AND q.run_id = r.run_id \
          WHERE r.tenant_id = current_setting('app.tenant', true) \
            AND r.run_id = $1 AND r.environment = $2"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_park_is_the_claims_visibility_gate_and_never_a_run_status() {
        let park = park_queued_run_sql();
        assert!(park.contains("SET available_at = 'infinity'::timestamptz"));
        assert!(park.contains("r.status = 'dispatched'"));
        let claim = super::super::select_production_claim_sql();
        assert!(
            claim.contains("q.available_at <= now()"),
            "the claim must skip a row whose available_at is infinity"
        );
        assert!(release_parked_run_sql().contains("SET available_at = now()"));
    }
}
