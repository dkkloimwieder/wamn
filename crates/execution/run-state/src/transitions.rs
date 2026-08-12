//! Fenced run-state transitions.
//!
//! Every executor-owned mutation joins `runs` to the queue-owned
//! `(lease_owner, lease_generation)` authority and returns one typed result row.
//! The explicit authority run id is deliberately separate from the target run
//! id: accidentally presenting one run's fence while mutating another is a
//! typed `cross-run-authority` refusal, not an empty update.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{NodeRunStatus, RunStatus};

pub(crate) const FENCED_PREFIX: &str = "\
WITH input AS ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           $1::text AS target_run_id, $2::text AS authority_run_id, \
           $3::text AS lease_owner, $4::bigint AS lease_generation \
), \
locked_run AS MATERIALIZED ( \
    SELECT r.* FROM runs AS r, input AS i \
     WHERE i.target_run_id = i.authority_run_id \
       AND r.tenant_id = i.tenant_id AND r.run_id = i.target_run_id \
     FOR UPDATE OF r \
), \
locked_queue AS MATERIALIZED ( \
    SELECT q.* FROM run_queue AS q \
      JOIN locked_run AS r \
        ON r.tenant_id = q.tenant_id AND r.run_id = q.run_id \
     FOR UPDATE OF q \
), \
authority AS ( \
    SELECT CASE \
             WHEN i.target_run_id <> i.authority_run_id THEN 'cross-run-authority' \
             WHEN r.run_id IS NULL THEN 'not-found' \
             WHEN r.status IN ('completed', 'failed', 'infrastructure-failure') \
               THEN 'run-terminal' \
             WHEN q.run_id IS NULL \
               OR q.lease_owner IS DISTINCT FROM i.lease_owner \
               OR q.lease_generation IS DISTINCT FROM i.lease_generation \
               THEN 'fence-lost' \
             ELSE 'ready' \
           END AS result_code, \
           r.tenant_id, r.run_id, r.status, \
           r.execution_bundle_hash, \
           r.caller_outcome_kind, r.caller_outcome_json, \
           r.caller_http_status, r.caller_release_node_id, \
           r.caller_outcome_hash, r.caller_released_at, r.run_deadline_at \
      FROM input AS i \
      LEFT JOIN locked_run AS r ON true \
      LEFT JOIN locked_queue AS q ON true \
)";

/// A caller outcome returned with `already-released`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StoredCallerOutcome {
    pub kind: String,
    pub body: Value,
    pub http_status: Option<u16>,
    pub release_node_id: Option<String>,
    pub hash: Option<String>,
}

impl StoredCallerOutcome {
    /// Whether an idempotent caller-release replay names the exact outcome that
    /// already won the CAS. Body and hash are both compared: the hash is the
    /// persisted replay identity, while the body comparison guards a corrupt
    /// or incorrectly decoded row.
    pub fn exactly_matches(
        &self,
        kind: &str,
        body: &Value,
        http_status: Option<u16>,
        release_node_id: Option<&str>,
        hash: &str,
    ) -> bool {
        self.kind == kind
            && &self.body == body
            && self.http_status == http_status
            && self.release_node_id.as_deref() == release_node_id
            && self.hash.as_deref() == Some(hash)
    }
}

/// Typed result of [`release_caller_sql`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerReleaseResult {
    Released,
    AlreadyReleased(StoredCallerOutcome),
    RunTerminal(RunStatus),
    FenceLost,
    CrossRunAuthority,
    NotFound,
}

impl CallerReleaseResult {
    /// Decode the result row returned by [`release_caller_sql`].
    pub fn from_parts(
        code: &str,
        run_status: &str,
        kind: Option<String>,
        body: Option<Value>,
        http_status: Option<u16>,
        release_node_id: Option<String>,
        hash: Option<String>,
    ) -> Option<CallerReleaseResult> {
        match code {
            "released" => Some(CallerReleaseResult::Released),
            "already-released" => Some(CallerReleaseResult::AlreadyReleased(StoredCallerOutcome {
                kind: kind?,
                body: body?,
                http_status,
                release_node_id,
                hash,
            })),
            "run-terminal" => Some(CallerReleaseResult::RunTerminal(RunStatus::from_sql(
                run_status,
            )?)),
            "fence-lost" => Some(CallerReleaseResult::FenceLost),
            "cross-run-authority" => Some(CallerReleaseResult::CrossRunAuthority),
            "not-found" => Some(CallerReleaseResult::NotFound),
            _ => None,
        }
    }

    /// `FenceLost` is absolute: callers must stop without another store access.
    pub fn permits_access(&self) -> bool {
        !matches!(self, CallerReleaseResult::FenceLost)
    }
}

/// Release the durable caller outcome under the queue fence.
///
/// Params: target run id, authority run id, lease owner, lease generation,
/// outcome kind, outcome JSON text, HTTP status, release node id, outcome hash.
/// The one returned row is `(result_code, run_status, kind, body_text,
/// http_status, release_node_id, hash)`.
pub fn release_caller_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         classified AS ( \
             SELECT CASE \
                      WHEN a.result_code <> 'ready' THEN a.result_code \
                      WHEN a.caller_released_at IS NOT NULL THEN 'already-released' \
                      ELSE 'ready' \
                    END AS result_code, \
                    a.tenant_id, a.run_id, a.status, \
                    a.caller_outcome_kind, a.caller_outcome_json, \
                    a.caller_http_status, a.caller_release_node_id, \
                    a.caller_outcome_hash, a.caller_released_at \
               FROM authority AS a \
         ), \
         released AS ( \
             UPDATE runs AS r \
                SET caller_outcome_kind = $5, \
                    caller_outcome_json = $6::text::jsonb, \
                    caller_http_status = $7, \
                    caller_release_node_id = $8, \
                    caller_outcome_hash = $9, \
                    caller_released_at = now(), updated_at = now() \
               FROM classified AS c \
              WHERE c.result_code = 'ready' \
                AND r.tenant_id = c.tenant_id AND r.run_id = c.run_id \
             RETURNING r.status, r.caller_outcome_kind, r.caller_outcome_json, \
                       r.caller_http_status, r.caller_release_node_id, \
                       r.caller_outcome_hash \
         ) \
         SELECT CASE WHEN x.status IS NOT NULL THEN 'released' ELSE c.result_code END \
                    AS result_code, \
                COALESCE(x.status, c.status) AS run_status, \
                COALESCE(x.caller_outcome_kind, c.caller_outcome_kind) AS outcome_kind, \
                COALESCE(x.caller_outcome_json, c.caller_outcome_json)::text AS outcome_json, \
                COALESCE(x.caller_http_status, c.caller_http_status) AS http_status, \
                COALESCE(x.caller_release_node_id, c.caller_release_node_id) \
                    AS release_node_id, \
                COALESCE(x.caller_outcome_hash, c.caller_outcome_hash) AS outcome_hash \
           FROM classified AS c LEFT JOIN released AS x ON true"
    )
}

/// Typed result of [`terminalize_sql`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalizeResult {
    Terminalized,
    RunTerminal(RunStatus),
    CallerUnreleased,
    FenceLost,
    CrossRunAuthority,
    NotFound,
}

impl TerminalizeResult {
    pub fn from_parts(code: &str, run_status: &str) -> Option<TerminalizeResult> {
        match code {
            "terminalized" => Some(TerminalizeResult::Terminalized),
            "run-terminal" => Some(TerminalizeResult::RunTerminal(RunStatus::from_sql(
                run_status,
            )?)),
            "caller-unreleased" => Some(TerminalizeResult::CallerUnreleased),
            "fence-lost" => Some(TerminalizeResult::FenceLost),
            "cross-run-authority" => Some(TerminalizeResult::CrossRunAuthority),
            "not-found" => Some(TerminalizeResult::NotFound),
            _ => None,
        }
    }

    /// `FenceLost` is absolute: callers must stop without another store access.
    pub fn permits_access(self) -> bool {
        self != TerminalizeResult::FenceLost
    }
}

/// Typed result of [`reserved_checkpoint_sql`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedCheckpointResult {
    Recorded,
    RunTerminal(RunStatus),
    FenceLost,
    CrossRunAuthority,
    NotFound,
}

impl ReservedCheckpointResult {
    pub fn from_parts(code: &str, run_status: &str) -> Option<ReservedCheckpointResult> {
        match code {
            "recorded" => Some(ReservedCheckpointResult::Recorded),
            "run-terminal" => Some(ReservedCheckpointResult::RunTerminal(RunStatus::from_sql(
                run_status,
            )?)),
            "fence-lost" => Some(ReservedCheckpointResult::FenceLost),
            "cross-run-authority" => Some(ReservedCheckpointResult::CrossRunAuthority),
            "not-found" => Some(ReservedCheckpointResult::NotFound),
            _ => None,
        }
    }

    /// `FenceLost` is absolute: callers must stop without another store access.
    pub fn permits_access(self) -> bool {
        self != ReservedCheckpointResult::FenceLost
    }
}

/// Record an engine-reserved checkpoint and renew its queue lease under the
/// exact claim generation.
///
/// Params: target run id, authority run id, lease owner, lease generation,
/// local node id, occurrence, sequence, output port, output JSON text, input JSON
/// text, preview head, payload size, payload hash, capture mode, redacted,
/// lease TTL milliseconds. A replayed checkpoint still renews, but a stale
/// owner or generation cannot insert the checkpoint.
pub fn reserved_checkpoint_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         recorded AS ( \
             INSERT INTO node_runs \
                    (tenant_id, run_id, frame_id, parent_frame_id, call_site_id, current_plan_hash, \
                     local_node_id, occurrence, seq, status, \
                     output_port, output_json, input_json, preview_head, \
                     payload_size, payload_hash, capture_mode, redacted) \
             SELECT a.tenant_id, a.run_id, 0, NULL, NULL, a.execution_bundle_hash, \
                    $5, $6, $7, '{success}', \
                    $8, $9::text::jsonb, $10::text::jsonb, $11, $12, $13, $14, $15 \
               FROM authority AS a \
              WHERE a.result_code = 'ready' \
             ON CONFLICT (tenant_id, run_id, frame_id, local_node_id, occurrence) DO NOTHING \
             RETURNING run_id \
         ), \
         renewed AS ( \
             UPDATE run_queue AS q \
                SET lease_expires_at = \
                    now() + ($16::bigint * interval '1 millisecond') \
              FROM authority AS a \
              WHERE a.result_code = 'ready' \
                AND q.tenant_id = a.tenant_id AND q.run_id = a.run_id \
                AND (SELECT count(*) FROM recorded) >= 0 \
             RETURNING q.run_id \
         ) \
         SELECT CASE WHEN r.run_id IS NOT NULL THEN 'recorded' ELSE a.result_code END \
                    AS result_code, \
                a.status AS run_status \
           FROM authority AS a LEFT JOIN renewed AS r ON true",
        success = NodeRunStatus::Success.as_sql(),
    )
}

/// Record a successful user-node emission and its replacement context under the
/// exact queue fence.
///
/// Params match [`reserved_checkpoint_sql`] through `$15`, followed by the
/// complete context JSON document (`$16`) and lease TTL milliseconds (`$17`).
/// The node output and context checkpoint are one statement: a stale owner or
/// generation writes neither.
pub fn node_context_checkpoint_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         recorded AS ( \
             INSERT INTO node_runs \
                    (tenant_id, run_id, frame_id, parent_frame_id, call_site_id, current_plan_hash, \
                     local_node_id, occurrence, seq, status, \
                     output_port, output_json, input_json, preview_head, \
                     payload_size, payload_hash, capture_mode, redacted) \
             SELECT a.tenant_id, a.run_id, 0, NULL, NULL, a.execution_bundle_hash, \
                    $5, $6, $7, '{success}', \
                    $8, $9::text::jsonb, $10::text::jsonb, $11, $12, $13, $14, $15 \
               FROM authority AS a \
              WHERE a.result_code = 'ready' \
                AND jsonb_typeof($16::text::jsonb) = 'object' \
             ON CONFLICT (tenant_id, run_id, frame_id, local_node_id, occurrence) DO NOTHING \
             RETURNING run_id \
         ), \
         checkpointed AS ( \
             UPDATE runs AS r \
                SET state_json = jsonb_set(COALESCE(r.state_json, '{{}}'::jsonb), \
                                           '{{context}}', $16::text::jsonb, true), \
                    updated_at = now() \
               FROM authority AS a \
              WHERE a.result_code = 'ready' \
                AND r.tenant_id = a.tenant_id AND r.run_id = a.run_id \
                AND (SELECT count(*) FROM recorded) = 1 \
             RETURNING r.run_id \
         ), \
         renewed AS ( \
             UPDATE run_queue AS q \
                SET lease_expires_at = \
                    now() + ($17::bigint * interval '1 millisecond') \
               FROM authority AS a \
              WHERE a.result_code = 'ready' \
                AND q.tenant_id = a.tenant_id AND q.run_id = a.run_id \
                AND (SELECT count(*) FROM checkpointed) = 1 \
             RETURNING q.run_id \
         ) \
         SELECT CASE WHEN r.run_id IS NOT NULL THEN 'recorded' ELSE a.result_code END \
                    AS result_code, \
                a.status AS run_status \
           FROM authority AS a LEFT JOIN renewed AS r ON true",
        success = NodeRunStatus::Success.as_sql(),
    )
}

/// Take the first durable terminal result and remove its queue row atomically.
///
/// Params: target run id, authority run id, lease owner, lease generation,
/// terminal status, terminal reason, result JSON text.
/// Completing a caller-attached run before caller release is a typed refusal.
pub fn terminalize_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         classified AS ( \
             SELECT CASE \
                      WHEN a.result_code <> 'ready' THEN a.result_code \
                      WHEN $5::text = 'completed' \
                       AND a.caller_released_at IS NULL \
                       AND EXISTS (SELECT 1 FROM locked_run \
                                    WHERE trigger_source IN ('http','internal','studio')) \
                        THEN 'caller-unreleased' \
                      ELSE 'ready' \
                    END AS result_code, \
                    a.tenant_id, a.run_id, a.status, a.caller_released_at \
               FROM authority AS a \
         ), \
         terminalized AS ( \
             UPDATE runs AS r \
                SET status = $5, terminal_reason = $6, \
                    result_json = $7::text::jsonb, updated_at = now() \
               FROM classified AS c \
              WHERE c.result_code = 'ready' \
                AND r.tenant_id = c.tenant_id AND r.run_id = c.run_id \
             RETURNING r.tenant_id, r.run_id, r.flow_id, r.status \
         ), \
         dead_lettered AS ( \
             INSERT INTO run_dead_letters \
                    (tenant_id, run_id, partition_key, flow_id, reason) \
             SELECT t.tenant_id, t.run_id, q.partition_key, t.flow_id, \
                    COALESCE($6::text, 'failed') \
               FROM terminalized AS t \
               JOIN run_queue AS q \
                 ON q.tenant_id = t.tenant_id AND q.run_id = t.run_id \
              WHERE t.status = 'failed' \
                AND q.partition_key IS NOT NULL \
                AND q.partition_policy = 'blocking' \
             ON CONFLICT (tenant_id, run_id) DO NOTHING \
             RETURNING run_id \
         ), \
         dequeued AS ( \
             DELETE FROM run_queue AS q USING terminalized AS t \
              WHERE q.tenant_id = t.tenant_id AND q.run_id = t.run_id \
                AND (SELECT count(*) FROM dead_lettered) >= 0 \
             RETURNING q.run_id \
         ) \
         SELECT CASE WHEN t.run_id IS NOT NULL THEN 'terminalized' ELSE c.result_code END \
                    AS result_code, \
                COALESCE(t.status, c.status) AS run_status \
           FROM classified AS c LEFT JOIN terminalized AS t ON true \
          WHERE (SELECT count(*) FROM dequeued) >= 0"
    )
}

/// Typed result shared by fenced checkpoint transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointResult {
    Applied,
    AlreadyCompleted,
    AttemptNotFound,
    AttemptNotStarted,
    RunTerminal(RunStatus),
    FenceLost,
    CrossRunAuthority,
    NotFound,
}

impl CheckpointResult {
    pub fn from_parts(code: &str, run_status: &str) -> Option<CheckpointResult> {
        match code {
            "completed" => Some(CheckpointResult::Applied),
            "already-completed" => Some(CheckpointResult::AlreadyCompleted),
            "attempt-not-found" => Some(CheckpointResult::AttemptNotFound),
            "attempt-not-started" => Some(CheckpointResult::AttemptNotStarted),
            "run-terminal" => Some(CheckpointResult::RunTerminal(RunStatus::from_sql(
                run_status,
            )?)),
            "fence-lost" => Some(CheckpointResult::FenceLost),
            "cross-run-authority" => Some(CheckpointResult::CrossRunAuthority),
            "not-found" => Some(CheckpointResult::NotFound),
            _ => None,
        }
    }

    /// `FenceLost` is absolute: callers must stop without another store access.
    pub fn permits_access(self) -> bool {
        self != CheckpointResult::FenceLost
    }
}

/// Complete one persisted effect attempt and advance the run checkpoint.
///
/// Params: target run id, authority run id, lease owner, lease generation,
/// local node id, occurrence, output port, output JSON text, state JSON text.
pub fn complete_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         locked_attempt AS MATERIALIZED ( \
             SELECT n.* FROM node_runs AS n, authority AS a \
              WHERE a.result_code = 'ready' \
                AND n.tenant_id = a.tenant_id AND n.run_id = a.run_id \
                AND n.frame_id = 0 AND n.local_node_id = $5 AND n.occurrence = $6 \
              FOR UPDATE OF n \
         ), \
         classified AS ( \
             SELECT CASE \
                      WHEN a.result_code <> 'ready' THEN a.result_code \
                      WHEN n.run_id IS NULL THEN 'attempt-not-found' \
                      WHEN n.status IN ('success', 'error') THEN 'already-completed' \
                      WHEN n.status <> 'started' THEN 'attempt-not-started' \
                      ELSE 'ready' \
                    END AS result_code, a.tenant_id, a.run_id, a.status \
               FROM authority AS a LEFT JOIN locked_attempt AS n ON true \
         ), \
         completed_attempt AS ( \
             UPDATE node_runs AS n \
                SET status = 'success', output_port = $7, \
                    output_json = $8::text::jsonb, ended_at = now() \
               FROM classified AS c \
              WHERE c.result_code = 'ready' \
                AND n.tenant_id = c.tenant_id AND n.run_id = c.run_id \
                AND n.frame_id = 0 AND n.local_node_id = $5 AND n.occurrence = $6 \
             RETURNING n.tenant_id, n.run_id \
         ), \
         checkpointed AS ( \
             UPDATE runs AS r SET state_json = $9::text::jsonb, updated_at = now() \
               FROM completed_attempt AS n \
              WHERE r.tenant_id = n.tenant_id AND r.run_id = n.run_id \
             RETURNING r.run_id, r.status \
         ) \
         SELECT CASE WHEN p.run_id IS NOT NULL THEN 'completed' ELSE c.result_code END \
                    AS result_code, \
                COALESCE(p.status, c.status) AS run_status \
           FROM classified AS c LEFT JOIN checkpointed AS p ON true"
    )
}

/// Complete a successful attempt, checkpoint replacement context, and renew.
///
/// Params: fence `$1..$4`, local node id, occurrence, output port, captured output,
/// captured input, preview, size, hash, capture mode, redacted, replacement
/// context document, and lease TTL milliseconds.
pub fn complete_attempt_success_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         locked_attempt AS MATERIALIZED ( \
             SELECT n.* FROM node_runs AS n, authority AS a \
              WHERE a.result_code = 'ready' \
                AND n.tenant_id = a.tenant_id AND n.run_id = a.run_id \
                AND n.frame_id = 0 AND n.local_node_id = $5 AND n.occurrence = $6 \
              FOR UPDATE OF n \
         ), \
         classified AS ( \
             SELECT CASE \
                      WHEN a.result_code <> 'ready' THEN a.result_code \
                      WHEN n.run_id IS NULL THEN 'attempt-not-found' \
                      WHEN n.status IN ('success', 'error') THEN 'already-completed' \
                      WHEN n.status <> 'started' THEN 'attempt-not-started' \
                      ELSE 'ready' \
                    END AS result_code, a.tenant_id, a.run_id, a.status \
               FROM authority AS a LEFT JOIN locked_attempt AS n ON true \
         ), \
         completed_attempt AS ( \
             UPDATE node_runs AS n \
                SET status = 'success', output_port = $7, output_json = $8::text::jsonb, \
                    input_json = $9::text::jsonb, preview_head = $10, payload_size = $11, \
                    payload_hash = $12, capture_mode = $13, redacted = $14, ended_at = now() \
               FROM classified AS c \
              WHERE c.result_code = 'ready' \
                AND n.tenant_id = c.tenant_id AND n.run_id = c.run_id \
                AND n.frame_id = 0 AND n.local_node_id = $5 AND n.occurrence = $6 \
             RETURNING n.tenant_id, n.run_id \
         ), \
         checkpointed AS ( \
             UPDATE runs AS r \
                SET state_json = jsonb_set(COALESCE(r.state_json, '{{}}'::jsonb), \
                                           '{{context}}', $15::text::jsonb, true), \
                    updated_at = now() \
               FROM completed_attempt AS n \
              WHERE r.tenant_id = n.tenant_id AND r.run_id = n.run_id \
                AND jsonb_typeof($15::text::jsonb) = 'object' \
             RETURNING r.run_id \
         ), \
         renewed AS ( \
             UPDATE run_queue AS q \
                SET lease_expires_at = now() + ($16::bigint * interval '1 millisecond') \
               FROM checkpointed AS p, classified AS c \
              WHERE q.tenant_id = c.tenant_id AND q.run_id = p.run_id \
             RETURNING q.run_id \
         ) \
         SELECT CASE WHEN q.run_id IS NOT NULL THEN 'completed' ELSE c.result_code END \
                    AS result_code, c.status AS run_status \
           FROM classified AS c \
           LEFT JOIN renewed AS q ON true"
    )
}

/// Complete an error-routed attempt and renew the exact queue lease.
///
/// Params: fence `$1..$4`, local node id, occurrence, captured error output,
/// captured input, error kind/detail, preview, size, hash, capture mode,
/// redacted, and lease TTL milliseconds.
pub fn complete_attempt_error_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         locked_attempt AS MATERIALIZED ( \
             SELECT n.* FROM node_runs AS n, authority AS a \
              WHERE a.result_code = 'ready' \
                AND n.tenant_id = a.tenant_id AND n.run_id = a.run_id \
                AND n.frame_id = 0 AND n.local_node_id = $5 AND n.occurrence = $6 \
              FOR UPDATE OF n \
         ), \
         classified AS ( \
             SELECT CASE \
                      WHEN a.result_code <> 'ready' THEN a.result_code \
                      WHEN n.run_id IS NULL THEN 'attempt-not-found' \
                      WHEN n.status IN ('success', 'error') THEN 'already-completed' \
                      WHEN n.status <> 'started' THEN 'attempt-not-started' \
                      ELSE 'ready' \
                    END AS result_code, a.tenant_id, a.run_id, a.status \
               FROM authority AS a LEFT JOIN locked_attempt AS n ON true \
         ), \
         completed_attempt AS ( \
             UPDATE node_runs AS n \
                SET status = 'error', output_port = 'error', \
                    output_json = $7::text::jsonb, input_json = $8::text::jsonb, \
                    error_kind = $9, error_detail = $10::text::jsonb, \
                    preview_head = $11, payload_size = $12, payload_hash = $13, \
                    capture_mode = $14, redacted = $15, ended_at = now() \
               FROM classified AS c \
              WHERE c.result_code = 'ready' \
                AND n.tenant_id = c.tenant_id AND n.run_id = c.run_id \
                AND n.frame_id = 0 AND n.local_node_id = $5 AND n.occurrence = $6 \
             RETURNING n.tenant_id, n.run_id \
         ), \
         renewed AS ( \
             UPDATE run_queue AS q \
                SET lease_expires_at = now() + ($16::bigint * interval '1 millisecond') \
               FROM completed_attempt AS n, classified AS c \
              WHERE q.tenant_id = c.tenant_id AND q.run_id = n.run_id \
             RETURNING q.run_id \
         ) \
         SELECT CASE WHEN q.run_id IS NOT NULL THEN 'completed' ELSE c.result_code END \
                    AS result_code, c.status AS run_status \
           FROM classified AS c \
           LEFT JOIN renewed AS q ON true"
    )
}

/// Typed mapping for the admissions ledger's named duplicate identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationAdmissionRefusal {
    DuplicateIdentity,
}

impl InvocationAdmissionRefusal {
    pub fn from_constraint(constraint: &str) -> Option<InvocationAdmissionRefusal> {
        (constraint == "invocation_admissions_identity")
            .then_some(InvocationAdmissionRefusal::DuplicateIdentity)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fence_lost_forbids_subsequent_access() {
        assert!(!CallerReleaseResult::FenceLost.permits_access());
        assert!(!TerminalizeResult::FenceLost.permits_access());
        assert!(!ReservedCheckpointResult::FenceLost.permits_access());
        assert!(!CheckpointResult::FenceLost.permits_access());
    }

    #[test]
    fn caller_replay_decodes_the_stored_outcome() {
        let result = CallerReleaseResult::from_parts(
            "already-released",
            "running",
            Some("responded".to_string()),
            Some(json!({"ok": true})),
            Some(200),
            Some("respond".to_string()),
            Some("sha256:out".to_string()),
        );
        assert_eq!(
            result,
            Some(CallerReleaseResult::AlreadyReleased(StoredCallerOutcome {
                kind: "responded".to_string(),
                body: json!({"ok": true}),
                http_status: Some(200),
                release_node_id: Some("respond".to_string()),
                hash: Some("sha256:out".to_string()),
            }))
        );
    }

    #[test]
    fn caller_replay_identity_requires_exact_body_and_hash() {
        let stored = StoredCallerOutcome {
            kind: "responded".to_string(),
            body: json!({"a": 1, "b": 2}),
            http_status: Some(200),
            release_node_id: Some("respond".to_string()),
            hash: Some("sha256:exact".to_string()),
        };
        assert!(stored.exactly_matches(
            "responded",
            &json!({"b": 2, "a": 1}),
            Some(200),
            Some("respond"),
            "sha256:exact",
        ));
        assert!(!stored.exactly_matches(
            "responded",
            &json!({"a": 9, "b": 2}),
            Some(200),
            Some("respond"),
            "sha256:exact",
        ));
        assert!(!stored.exactly_matches(
            "responded",
            &json!({"a": 1, "b": 2}),
            Some(200),
            Some("respond"),
            "sha256:changed",
        ));
    }

    #[test]
    fn duplicate_admission_constraint_is_a_typed_refusal() {
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        assert!(ddl.contains("CONSTRAINT invocation_admissions_identity UNIQUE"));
        assert_eq!(
            InvocationAdmissionRefusal::from_constraint("invocation_admissions_identity"),
            Some(InvocationAdmissionRefusal::DuplicateIdentity)
        );
        assert_eq!(
            InvocationAdmissionRefusal::from_constraint("some_other_constraint"),
            None
        );
    }

    #[test]
    fn transitions_are_queue_joined_and_generation_fenced() {
        for sql in [
            release_caller_sql(),
            reserved_checkpoint_sql(),
            terminalize_sql(),
            complete_sql(),
        ] {
            assert!(sql.contains("locked_queue AS MATERIALIZED"), "{sql}");
            assert!(
                sql.contains("q.lease_owner IS DISTINCT FROM i.lease_owner"),
                "{sql}"
            );
            assert!(
                sql.contains("q.lease_generation IS DISTINCT FROM i.lease_generation"),
                "{sql}"
            );
            assert!(sql.contains("'cross-run-authority'"), "{sql}");
            assert!(sql.contains("'fence-lost'"), "{sql}");
        }
    }

    #[test]
    fn reserved_checkpoint_inserts_only_from_ready_generation_authority() {
        let sql = reserved_checkpoint_sql();
        assert!(sql.contains("INSERT INTO node_runs"), "{sql}");
        assert!(sql.contains("r.execution_bundle_hash"), "{sql}");
        assert!(sql.contains("a.execution_bundle_hash"), "{sql}");
        assert!(sql.contains("FROM authority AS a"), "{sql}");
        assert!(sql.contains("WHERE a.result_code = 'ready'"), "{sql}");
        assert!(
            sql.contains("q.lease_generation IS DISTINCT FROM i.lease_generation"),
            "{sql}"
        );
        assert!(
            sql.find("authority AS").expect("authority CTE")
                < sql.find("recorded AS").expect("record CTE"),
            "{sql}"
        );
    }

    #[test]
    fn terminal_and_checkpoint_transitions_are_atomic_statements() {
        let terminal = terminalize_sql();
        assert!(terminal.contains("trigger_source IN ('http','internal','studio')"));
        assert!(!terminal.contains("attachment_id IS NOT NULL"));
        assert!(terminal.contains("terminalized AS"));
        assert!(terminal.contains("dead_lettered AS"));
        assert!(terminal.contains("INSERT INTO run_dead_letters"));
        assert!(terminal.contains("t.status = 'failed'"));
        assert!(terminal.contains("q.partition_policy = 'blocking'"));
        assert!(terminal.contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
        assert!(
            terminal.find("dead_lettered AS").expect("ledger CTE")
                < terminal.find("dequeued AS").expect("dequeue CTE")
        );
        assert!(terminal.contains("dequeued AS"));

        let complete = complete_sql();
        assert!(complete.contains("completed_attempt AS"));
        assert!(complete.contains("checkpointed AS"));
    }

    #[test]
    fn attempt_completion_updates_existing_intent_atomically() {
        let success = complete_attempt_success_sql();
        assert!(success.contains("UPDATE node_runs AS n"));
        assert!(success.contains("SET status = 'success'"));
        assert!(success.contains("checkpointed AS"));
        assert!(success.contains("renewed AS"));
        assert!(!success.contains("INSERT INTO node_runs"));

        let error = complete_attempt_error_sql();
        assert!(error.contains("SET status = 'error'"));
        assert!(error.contains("output_port = 'error'"));
        assert!(error.contains("renewed AS"));
        assert!(!error.contains("INSERT INTO node_runs"));

        for sql in [success, error] {
            assert!(!sql.contains("cancel_requested"));
            assert!(!sql.contains("caller_outcome_kind ="));
            assert!(!sql.contains("pg_notify"));
        }
    }

    #[test]
    fn context_checkpoint_is_generation_fenced_and_atomic_with_output() {
        let sql = node_context_checkpoint_sql();
        assert!(sql.starts_with("WITH input AS"));
        assert!(sql.contains("q.lease_generation IS DISTINCT FROM i.lease_generation"));
        assert!(sql.contains("INSERT INTO node_runs"));
        assert!(sql.contains("a.execution_bundle_hash"));
        assert!(sql.contains("jsonb_typeof($16::text::jsonb) = 'object'"));
        assert!(sql.contains("SET state_json = jsonb_set"));
        assert!(sql.contains("(SELECT count(*) FROM recorded) = 1"));
        assert!(sql.contains("'{context}', $16::text::jsonb"));
        assert!(sql.contains("$17::bigint * interval '1 millisecond'"));
        assert_eq!(sql.matches("WITH input AS").count(), 1);
    }

    #[test]
    fn from_zero_schema_carries_the_callable_run_spine() {
        let run = include_str!("../../../../deploy/sql/run-state.sql");
        for field in [
            "catalog_id",
            "catalog_version",
            "attachment_id",
            "registration_id",
            "event_source_run_id",
            "event_root_run_id",
            "event_depth",
            "parent_run_id",
            "parent_node_id",
            "parent_occurrence",
            "invoke_depth",
            "waiting_child_run_id",
            "waiting_child_occurrence",
            "wait_generation",
            "caller_outcome_kind",
            "caller_outcome_json",
            "caller_http_status",
            "caller_release_node_id",
            "caller_outcome_hash",
            "caller_released_at",
            "response_deadline_at",
            "run_deadline_at",
            "invocation_context",
            "admission_context_version",
            "platform_revision",
            "terminal_reason",
        ] {
            assert!(run.contains(field), "run-state DDL missing {field}");
        }
        assert!(run.contains("CREATE TABLE wamn_run.invocation_admissions"));
        assert!(run.contains("CREATE TRIGGER runs_event_lineage_immutable"));
        assert!(run.contains("'effect-uncertain'"));

        let queue = include_str!("../../../../deploy/sql/run-queue.sql");
        assert!(queue.contains("lease_generation bigint NOT NULL DEFAULT 0"));
    }

    #[test]
    fn recovery_and_successor_lineage_residue_is_absent() {
        let attempt = include_str!("attempt.rs");
        let transitions = include_str!("transitions.rs")
            .split_once("#[cfg(test)]")
            .map_or(include_str!("transitions.rs"), |(production, _)| production);
        for residue in [
            "RecoveryClass",
            "AttemptStartResult",
            "AttemptDispatchResult",
            "begin_attempt_sql",
            "mark_attempt_dispatched_sql",
            "redispatch",
            "missing-attempt-key",
        ] {
            assert!(!attempt.contains(residue), "attempt API retains {residue}");
            assert!(
                !transitions.contains(residue),
                "transition API retains {residue}"
            );
        }
    }

    #[test]
    fn effect_attempt_schema_has_one_identity_and_no_successor_shape() {
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        let nodes = ddl
            .split_once("CREATE TABLE wamn_run.node_runs (")
            .and_then(|(_, tail)| tail.split_once("\n);"))
            .map(|(table, _)| table)
            .expect("node_runs table");
        let attempts = ddl
            .split_once("CREATE TABLE wamn_run.effect_attempts (")
            .and_then(|(_, tail)| tail.split_once("\n);"))
            .map(|(table, _)| table)
            .expect("effect_attempts table");
        assert!(
            attempts.contains("UNIQUE (tenant_id, run_id, frame_id, local_node_id, occurrence)")
        );
        for frame_fact in [
            "root_plan_hash",
            "current_plan_hash",
            "frame_id",
            "parent_frame_id",
            "call_site_id",
            "local_node_id",
            "source_artifact_hash",
            "requirement_name",
        ] {
            assert!(
                attempts.contains(frame_fact),
                "attempt table lacks frame/effect fact {frame_fact}"
            );
        }
        for residue in [
            "attempt_index",
            "predecessor_attempt_id",
            "legacy_imported",
            "selected_recovery_class",
            "recovery_class",
        ] {
            assert!(
                !attempts.contains(residue),
                "attempt table retains {residue}"
            );
        }
        for residue in [
            "current_effect_attempt_id",
            "selected_recovery_class",
            "recovery_class",
            "attempt_index",
            "attempt_dispatched_at",
        ] {
            assert!(!nodes.contains(residue), "node table retains {residue}");
        }
    }

    #[test]
    fn effect_ledger_and_cdc_lineage_remain_immutable() {
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        for trigger in [
            "CREATE TRIGGER effect_attempts_update_immutable",
            "CREATE TRIGGER effect_attempts_delete_immutable",
            "CREATE TRIGGER effect_attempt_dispatches_update_immutable",
            "CREATE TRIGGER effect_attempt_dispatches_delete_immutable",
            "CREATE TRIGGER effect_attempt_outcomes_update_immutable",
            "CREATE TRIGGER effect_attempt_outcomes_delete_immutable",
        ] {
            assert!(ddl.contains(trigger), "effect ledger lacks {trigger}");
        }
        assert!(ddl.contains("event_source_run_id IS NOT NULL AND event_source_run_id <> ''"));
        assert!(ddl.contains("event_root_run_id IS NOT NULL AND event_root_run_id <> ''"));
        assert!(ddl.contains("event_depth IS NOT NULL"));
        assert!(ddl.contains(
            "CREATE TRIGGER runs_event_lineage_immutable\nBEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth"
        ));
    }
}
