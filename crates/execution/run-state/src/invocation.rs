//! Durable queries owned by the production flow-invocation service.
//!
//! Resolution deliberately includes disabled definitions. Authentication is an
//! adapter concern, but the selected source document is returned before the
//! admissions ledger can be queried, preserving FLOW-SPEC rev18 §6.2's order.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct InvocationTarget {
    pub catalog_version: i32,
    pub definition_hash: String,
    pub flow_id: String,
    pub flow_version: i32,
    pub definition: Value,
    pub auth_policy: Value,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InvocationOutcome {
    pub run_id: String,
    pub kind: String,
    pub body: Value,
    pub http_status: Option<u16>,
    pub hash: String,
    pub flow_id: String,
    pub flow_version: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InvocationRecovery {
    Missing,
    InFlight { run_id: String },
    Released(InvocationOutcome),
    IdempotencyKeyReused,
    IdempotencyScopeChanged,
    OutcomeExpired,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InvocationPoll {
    Running,
    Released(InvocationOutcome),
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationCancelResult {
    Requested,
    AlreadyRequested,
    AlreadyReleased,
    RunTerminal,
    FenceLost,
    NotFound,
}

/// Resolve an HTTP attachment from the applied release, even when disabled.
///
/// Params: catalog id, environment, attachment id. Tombstoned/removed routes
/// return no row and therefore end transparent recovery.
pub fn resolve_invocation_target_sql() -> &'static str {
    "\
SELECT h.applied_catalog_version, a.definition_hash, a.flow_id, f.flow_version, \
       a.definition_json::text, s.definition_json::text, \
       COALESCE(x.enabled AND x.confirmed_definition_hash = a.definition_hash, false) \
  FROM catalog.catalog_heads AS h \
  JOIN catalog.release_attachments AS a \
    ON a.tenant_id = h.tenant_id AND a.catalog_id = h.catalog_id \
   AND a.catalog_version = h.applied_catalog_version \
  JOIN catalog.release_flows AS f \
    ON f.tenant_id = a.tenant_id AND f.catalog_id = a.catalog_id \
   AND f.catalog_version = a.catalog_version AND f.flow_id = a.flow_id \
  JOIN catalog.release_sources AS s \
    ON s.tenant_id = a.tenant_id AND s.catalog_id = a.catalog_id \
   AND s.catalog_version = a.catalog_version AND s.source_id = a.source_id \
  LEFT JOIN catalog.attachment_activation AS x \
    ON x.tenant_id = h.tenant_id AND x.catalog_id = h.catalog_id \
   AND x.environment = h.environment AND x.attachment_id = a.attachment_id \
 WHERE h.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
   AND h.catalog_id = $1 AND h.environment = $2 AND a.attachment_id = $3 \
   AND a.attachment_kind IN ('http', 'studio') \
   AND NOT EXISTS ( \
       SELECT 1 FROM catalog.attachment_tombstones AS dead \
        WHERE dead.tenant_id = h.tenant_id AND dead.catalog_id = h.catalog_id \
          AND dead.environment = h.environment \
          AND dead.attachment_id = a.attachment_id \
   )"
}

/// Look up the admissions ledger after route-policy authentication.
///
/// Params: catalog id, environment, attachment id, principal digest,
/// client-key digest, current definition hash, request fingerprint.
pub fn lookup_invocation_recovery_sql() -> &'static str {
    "\
SELECT CASE \
         WHEN a.run_id IS NULL THEN 'missing' \
         WHEN a.definition_hash <> $6 THEN 'idempotency-scope-changed' \
         WHEN a.client_request_fingerprint <> $7 THEN 'idempotency-key-reused' \
         WHEN a.expires_at <= now() THEN 'outcome-expired' \
         WHEN r.caller_released_at IS NULL THEN 'in-flight' \
         ELSE 'released' \
       END AS result_code, \
       a.run_id, r.caller_outcome_kind, r.caller_outcome_json::text, \
       r.caller_http_status, r.caller_outcome_hash, r.flow_id, r.flow_version \
  FROM (SELECT 1) AS one \
  LEFT JOIN wamn_run.invocation_admissions AS a \
    ON a.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
   AND a.catalog_id = $1 AND a.environment = $2 AND a.attachment_id = $3 \
   AND a.principal_digest = $4 AND a.client_key_digest = $5 \
  LEFT JOIN wamn_run.runs AS r \
    ON r.tenant_id = a.tenant_id AND r.run_id = a.run_id"
}

pub fn poll_invocation_outcome_sql() -> &'static str {
    "\
SELECT CASE WHEN r.run_id IS NULL THEN 'not-found' \
            WHEN r.caller_released_at IS NULL THEN 'running' \
            ELSE 'released' END AS result_code, \
       r.run_id, r.caller_outcome_kind, r.caller_outcome_json::text, \
       r.caller_http_status, r.caller_outcome_hash, r.flow_id, r.flow_version \
  FROM (SELECT 1) AS one \
  LEFT JOIN wamn_run.runs AS r \
    ON r.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
   AND r.run_id = $1"
}

/// Request observed-disconnect cancellation under inline ownership.
///
/// Params: run id, executor id, expected generation. This records a durable
/// request but never seizes a live attempt's authority.
pub fn cancel_inline_invocation_sql() -> &'static str {
    "\
WITH input AS ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           $1::text AS run_id, $2::text AS executor_id, \
           $3::bigint AS expected_generation \
), \
locked_run AS MATERIALIZED ( \
    SELECT r.* FROM wamn_run.runs AS r, input AS i \
     WHERE r.tenant_id = i.tenant_id AND r.run_id = i.run_id \
     FOR UPDATE OF r \
), \
locked_queue AS MATERIALIZED ( \
    SELECT q.* FROM wamn_run.run_queue AS q \
      JOIN locked_run AS r USING (tenant_id, run_id) \
     FOR UPDATE OF q \
), \
classified AS ( \
    SELECT CASE \
             WHEN r.run_id IS NULL THEN 'not-found' \
             WHEN r.status IN ('completed','failed','cancelled','infrastructure-failure') \
               THEN 'run-terminal' \
             WHEN r.caller_released_at IS NOT NULL THEN 'already-released' \
             WHEN q.run_id IS NULL OR q.lease_owner IS DISTINCT FROM i.executor_id \
               OR q.lease_generation IS DISTINCT FROM i.expected_generation \
               THEN 'fence-lost' \
             WHEN r.cancel_requested_kind IS NOT NULL \
              AND r.cancel_requested_kind <> 'observed-disconnect' THEN 'fence-lost' \
             WHEN r.cancel_requested_kind IS NOT NULL THEN 'already-requested' \
             ELSE 'ready' \
           END AS result_code, r.tenant_id, r.run_id \
      FROM input AS i \
      LEFT JOIN locked_run AS r ON true \
      LEFT JOIN locked_queue AS q ON true \
), \
requested AS ( \
    UPDATE wamn_run.runs AS r \
       SET cancel_requested_kind = 'observed-disconnect', \
           cancel_requested_at = now(), updated_at = now() \
      FROM classified AS c \
     WHERE c.result_code = 'ready' \
       AND r.tenant_id = c.tenant_id AND r.run_id = c.run_id \
    RETURNING r.run_id \
) \
SELECT CASE WHEN x.run_id IS NOT NULL THEN 'requested' ELSE c.result_code END \
  FROM classified AS c LEFT JOIN requested AS x ON true"
}

pub fn decode_invocation_cancel(code: &str) -> Option<InvocationCancelResult> {
    match code {
        "requested" => Some(InvocationCancelResult::Requested),
        "already-requested" => Some(InvocationCancelResult::AlreadyRequested),
        "already-released" => Some(InvocationCancelResult::AlreadyReleased),
        "run-terminal" => Some(InvocationCancelResult::RunTerminal),
        "fence-lost" => Some(InvocationCancelResult::FenceLost),
        "not-found" => Some(InvocationCancelResult::NotFound),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_order_resolves_disabled_policy_before_the_ledger() {
        let resolve = resolve_invocation_target_sql();
        assert!(resolve.contains("release_sources AS s"));
        assert!(resolve.contains("LEFT JOIN catalog.attachment_activation"));
        assert!(!resolve.contains("WHERE x.enabled"));
        assert!(resolve.contains("attachment_tombstones"));

        let recovery = lookup_invocation_recovery_sql();
        let scope = recovery.find("a.definition_hash <> $6").unwrap();
        let body = recovery.find("a.client_request_fingerprint <> $7").unwrap();
        assert!(scope < body);
        assert!(body < recovery.find("a.expires_at <= now()").unwrap());
    }

    #[test]
    fn polling_returns_only_exact_stored_columns() {
        let sql = poll_invocation_outcome_sql();
        for column in [
            "caller_outcome_kind",
            "caller_outcome_json::text",
            "caller_http_status",
            "caller_outcome_hash",
            "flow_id",
            "flow_version",
        ] {
            assert!(sql.contains(column), "missing {column}");
        }
        assert!(!sql.contains("COALESCE(r.caller_http_status"));
    }

    #[test]
    fn cancellation_is_owner_and_generation_fenced() {
        let sql = cancel_inline_invocation_sql();
        assert!(sql.contains("q.lease_owner IS DISTINCT FROM i.executor_id"));
        assert!(sql.contains("q.lease_generation IS DISTINCT FROM i.expected_generation"));
        assert!(sql.contains("cancel_requested_kind = 'observed-disconnect'"));
        assert!(!sql.contains("lease_generation + 1"));
        assert_eq!(
            decode_invocation_cancel("fence-lost"),
            Some(InvocationCancelResult::FenceLost)
        );
    }
}
