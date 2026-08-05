//! Durable state transitions for engine-reserved `invoke-flow`.
//!
//! Child execution policy is deliberately outside this module. These statements
//! accept an already-resolved immutable callee identity and make only the
//! occurrence-keyed state changes: create or recover one child, park its parent,
//! and release the child while waking that exact parent wait.

use crate::RunStatus;
use crate::transitions::{FENCED_PREFIX, StoredCallerOutcome};
use serde_json::Value;

/// Result of [`create_or_recover_child_sql`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildCreateResult {
    Created {
        child_run_id: String,
        wait_generation: i64,
    },
    Recovered {
        child_run_id: String,
        wait_generation: i64,
    },
    Released {
        child_run_id: String,
        outcome: StoredCallerOutcome,
    },
    OccurrenceConflict,
    ChildIdConflict,
    ParentAlreadyWaiting,
    DefinitionMismatch,
    CalleeRevoked,
    CallerRefused,
    DepthExceeded,
    FanoutExceeded,
    UnsupportedActorMode,
    InvalidInput,
    RunTerminal(RunStatus),
    FenceLost,
    CrossRunAuthority,
    NotFound,
    StateConflict,
}

impl ChildCreateResult {
    /// Decode the single row returned by [`create_or_recover_child_sql`].
    #[expect(
        clippy::too_many_arguments,
        reason = "the decoder mirrors the transition's flat SQL result row"
    )]
    pub fn from_parts(
        code: &str,
        run_status: &str,
        child_run_id: Option<String>,
        wait_generation: Option<i64>,
        kind: Option<String>,
        body: Option<Value>,
        http_status: Option<u16>,
        release_node_id: Option<String>,
        hash: Option<String>,
    ) -> Option<ChildCreateResult> {
        match code {
            "created" => Some(ChildCreateResult::Created {
                child_run_id: child_run_id?,
                wait_generation: wait_generation?,
            }),
            "recovered" => Some(ChildCreateResult::Recovered {
                child_run_id: child_run_id?,
                wait_generation: wait_generation?,
            }),
            "released" => Some(ChildCreateResult::Released {
                child_run_id: child_run_id?,
                outcome: StoredCallerOutcome {
                    kind: kind?,
                    body: body?,
                    http_status,
                    release_node_id,
                    hash,
                },
            }),
            "occurrence-conflict" => Some(ChildCreateResult::OccurrenceConflict),
            "child-id-conflict" => Some(ChildCreateResult::ChildIdConflict),
            "parent-already-waiting" => Some(ChildCreateResult::ParentAlreadyWaiting),
            "definition-mismatch" => Some(ChildCreateResult::DefinitionMismatch),
            "callee-revoked" => Some(ChildCreateResult::CalleeRevoked),
            "caller-refused" => Some(ChildCreateResult::CallerRefused),
            "depth-exceeded" => Some(ChildCreateResult::DepthExceeded),
            "fanout-exceeded" => Some(ChildCreateResult::FanoutExceeded),
            "unsupported-actor-mode" => Some(ChildCreateResult::UnsupportedActorMode),
            "invalid-input" => Some(ChildCreateResult::InvalidInput),
            "run-terminal" => Some(ChildCreateResult::RunTerminal(RunStatus::from_sql(
                run_status,
            )?)),
            "fence-lost" => Some(ChildCreateResult::FenceLost),
            "cross-run-authority" => Some(ChildCreateResult::CrossRunAuthority),
            "not-found" => Some(ChildCreateResult::NotFound),
            "state-conflict" => Some(ChildCreateResult::StateConflict),
            _ => None,
        }
    }

    /// `FenceLost` is absolute: callers must stop without another store access.
    pub fn permits_access(&self) -> bool {
        !matches!(self, ChildCreateResult::FenceLost)
    }
}

/// Result of a generation-fenced pre-release child cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildCancelResult {
    Cancelled { seized_generation: i64 },
    AlreadyTerminal(RunStatus),
    AlreadyReleased,
    LiveAttempt,
    StaleGeneration,
    NotChild,
    NotFound,
    StateConflict,
}

impl ChildCancelResult {
    pub fn from_parts(
        code: &str,
        run_status: &str,
        seized_generation: Option<i64>,
    ) -> Option<Self> {
        match code {
            "cancelled" => Some(Self::Cancelled {
                seized_generation: seized_generation?,
            }),
            "run-terminal" => Some(Self::AlreadyTerminal(RunStatus::from_sql(run_status)?)),
            "already-released" => Some(Self::AlreadyReleased),
            "live-attempt" => Some(Self::LiveAttempt),
            "stale-generation" => Some(Self::StaleGeneration),
            "not-child" => Some(Self::NotChild),
            "not-found" => Some(Self::NotFound),
            "state-conflict" => Some(Self::StateConflict),
            _ => None,
        }
    }
}

/// Seize and cancel an unreleased child before it can execute another attempt.
///
/// Params: child run id, observed queue generation, and persisted cancellation
/// cause. A released child is an irreversible boundary and is never touched.
pub fn cancel_unreleased_child_sql() -> &'static str {
    "\
WITH input AS ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           $1::text AS run_id, $2::bigint AS expected_generation, \
           $3::text AS cancel_kind \
), \
locked_child AS MATERIALIZED ( \
    SELECT r.* FROM runs AS r, input AS i \
     WHERE r.tenant_id = i.tenant_id AND r.run_id = i.run_id \
     FOR UPDATE OF r \
), \
locked_queue AS MATERIALIZED ( \
    SELECT q.* FROM run_queue AS q JOIN locked_child AS r USING (tenant_id, run_id) \
     FOR UPDATE OF q \
), \
classified AS ( \
    SELECT CASE \
      WHEN c.run_id IS NULL THEN 'not-found' \
      WHEN c.parent_run_id IS NULL THEN 'not-child' \
      WHEN c.caller_released_at IS NOT NULL THEN 'already-released' \
      WHEN c.status IN ('completed','failed','cancelled','infrastructure-failure') \
        THEN 'run-terminal' \
      WHEN q.run_id IS NULL OR q.lease_generation <> i.expected_generation \
        THEN 'stale-generation' \
      WHEN EXISTS (SELECT FROM node_runs AS n \
                    WHERE n.tenant_id = c.tenant_id AND n.run_id = c.run_id \
                      AND n.status = 'started' AND n.attempt_deadline_at > now()) \
        THEN 'live-attempt' \
      WHEN i.cancel_kind IS NULL OR i.cancel_kind = '' THEN 'state-conflict' \
      ELSE 'ready' END AS result_code, c.tenant_id, c.run_id, c.status, \
           i.cancel_kind, q.lease_generation \
      FROM input AS i \
      LEFT JOIN locked_child AS c ON true \
      LEFT JOIN locked_queue AS q ON true \
), \
seized AS ( \
    DELETE FROM run_queue AS q USING classified AS c \
     WHERE c.result_code = 'ready' AND q.tenant_id = c.tenant_id AND q.run_id = c.run_id \
       AND q.lease_generation = c.lease_generation \
    RETURNING q.tenant_id, q.run_id, q.lease_generation + 1 AS seized_generation \
), \
cancelled AS ( \
    UPDATE runs AS r SET status = 'cancelled', cancel_kind = c.cancel_kind, \
      terminal_reason = c.cancel_kind, cancel_requested_kind = c.cancel_kind, \
      cancel_requested_at = COALESCE(r.cancel_requested_at, now()), \
      caller_outcome_kind = 'cancelled', \
      caller_outcome_json = jsonb_build_object('error', jsonb_build_object( \
        'code', c.cancel_kind, 'run-id', r.run_id, 'flow-id', r.flow_id, \
        'flow-version', r.flow_version)), \
      caller_http_status = 499, caller_released_at = now(), updated_at = now() \
      FROM classified AS c JOIN seized AS s USING (tenant_id, run_id) \
     WHERE r.tenant_id = c.tenant_id AND r.run_id = c.run_id \
    RETURNING s.seized_generation \
) \
SELECT CASE WHEN x.seized_generation IS NOT NULL THEN 'cancelled' ELSE c.result_code END, \
       c.status, x.seized_generation \
  FROM classified AS c LEFT JOIN cancelled AS x ON true"
}

/// Create or recover one occurrence-keyed child and park the fenced parent.
///
/// Params are the parent fence (`$1..$4`), parent node id and occurrence,
/// proposed child run id, internal attachment id, callee flow id, actor mode,
/// input JSON, platform revision, depth/fanout caps, and child partition
/// key/policy.
///
/// Creation resolves and authorizes against the parent's pinned release and
/// current single-hash activation. Recovery deliberately bypasses both live
/// activation and caller-policy checks, using the child's stored pin. Child
/// insertion, enqueue, parent wait, and lease release are one statement.
pub fn create_or_recover_child_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         resolved AS MATERIALIZED ( \
             SELECT a.attachment_id, a.definition_hash, a.flow_id, f.flow_version, \
                    g.artifact_hash, s.definition_json AS caller_policy, \
                    x.enabled AS activation_enabled, \
                    x.confirmed_definition_hash, \
                    (a.definition_json->>'run-deadline-ms')::bigint AS run_deadline_ms, \
                    (a.definition_json->>'response-deadline-ms')::bigint AS response_deadline_ms \
               FROM locked_run AS p \
               JOIN catalog.release_attachments AS a \
                 ON a.tenant_id = p.tenant_id AND a.catalog_id = p.catalog_id \
                AND a.catalog_version = p.catalog_version \
                AND a.attachment_id = $8 AND a.attachment_kind = 'internal' \
                AND a.flow_id = $9 \
               JOIN catalog.release_sources AS s \
                 ON s.tenant_id = a.tenant_id AND s.catalog_id = a.catalog_id \
                AND s.catalog_version = a.catalog_version AND s.source_id = a.source_id \
                AND s.source_kind = 'caller-policy' \
               JOIN catalog.release_flows AS f \
                 ON f.tenant_id = a.tenant_id AND f.catalog_id = a.catalog_id \
                AND f.catalog_version = a.catalog_version AND f.flow_id = a.flow_id \
               JOIN catalog.flow_artifacts AS g \
                 ON g.tenant_id = f.tenant_id AND g.flow_id = f.flow_id \
                AND g.flow_version = f.flow_version \
               LEFT JOIN catalog.attachment_activation AS x \
                 ON x.tenant_id = a.tenant_id AND x.catalog_id = a.catalog_id \
                AND x.environment = p.environment AND x.attachment_id = a.attachment_id \
         ), \
         existing_child AS MATERIALIZED ( \
             SELECT c.* FROM runs AS c, locked_run AS p \
              WHERE c.tenant_id = p.tenant_id AND c.parent_run_id = p.run_id \
                AND c.parent_node_id = $5 AND c.parent_occurrence = $6 \
              FOR UPDATE OF c \
         ), \
         proposed_child AS MATERIALIZED ( \
             SELECT c.run_id FROM runs AS c, locked_run AS p \
              WHERE c.tenant_id = p.tenant_id AND c.run_id = $7 \
              FOR UPDATE OF c \
         ), \
         existing_child_queue AS MATERIALIZED ( \
             SELECT q.run_id FROM run_queue AS q JOIN existing_child AS c \
               ON q.tenant_id = c.tenant_id AND q.run_id = c.run_id \
              FOR UPDATE OF q \
         ), \
         family_size AS MATERIALIZED ( \
             SELECT count(*)::bigint AS child_count FROM runs AS child, locked_run AS p \
              WHERE child.tenant_id = p.tenant_id \
                AND child.invoke_root_run_id = COALESCE(p.invoke_root_run_id, p.run_id) \
         ), \
         classified AS ( \
             SELECT CASE \
                      WHEN a.result_code <> 'ready' THEN a.result_code \
                      WHEN $5::text IS NULL OR $5::text = '' \
                        OR $6::int IS NULL OR $6::int < 0 \
                        OR $7::text IS NULL OR $7::text = '' \
                        OR $8::text IS NULL OR $8::text = '' \
                        OR $9::text IS NULL OR $9::text = '' \
                        OR $10::text IS NULL OR $11::text::jsonb IS NULL \
                        OR $12::text IS NULL OR $12::text = '' \
                        OR $13::int IS NULL OR $13::int <= 0 \
                        OR $14::bigint IS NULL OR $14::bigint <= 0 \
                        OR $16::text IS NULL OR $16::text NOT IN ('blocking', 'leapfrog') \
                        OR ($15::text IS NULL AND $16::text <> 'blocking') \
                        OR $15::text = '' \
                        THEN 'invalid-input' \
                      WHEN c.run_id IS NULL AND proposed.run_id IS NOT NULL \
                        THEN 'child-id-conflict' \
                      WHEN c.run_id IS NULL AND p.waiting_child_run_id IS NOT NULL \
                        THEN 'parent-already-waiting' \
                      WHEN c.run_id IS NOT NULL AND ( \
                           c.catalog_id IS DISTINCT FROM p.catalog_id \
                        OR c.catalog_version IS DISTINCT FROM p.catalog_version \
                        OR c.environment IS DISTINCT FROM p.environment \
                        OR c.attachment_id IS DISTINCT FROM $8::text \
                        OR c.flow_id IS DISTINCT FROM $9::text \
                        OR c.input_json IS DISTINCT FROM $11::text::jsonb \
                        OR c.platform_revision IS DISTINCT FROM $12::text) \
                        THEN 'occurrence-conflict' \
                      WHEN c.run_id IS NOT NULL AND p.waiting_child_run_id IS NOT NULL \
                       AND (p.waiting_child_run_id IS DISTINCT FROM c.run_id \
                        OR p.waiting_child_occurrence IS DISTINCT FROM $6::int \
                        OR p.wait_generation IS DISTINCT FROM $4::bigint) \
                        THEN 'parent-already-waiting' \
               WHEN c.run_id IS NOT NULL THEN 'ready' \
                      WHEN $10::text <> 'service' THEN 'unsupported-actor-mode' \
                      WHEN p.catalog_id IS NULL OR p.catalog_version IS NULL \
                        OR p.environment IS NULL OR d.attachment_id IS NULL \
                        OR d.run_deadline_ms IS NULL OR d.run_deadline_ms <= 0 \
                        OR d.response_deadline_ms IS NOT NULL \
                           AND (d.response_deadline_ms <= 0 \
                                OR d.response_deadline_ms > d.run_deadline_ms) \
                        THEN 'definition-mismatch' \
                      WHEN d.activation_enabled IS DISTINCT FROM true \
                        OR d.confirmed_definition_hash IS DISTINCT FROM d.definition_hash \
                        THEN 'callee-revoked' \
                      WHEN jsonb_typeof(d.caller_policy->'allowed-callers') \
                           IS DISTINCT FROM 'array' \
                        OR jsonb_array_length(d.caller_policy->'allowed-callers') = 0 \
                        OR NOT (d.caller_policy->'allowed-callers' ? p.flow_id) \
                        THEN 'caller-refused' \
                      WHEN p.invoke_depth + 1 > $13::int THEN 'depth-exceeded' \
                      WHEN fs.child_count >= $14::bigint THEN 'fanout-exceeded' \
                      ELSE 'ready' \
                    END AS result_code, a.tenant_id, a.run_id, a.status, \
                    p.catalog_id, p.catalog_version, p.environment, p.flow_id AS caller_flow_id, \
                    p.invocation_context AS caller_context, p.run_deadline_at AS parent_run_deadline, \
                    p.response_deadline_at AS parent_response_deadline, \
                    p.caller_released_at AS parent_released_at, \
                    p.invoke_depth, COALESCE(p.invoke_root_run_id, p.run_id) AS invoke_root_run_id, \
                    c.run_id AS existing_run_id, c.caller_released_at AS child_released_at, \
                    c.caller_outcome_kind, c.caller_outcome_json, c.caller_http_status, \
                    c.caller_release_node_id, c.caller_outcome_hash, \
                    d.flow_version, d.artifact_hash, d.run_deadline_ms, d.response_deadline_ms \
               FROM authority AS a \
               LEFT JOIN locked_run AS p ON true \
               LEFT JOIN resolved AS d ON true \
               LEFT JOIN existing_child AS c ON true \
               LEFT JOIN proposed_child AS proposed ON true \
               LEFT JOIN family_size AS fs ON true \
         ), \
         inserted_child AS ( \
             INSERT INTO runs \
                    (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment, \
                     attachment_id, status, trigger_source, input_json, invocation_context, \
                     admission_context_version, platform_revision, \
                     parent_run_id, parent_node_id, parent_occurrence, \
                     invoke_depth, invoke_root_run_id, response_deadline_at, run_deadline_at) \
             SELECT c.tenant_id, $7, $9, c.flow_version, c.catalog_id, c.catalog_version, \
                    c.environment, $8, 'dispatched', 'internal', $11::text::jsonb, \
                    jsonb_build_object( \
                      'version', 1, \
                      'principal', jsonb_build_object( \
                        'tenant-id', c.tenant_id, 'environment', c.environment, \
                        'catalog-id', c.catalog_id, 'catalog-version', c.catalog_version, \
                        'run-id', $7, \
                        'flow-id', $9::text, 'flow-version', c.flow_version, \
                        'artifact-digest', c.artifact_hash), \
                      'source', jsonb_build_object( \
                        'actor', jsonb_build_object( \
                          'mode', 'service', \
                          'subject', 'service:' || c.catalog_id || ':' \
                                     || c.environment || ':' || $9::text), \
                        'caller', jsonb_build_object( \
                          'run-id', c.run_id, 'flow-id', c.caller_flow_id, \
                          'actor', COALESCE(c.caller_context->'source'->'actor', 'null'::jsonb), \
                          'lineage', COALESCE(c.caller_context->'source'->'caller', 'null'::jsonb)))), \
                    1, $12, c.run_id, $5, $6, c.invoke_depth + 1, c.invoke_root_run_id, \
                    LEAST( \
                      now() + (COALESCE(c.response_deadline_ms, c.run_deadline_ms) \
                               * interval '1 millisecond'), \
                      COALESCE(CASE WHEN c.parent_released_at IS NULL \
                                    THEN c.parent_response_deadline END, \
                               c.parent_run_deadline, 'infinity'::timestamptz), \
                      now() + (c.run_deadline_ms * interval '1 millisecond')), \
                    now() + (c.run_deadline_ms * interval '1 millisecond') \
               FROM classified AS c \
              WHERE c.result_code = 'ready' AND c.existing_run_id IS NULL \
             RETURNING tenant_id, run_id \
         ), \
         chosen_child AS ( \
             SELECT c.tenant_id, c.existing_run_id AS run_id, false AS inserted \
               FROM classified AS c WHERE c.existing_run_id IS NOT NULL \
             UNION ALL \
             SELECT i.tenant_id, i.run_id, true FROM inserted_child AS i \
         ), \
         inserted_queue AS ( \
             INSERT INTO run_queue \
                    (tenant_id, run_id, partition_key, partition_policy, available_at) \
             SELECT c.tenant_id, c.run_id, $15, $16, now() FROM chosen_child AS c \
              JOIN classified AS x ON x.tenant_id = c.tenant_id \
             WHERE x.child_released_at IS NULL \
             ON CONFLICT (tenant_id, run_id) DO NOTHING \
             RETURNING run_id \
         ), \
         queued_child AS ( \
             SELECT run_id FROM existing_child_queue \
             UNION ALL SELECT run_id FROM inserted_queue \
         ), \
         parked_parent AS ( \
             UPDATE runs AS p \
                SET waiting_child_run_id = c.run_id, waiting_child_occurrence = $6, \
                    wait_generation = $4, updated_at = now() \
               FROM classified AS x JOIN chosen_child AS c ON c.tenant_id = x.tenant_id \
              WHERE x.result_code = 'ready' \
                AND p.tenant_id = x.tenant_id AND p.run_id = x.run_id \
                AND x.child_released_at IS NULL \
                AND (SELECT count(*) FROM queued_child) = 1 \
             RETURNING p.tenant_id, p.run_id \
         ), \
         parked_queue AS ( \
             UPDATE run_queue AS q \
                SET available_at = 'infinity'::timestamptz, \
                    lease_owner = NULL, lease_expires_at = NULL \
               FROM parked_parent AS p \
              WHERE q.tenant_id = p.tenant_id AND q.run_id = p.run_id \
             RETURNING q.run_id \
         ) \
         SELECT CASE \
                  WHEN x.result_code <> 'ready' THEN x.result_code \
                  WHEN x.child_released_at IS NOT NULL THEN 'released' \
                  WHEN pq.run_id IS NULL THEN 'state-conflict' \
                  WHEN c.inserted THEN 'created' ELSE 'recovered' \
                END AS result_code, x.status AS run_status, c.run_id AS child_run_id, \
                CASE WHEN pq.run_id IS NOT NULL THEN $4::bigint END AS wait_generation, \
                x.caller_outcome_kind AS outcome_kind, \
                x.caller_outcome_json::text AS outcome_json, \
                x.caller_http_status AS http_status, \
                x.caller_release_node_id AS release_node_id, \
                x.caller_outcome_hash AS outcome_hash \
           FROM classified AS x \
           LEFT JOIN chosen_child AS c ON true \
           LEFT JOIN parked_queue AS pq ON true"
    )
}

/// Result of [`release_child_sql`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildReleaseResult {
    Released,
    AlreadyReleased(StoredCallerOutcome),
    RunTerminal(RunStatus),
    FenceLost,
    CrossRunAuthority,
    CrossParentAccess,
    ParentNotFound,
    ParentNotWaiting,
    StaleWaitGeneration,
    NotChild,
    NotFound,
    StateConflict,
}

impl ChildReleaseResult {
    /// Decode the result row returned by [`release_child_sql`].
    pub fn from_parts(
        code: &str,
        run_status: &str,
        kind: Option<String>,
        body: Option<Value>,
        http_status: Option<u16>,
        release_node_id: Option<String>,
        hash: Option<String>,
    ) -> Option<ChildReleaseResult> {
        match code {
            "released" => Some(ChildReleaseResult::Released),
            "already-released" => Some(ChildReleaseResult::AlreadyReleased(StoredCallerOutcome {
                kind: kind?,
                body: body?,
                http_status,
                release_node_id,
                hash,
            })),
            "run-terminal" => Some(ChildReleaseResult::RunTerminal(RunStatus::from_sql(
                run_status,
            )?)),
            "fence-lost" => Some(ChildReleaseResult::FenceLost),
            "cross-run-authority" => Some(ChildReleaseResult::CrossRunAuthority),
            "cross-parent-access" => Some(ChildReleaseResult::CrossParentAccess),
            "parent-not-found" => Some(ChildReleaseResult::ParentNotFound),
            "parent-not-waiting" => Some(ChildReleaseResult::ParentNotWaiting),
            "stale-wait-generation" => Some(ChildReleaseResult::StaleWaitGeneration),
            "not-child" => Some(ChildReleaseResult::NotChild),
            "not-found" => Some(ChildReleaseResult::NotFound),
            "state-conflict" => Some(ChildReleaseResult::StateConflict),
            _ => None,
        }
    }

    /// `FenceLost` is absolute: callers must stop without another store access.
    pub fn permits_access(&self) -> bool {
        !matches!(self, ChildReleaseResult::FenceLost)
    }
}

/// Release a child outcome and clear/wake its exact parent wait atomically.
///
/// Params are the child fence (`$1..$4`), stored caller outcome (`$5..$9`),
/// parent run id, parent node id, parent occurrence, and expected wait
/// generation. The relation is checked against both the child's immutable
/// parent tuple and the parent's current wait before either row is changed.
pub fn release_child_sql() -> String {
    format!(
        "{FENCED_PREFIX}, \
         locked_parent AS MATERIALIZED ( \
             SELECT p.* FROM runs AS p, authority AS a \
              WHERE p.tenant_id = a.tenant_id AND p.run_id = $10 \
              FOR UPDATE OF p \
         ), \
         locked_parent_queue AS MATERIALIZED ( \
             SELECT q.* FROM run_queue AS q JOIN locked_parent AS p \
               ON q.tenant_id = p.tenant_id AND q.run_id = p.run_id \
              FOR UPDATE OF q \
         ), \
         classified AS ( \
             SELECT CASE \
                      WHEN a.result_code <> 'ready' THEN a.result_code \
                      WHEN c.parent_run_id IS NULL THEN 'not-child' \
                      WHEN c.parent_run_id IS DISTINCT FROM $10::text \
                        OR c.parent_node_id IS DISTINCT FROM $11::text \
                        OR c.parent_occurrence IS DISTINCT FROM $12::int \
                        THEN 'cross-parent-access' \
                      WHEN a.caller_released_at IS NOT NULL THEN 'already-released' \
                      WHEN p.run_id IS NULL OR pq.run_id IS NULL THEN 'parent-not-found' \
                      WHEN p.waiting_child_run_id IS DISTINCT FROM c.run_id \
                        OR p.waiting_child_occurrence IS DISTINCT FROM $12::int \
                        THEN 'parent-not-waiting' \
                      WHEN p.wait_generation IS DISTINCT FROM $13::bigint \
                        THEN 'stale-wait-generation' \
                      ELSE 'ready' \
                    END AS result_code, a.tenant_id, a.run_id, a.status, \
                    a.caller_outcome_kind, a.caller_outcome_json, \
                    a.caller_http_status, a.caller_release_node_id, \
                    a.caller_outcome_hash \
               FROM authority AS a \
               LEFT JOIN locked_run AS c ON true \
               LEFT JOIN locked_parent AS p ON true \
               LEFT JOIN locked_parent_queue AS pq ON true \
         ), \
         released AS ( \
             UPDATE runs AS c \
                SET caller_outcome_kind = $5, caller_outcome_json = $6::text::jsonb, \
                    caller_http_status = $7, caller_release_node_id = $8, \
                    caller_outcome_hash = $9, caller_released_at = now(), updated_at = now() \
               FROM classified AS x \
              WHERE x.result_code = 'ready' \
                AND c.tenant_id = x.tenant_id AND c.run_id = x.run_id \
             RETURNING c.tenant_id, c.run_id, c.status, c.caller_outcome_kind, \
                       c.caller_outcome_json, c.caller_http_status, \
                       c.caller_release_node_id, c.caller_outcome_hash \
         ), \
         cleared_parent AS ( \
             UPDATE runs AS p \
                SET waiting_child_run_id = NULL, waiting_child_occurrence = NULL, \
                    wait_generation = NULL, updated_at = now() \
               FROM released AS c \
              WHERE p.tenant_id = c.tenant_id AND p.run_id = $10 \
                AND p.waiting_child_run_id = c.run_id \
                AND p.waiting_child_occurrence = $12 \
                AND p.wait_generation = $13 \
             RETURNING p.tenant_id, p.run_id \
         ), \
         woken_parent AS ( \
             UPDATE run_queue AS q SET available_at = now() \
               FROM cleared_parent AS p \
              WHERE q.tenant_id = p.tenant_id AND q.run_id = p.run_id \
             RETURNING q.run_id \
         ) \
         SELECT CASE \
                  WHEN x.result_code <> 'ready' THEN x.result_code \
                  WHEN w.run_id IS NULL THEN 'state-conflict' \
                  ELSE 'released' \
                END AS result_code, COALESCE(r.status, x.status) AS run_status, \
                COALESCE(r.caller_outcome_kind, x.caller_outcome_kind) AS outcome_kind, \
                COALESCE(r.caller_outcome_json, x.caller_outcome_json)::text AS outcome_json, \
                COALESCE(r.caller_http_status, x.caller_http_status) AS http_status, \
                COALESCE(r.caller_release_node_id, x.caller_release_node_id) AS release_node_id, \
                COALESCE(r.caller_outcome_hash, x.caller_outcome_hash) AS outcome_hash \
           FROM classified AS x \
           LEFT JOIN released AS r ON true \
           LEFT JOIN woken_parent AS w ON true"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn child_results_make_fence_loss_absolute() {
        assert!(!ChildCreateResult::FenceLost.permits_access());
        assert!(!ChildReleaseResult::FenceLost.permits_access());
        assert!(ChildCreateResult::OccurrenceConflict.permits_access());
        assert!(ChildReleaseResult::StaleWaitGeneration.permits_access());
    }

    #[test]
    fn child_result_decoders_preserve_identity_and_stored_outcome() {
        assert_eq!(
            ChildCreateResult::from_parts(
                "recovered",
                "running",
                Some("child-1".into()),
                Some(7),
                None,
                None,
                None,
                None,
                None,
            ),
            Some(ChildCreateResult::Recovered {
                child_run_id: "child-1".into(),
                wait_generation: 7,
            })
        );
        assert_eq!(
            ChildCreateResult::from_parts(
                "released",
                "running",
                Some("child-1".into()),
                None,
                Some("responded".into()),
                Some(json!({"recommendation": "approve"})),
                Some(200),
                Some("respond".into()),
                Some("sha256:out".into()),
            ),
            Some(ChildCreateResult::Released {
                child_run_id: "child-1".into(),
                outcome: StoredCallerOutcome {
                    kind: "responded".into(),
                    body: json!({"recommendation": "approve"}),
                    http_status: Some(200),
                    release_node_id: Some("respond".into()),
                    hash: Some("sha256:out".into()),
                },
            })
        );
        assert_eq!(
            ChildReleaseResult::from_parts(
                "already-released",
                "running",
                Some("responded".into()),
                Some(json!({"ok": true})),
                Some(200),
                Some("respond".into()),
                Some("sha256:out".into()),
            ),
            Some(ChildReleaseResult::AlreadyReleased(StoredCallerOutcome {
                kind: "responded".into(),
                body: json!({"ok": true}),
                http_status: Some(200),
                release_node_id: Some("respond".into()),
                hash: Some("sha256:out".into()),
            }))
        );
    }

    #[test]
    fn child_create_is_occurrence_keyed_fenced_and_atomic() {
        let sql = create_or_recover_child_sql();
        assert!(sql.contains("c.parent_node_id = $5 AND c.parent_occurrence = $6"));
        assert!(sql.contains("q.lease_generation IS DISTINCT FROM i.lease_generation"));
        assert!(sql.contains("THEN 'occurrence-conflict'"));
        assert!(sql.contains("inserted_child AS"));
        assert!(sql.contains("parked_parent AS"));
        assert!(sql.contains("parked_queue AS"));
        assert!(sql.contains("catalog.release_sources AS s"));
        assert!(sql.contains("catalog.attachment_activation AS x"));
        assert!(sql.contains("NOT (d.caller_policy->'allowed-callers' ? p.flow_id)"));
        assert!(sql.contains("WHEN c.run_id IS NOT NULL THEN 'ready'"));
        assert!(sql.contains("p.invoke_depth + 1 > $13::int"));
        assert!(sql.contains("fs.child_count >= $14::bigint"));
        assert!(sql.contains("'artifact-digest', c.artifact_hash"));
        assert!(sql.contains("'run-id', $7"));
        assert!(sql.contains("invocation_context, admission_context_version"));
        assert!(sql.contains("c.caller_context->'source'->'actor'"));
        assert!(sql.contains(
            "'subject', 'service:' || c.catalog_id || ':' || c.environment || ':' || $9::text"
        ));
        assert!(sql.contains("x.child_released_at IS NOT NULL THEN 'released'"));
        assert!(
            sql.find("inserted_child AS").expect("child insert")
                < sql.find("parked_parent AS").expect("parent park")
        );
    }

    #[test]
    fn pre_release_child_cancel_seizes_the_observed_generation() {
        let sql = cancel_unreleased_child_sql();
        assert!(sql.contains("c.caller_released_at IS NOT NULL THEN 'already-released'"));
        assert!(sql.contains("q.lease_generation <> i.expected_generation"));
        assert!(sql.contains("n.attempt_deadline_at > now()"));
        assert!(sql.contains("q.lease_generation + 1 AS seized_generation"));
        assert!(sql.contains("DELETE FROM run_queue"));
        assert_eq!(
            ChildCancelResult::from_parts("cancelled", "running", Some(9)),
            Some(ChildCancelResult::Cancelled {
                seized_generation: 9
            })
        );
        assert_eq!(
            ChildCancelResult::from_parts("stale-generation", "running", None),
            Some(ChildCancelResult::StaleGeneration)
        );
    }

    #[test]
    fn child_release_fences_wait_generation_and_wakes_atomically() {
        let sql = release_child_sql();
        assert!(sql.contains("p.wait_generation IS DISTINCT FROM $13::bigint"));
        assert!(sql.contains("THEN 'stale-wait-generation'"));
        assert!(sql.contains("released AS"));
        assert!(sql.contains("cleared_parent AS"));
        assert!(sql.contains("woken_parent AS"));
        assert!(sql.contains(
            "waiting_child_run_id = NULL, waiting_child_occurrence = NULL, \
                    wait_generation = NULL"
        ));
        assert!(
            sql.find("released AS").expect("child release")
                < sql.find("cleared_parent AS").expect("parent clear")
        );
        assert!(
            sql.find("cleared_parent AS").expect("parent clear")
                < sql.find("woken_parent AS").expect("parent wake")
        );
    }
}
