//! Durable cancellation requests and the dispatcher-owned deadline sweep.
//!
//! A request is only a durable fact. The executor keeps authority while its
//! current attempt window is live; attempt completion observes the fact and
//! terminalizes. The bounded dispatcher sweep handles every other case.

use crate::RunStatus;

/// Typed result of [`request_cancellation_sql`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationRequestResult {
    Requested,
    AlreadyRequested,
    RunTerminal(RunStatus),
    CallerReleased,
    StaleGeneration,
    RequestConflict,
    NotFound,
}

impl CancellationRequestResult {
    pub fn from_parts(code: &str, run_status: &str) -> Option<Self> {
        match code {
            "requested" => Some(Self::Requested),
            "already-requested" => Some(Self::AlreadyRequested),
            "run-terminal" => Some(Self::RunTerminal(RunStatus::from_sql(run_status)?)),
            "caller-released" => Some(Self::CallerReleased),
            "stale-generation" => Some(Self::StaleGeneration),
            "request-conflict" => Some(Self::RequestConflict),
            "not-found" => Some(Self::NotFound),
            _ => None,
        }
    }
}

/// Persist a cancellation request under the generation observed by the caller.
///
/// Params: run id, cancellation kind, expected lease generation. The first
/// request wins; a retry with the same kind is idempotent. This transition does
/// not seize authority.
pub fn request_cancellation_sql() -> &'static str {
    "\
WITH input AS ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           $1::text AS run_id, $2::text AS cancel_kind, \
           $3::bigint AS expected_generation \
), \
locked_run AS MATERIALIZED ( \
    SELECT r.* FROM runs AS r, input AS i \
     WHERE r.tenant_id = i.tenant_id AND r.run_id = i.run_id \
     FOR UPDATE OF r \
), \
locked_queue AS MATERIALIZED ( \
    SELECT q.* FROM run_queue AS q \
      JOIN locked_run AS r USING (tenant_id, run_id) \
     FOR UPDATE OF q \
), \
classified AS ( \
    SELECT CASE \
             WHEN r.run_id IS NULL THEN 'not-found' \
             WHEN r.status IN ('completed','failed','cancelled','infrastructure-failure') \
               THEN 'run-terminal' \
             WHEN r.caller_released_at IS NOT NULL THEN 'caller-released' \
             WHEN q.run_id IS NULL OR q.lease_generation <> i.expected_generation \
               THEN 'stale-generation' \
             WHEN r.cancel_requested_kind IS NOT NULL \
              AND r.cancel_requested_kind <> i.cancel_kind THEN 'request-conflict' \
             WHEN r.cancel_requested_kind IS NOT NULL THEN 'already-requested' \
             ELSE 'ready' \
           END AS result_code, r.tenant_id, r.run_id, r.status, \
           r.cancel_requested_kind \
      FROM input AS i \
      LEFT JOIN locked_run AS r ON true \
      LEFT JOIN locked_queue AS q ON true \
), \
requested AS ( \
    UPDATE runs AS r \
       SET cancel_requested_kind = i.cancel_kind, cancel_requested_at = now(), \
           updated_at = now() \
      FROM input AS i, classified AS c \
     WHERE c.result_code = 'ready' \
       AND r.tenant_id = c.tenant_id AND r.run_id = c.run_id \
    RETURNING r.cancel_requested_kind \
) \
SELECT CASE WHEN x.cancel_requested_kind IS NOT NULL THEN 'requested' \
            ELSE c.result_code END AS result_code, \
       c.status AS run_status, \
       COALESCE(x.cancel_requested_kind, c.cancel_requested_kind) AS cancel_kind \
  FROM classified AS c LEFT JOIN requested AS x ON true"
}

/// Bounded dispatcher reconciliation for requests and elapsed deadlines.
///
/// The statement locks at most `$1` runs with `SKIP LOCKED`, rechecks all
/// predicates, defers a run while any started attempt still owns an unexpired
/// authority window, seizes its queue generation, terminalizes the run and an
/// unreleased caller, propagates a durable request to descendants, removes the
/// queue row, and emits one transactional waiter notification.
pub fn cancellation_sweep_sql() -> &'static str {
    "\
WITH candidates AS MATERIALIZED ( \
    SELECT tenant_id, run_id FROM runs \
     WHERE status IN ('dispatched','running') \
       AND (cancel_requested_at IS NOT NULL \
         OR (caller_released_at IS NULL \
             AND response_deadline_at IS NOT NULL AND response_deadline_at <= now()) \
         OR (run_deadline_at IS NOT NULL AND run_deadline_at <= now())) \
     ORDER BY COALESCE(cancel_requested_at, response_deadline_at, run_deadline_at), run_id \
     LIMIT $1 \
     FOR UPDATE SKIP LOCKED \
), \
eligible AS MATERIALIZED ( \
    SELECT r.tenant_id, r.run_id, r.flow_id, r.flow_version, \
           COALESCE(r.cancel_requested_kind, \
                    CASE WHEN r.caller_released_at IS NULL \
                               AND r.response_deadline_at IS NOT NULL \
                               AND r.response_deadline_at <= now() \
                         THEN 'response-deadline' \
                         WHEN r.run_deadline_at IS NOT NULL AND r.run_deadline_at <= now() \
                         THEN 'run-deadline' END) AS cancel_kind \
      FROM runs AS r JOIN candidates AS c USING (tenant_id, run_id) \
     WHERE r.status IN ('dispatched','running') \
       AND NOT EXISTS ( \
           SELECT 1 FROM node_runs AS n \
            WHERE n.tenant_id = r.tenant_id AND n.run_id = r.run_id \
              AND n.status = 'started' AND n.attempt_deadline_at > now() \
       ) \
), \
outcomes AS MATERIALIZED ( \
    SELECT e.*, \
           CASE WHEN e.cancel_kind = 'response-deadline' THEN 504 ELSE 499 END \
               AS caller_http_status, \
           '{\"error\":{\"code\":' || to_jsonb(e.cancel_kind)::text || \
           ',\"flow-id\":' || to_jsonb(e.flow_id)::text || \
           ',\"flow-version\":' || e.flow_version::text || \
           ',\"run-id\":' || to_jsonb(e.run_id)::text || '}}' AS canonical_text \
      FROM eligible AS e \
), \
seized AS ( \
    DELETE FROM run_queue AS q USING outcomes AS o \
     WHERE q.tenant_id = o.tenant_id AND q.run_id = o.run_id \
    RETURNING q.tenant_id, q.run_id, q.lease_generation + 1 AS seized_generation \
), \
terminalized AS ( \
    UPDATE runs AS r \
       SET status = 'cancelled', cancel_kind = o.cancel_kind, \
           terminal_reason = o.cancel_kind, \
           caller_outcome_kind = CASE WHEN r.caller_released_at IS NULL \
                                      THEN 'cancelled' ELSE r.caller_outcome_kind END, \
           caller_outcome_json = CASE WHEN r.caller_released_at IS NULL \
                                      THEN o.canonical_text::jsonb \
                                      ELSE r.caller_outcome_json END, \
           caller_http_status = CASE WHEN r.caller_released_at IS NULL \
                                     THEN o.caller_http_status \
                                     ELSE r.caller_http_status END, \
           caller_outcome_hash = CASE WHEN r.caller_released_at IS NULL \
                                      THEN 'sha256:' || encode( \
                                          sha256(convert_to(o.canonical_text, 'UTF8')), 'hex') \
                                      ELSE r.caller_outcome_hash END, \
           caller_released_at = COALESCE(r.caller_released_at, now()), \
           updated_at = now() \
      FROM outcomes AS o JOIN seized AS s USING (tenant_id, run_id) \
     WHERE r.tenant_id = o.tenant_id AND r.run_id = o.run_id \
       AND r.status IN ('dispatched','running') \
    RETURNING r.tenant_id, r.run_id, r.cancel_kind, s.seized_generation \
), \
descendants AS ( \
    WITH RECURSIVE tree AS ( \
        SELECT r.tenant_id, r.run_id, t.cancel_kind \
         FROM runs AS r JOIN terminalized AS t \
            ON r.tenant_id = t.tenant_id AND r.parent_run_id = t.run_id \
         WHERE r.status IN ('dispatched','running') AND r.caller_released_at IS NULL \
        UNION ALL \
        SELECT r.tenant_id, r.run_id, tree.cancel_kind \
          FROM runs AS r JOIN tree \
            ON r.tenant_id = tree.tenant_id AND r.parent_run_id = tree.run_id \
         WHERE r.status IN ('dispatched','running') AND r.caller_released_at IS NULL \
    ) SELECT * FROM tree \
), \
propagated AS ( \
    UPDATE runs AS r \
       SET cancel_requested_kind = COALESCE(r.cancel_requested_kind, \
                                            'parent-' || d.cancel_kind), \
           cancel_requested_at = COALESCE(r.cancel_requested_at, now()), \
           updated_at = now() \
      FROM descendants AS d \
     WHERE r.tenant_id = d.tenant_id AND r.run_id = d.run_id \
    RETURNING r.run_id \
), \
notified AS ( \
    SELECT pg_notify('wamn_run_outcome', t.tenant_id || ':' || t.run_id) \
      FROM terminalized AS t \
     WHERE (SELECT count(*) FROM propagated) >= 0 \
) \
SELECT t.run_id, t.cancel_kind, \
       (SELECT count(*) FROM notified) AS notification_count, \
       t.seized_generation \
  FROM terminalized AS t \
 WHERE (SELECT count(*) FROM notified) >= 0 \
 ORDER BY t.run_id"
}

#[cfg(test)]
mod tests {
    use super::{CancellationRequestResult, cancellation_sweep_sql, request_cancellation_sql};
    use crate::RunStatus;

    #[test]
    fn request_is_generation_fenced_and_never_seizes() {
        let sql = request_cancellation_sql();
        assert!(sql.contains("q.lease_generation <> i.expected_generation"));
        assert!(sql.contains("cancel_requested_kind = i.cancel_kind"));
        assert!(sql.contains("'caller-released'"));
        assert!(!sql.contains("lease_generation + 1"));
        assert!(!sql.contains("DELETE FROM run_queue"));
        assert_eq!(
            CancellationRequestResult::from_parts("run-terminal", "cancelled"),
            Some(CancellationRequestResult::RunTerminal(RunStatus::Cancelled))
        );
        assert_eq!(
            CancellationRequestResult::from_parts("stale-generation", "running"),
            Some(CancellationRequestResult::StaleGeneration)
        );
    }

    #[test]
    fn sweep_is_bounded_deferred_seizing_and_notifying() {
        let sql = cancellation_sweep_sql();
        assert!(sql.contains("LIMIT $1"));
        assert!(sql.contains("FOR UPDATE SKIP LOCKED"));
        assert!(sql.contains("n.attempt_deadline_at > now()"));
        assert!(sql.contains("q.lease_generation + 1 AS seized_generation"));
        assert!(sql.contains("DELETE FROM run_queue"));
        assert!(sql.contains("status = 'cancelled'"));
        assert!(sql.contains("sha256(convert_to(o.canonical_text, 'UTF8'))"));
        assert!(!sql.contains("jsonb::text"));
        assert!(sql.contains("THEN o.caller_http_status"));
        assert!(sql.contains("(SELECT count(*) FROM notified) AS notification_count"));
        assert!(sql.contains("t.seized_generation"));
        assert!(sql.contains("WITH RECURSIVE tree"));
        assert!(
            sql.matches("r.caller_released_at IS NULL").count() >= 4,
            "cancellation propagation must stop at every released child boundary"
        );
        assert!(sql.contains("pg_notify('wamn_run_outcome'"));
    }

    #[test]
    fn response_deadline_status_mapping_rejects_499_mutant() {
        let sql = cancellation_sweep_sql();
        assert!(
            sql.contains("CASE WHEN e.cancel_kind = 'response-deadline' THEN 504 ELSE 499 END"),
            "response deadline must be the sole 504 cancellation class"
        );
        assert!(
            sql.contains("r.caller_released_at IS NULL AND r.response_deadline_at IS NOT NULL"),
            "only an unreleased caller is eligible for response-deadline terminalization"
        );
        assert!(
            sql.contains(
                "WHEN r.caller_released_at IS NULL AND r.response_deadline_at IS NOT NULL"
            ),
            "response deadline must win the equal/elapsed run-deadline boundary"
        );
    }
}
