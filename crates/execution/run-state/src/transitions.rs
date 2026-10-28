//! Fenced run-state transitions.
//!
//! Every executor-owned mutation joins `runs` to the queue-owned
//! `(lease_owner, lease_generation)` authority and returns one typed result row.
//! The explicit authority run id is deliberately separate from the target run
//! id: accidentally presenting one run's fence while mutating another is a
//! typed `cross-run-authority` refusal, not an empty update.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RunStatus;

pub(crate) const FENCED_PREFIX: &str = "\
WITH input AS ( \
    SELECT NULLIF(current_setting('app.env_key', true), '')::text AS env_key, \
           $1::text AS target_run_id, $2::text AS authority_run_id, \
           $3::text AS lease_owner, $4::bigint AS lease_generation \
), \
locked_run AS MATERIALIZED ( \
    SELECT r.* FROM runs AS r, input AS i \
     WHERE i.target_run_id = i.authority_run_id \
       AND r.env_key = i.env_key AND r.run_id = i.target_run_id \
     FOR UPDATE OF r \
), \
locked_queue AS MATERIALIZED ( \
    SELECT q.* FROM run_queue AS q \
      JOIN locked_run AS r \
        ON r.env_key = q.env_key AND r.run_id = q.run_id \
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
           r.env_key, r.run_id, r.status, \
           r.caller_outcome_type, r.caller_outcome_json, \
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
    #[serde(rename = "type")]
    pub type_: String,
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
        self.type_ == kind
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
                type_: kind?,
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
/// outcome kind, outcome JSON text, HTTP status, release node id, outcome hash,
/// release instant. The one returned row is `(result_code, run_status, kind,
/// body_text, http_status, release_node_id, hash)`.
///
/// The caller supplies the instant (D2b). The statement never reads the server
/// clock, so a test or a simulator controls `caller_released_at` and
/// `updated_at` directly.
pub fn release_caller_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         classified AS ( \
             SELECT CASE \
                      WHEN a.result_code = 'run-terminal' \
                       AND a.caller_released_at IS NOT NULL THEN 'already-released' \
                      WHEN a.result_code <> 'ready' THEN a.result_code \
                      WHEN a.caller_released_at IS NOT NULL THEN 'already-released' \
                      ELSE 'ready' \
                    END AS result_code, \
                    a.env_key, a.run_id, a.status, \
                    a.caller_outcome_type, a.caller_outcome_json, \
                    a.caller_http_status, a.caller_release_node_id, \
                    a.caller_outcome_hash, a.caller_released_at \
               FROM authority AS a \
         ), \
         released AS ( \
             UPDATE runs AS r \
                SET caller_outcome_type = $5, \
                    caller_outcome_json = $6::text::jsonb, \
                    caller_http_status = $7, \
                    caller_release_node_id = $8, \
                    caller_outcome_hash = $9, \
                    caller_released_at = $10::timestamptz, \
                    updated_at = $10::timestamptz \
               FROM classified AS c \
              WHERE c.result_code = 'ready' \
                AND r.env_key = c.env_key AND r.run_id = c.run_id \
             RETURNING r.status, r.caller_outcome_type, r.caller_outcome_json, \
                       r.caller_http_status, r.caller_release_node_id, \
                       r.caller_outcome_hash \
         ) \
         SELECT CASE WHEN x.status IS NOT NULL THEN 'released' ELSE c.result_code END \
                    AS result_code, \
                COALESCE(x.status, c.status) AS run_status, \
                COALESCE(x.caller_outcome_type, c.caller_outcome_type) AS outcome_type, \
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

/// Store deadline changes under the same run and queue locks as terminalization.
/// Parameters are target run, authority run, owner, generation, and JSON text.
/// A live test must exercise the fence and the write under executor credentials.
pub fn record_deadline_adjustments_sql() -> String {
    format!(
        "{FENCED_PREFIX}, recorded AS ( \
             UPDATE runs AS r SET deadline_adjustments_json = $5::text::jsonb \
               FROM authority AS a WHERE a.result_code = 'ready' \
                AND r.env_key = a.env_key AND r.run_id = a.run_id \
             RETURNING r.run_id \
         ) SELECT EXISTS (SELECT 1 FROM recorded)"
    )
}

/// Take the first durable terminal result and remove its queue row atomically.
///
/// Params: target run id, authority run id, lease owner, lease generation,
/// terminal status, terminal reason, result JSON text, failure kind, terminal
/// instant. Completing a caller-attached run before caller release is a typed
/// refusal.
///
/// The caller supplies the instant (D2b). The statement never reads the server
/// clock, so a test or a simulator controls `updated_at` directly.
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
                    a.env_key, a.run_id, a.status, a.caller_released_at \
               FROM authority AS a \
         ), \
         terminalized AS ( \
             UPDATE runs AS r \
                SET status = $5, terminal_reason = $6, \
                    result_json = $7::text::jsonb, fail_type = $8, \
                    updated_at = $9::timestamptz \
               FROM classified AS c \
              WHERE c.result_code = 'ready' \
                AND r.env_key = c.env_key AND r.run_id = c.run_id \
             RETURNING r.env_key, r.run_id, r.status \
         ), \
         dequeued AS ( \
             DELETE FROM run_queue AS q USING terminalized AS t \
              WHERE q.env_key = t.env_key AND q.run_id = t.run_id \
             RETURNING q.run_id \
         ) \
         SELECT CASE WHEN t.run_id IS NOT NULL THEN 'terminalized' ELSE c.result_code END \
                    AS result_code, \
                COALESCE(t.status, c.status) AS run_status \
           FROM classified AS c LEFT JOIN terminalized AS t ON true \
          WHERE (SELECT count(*) FROM dequeued) >= 0"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fence_lost_forbids_subsequent_access() {
        assert!(!CallerReleaseResult::FenceLost.permits_access());
        assert!(!TerminalizeResult::FenceLost.permits_access());
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
                type_: "responded".to_string(),
                body: json!({"ok": true}),
                http_status: Some(200),
                release_node_id: Some("respond".to_string()),
                hash: Some("sha256:out".to_string()),
            }))
        );
    }

    #[test]
    fn caller_replay_identity_requires_exact_body_node_and_hash() {
        let stored = StoredCallerOutcome {
            type_: "responded".to_string(),
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
            Some("another-respond"),
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
}
