//! Immutable operator dispositions for uncertain effect attempts.
//!
//! This module owns the transport-neutral request vocabulary, fail-closed
//! validation, and parameterized SQL. It does not authenticate a principal:
//! project adapters must construct [`AuthenticatedActor`] only from their
//! authenticated request context. The privileged operator CLI uses the
//! separate platform statement, which derives its service actor from
//! PostgreSQL `SESSION_USER`; no caller-supplied principal is trusted.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An immutable disposition action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispositionAction {
    Park,
    Release,
    Resolve,
}

impl DispositionAction {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Park => "park",
            Self::Release => "release",
            Self::Resolve => "resolve",
        }
    }
}

/// The effective project role supplied by an authenticated application
/// adapter. Platform break-glass deliberately has a separate SQL entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectDispositionRole {
    ProjectDeployer,
    ProjectAdmin,
}

impl ProjectDispositionRole {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::ProjectDeployer => "project-deployer",
            Self::ProjectAdmin => "project-admin",
        }
    }
}

/// A principal already authenticated by the application adapter.
///
/// The constructor checks shape, not identity. A CLI string is not an
/// authentication mechanism and must never be used to construct this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedActor {
    principal: String,
    role: ProjectDispositionRole,
}

impl AuthenticatedActor {
    pub fn new(
        principal: impl Into<String>,
        role: ProjectDispositionRole,
    ) -> Result<Self, InvalidDisposition> {
        let principal = principal.into();
        if principal.is_empty() {
            return Err(InvalidDisposition::new("principal-required"));
        }
        Ok(Self { principal, role })
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub const fn role(&self) -> ProjectDispositionRole {
        self.role
    }
}

/// Evidence classification recorded for every resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolutionBasis {
    ExternalEvidence,
    CounterpartyConfirmation,
    OperatorJudgment,
}

impl ResolutionBasis {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::ExternalEvidence => "external-evidence",
            Self::CounterpartyConfirmation => "counterparty-confirmation",
            Self::OperatorJudgment => "operator-judgment",
        }
    }
}

/// The only failure variants a resolution may assert. Both are non-retrying
/// and feed the engine's existing error-route-or-fail transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolvedFailureKind {
    Terminal,
    InvalidInput,
}

impl ResolvedFailureKind {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::InvalidInput => "invalid-input",
        }
    }
}

/// A complete asserted node outcome. Success has no default payload or port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ResolutionOutcome {
    Succeeded {
        payload: Value,
        port: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
    },
    Failed {
        kind: ResolvedFailureKind,
        /// Existing `ErrorDetail` JSON object (`message`, optional `code` and
        /// `data`); the store rejects a non-object.
        detail: Value,
    },
}

/// Audit data required for an asserted resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ResolutionAudit {
    pub basis: ResolutionBasis,
    pub evidence_ref: String,
}

/// One transport-neutral project operation. The adapter supplies the
/// authenticated actor separately so request bytes cannot select their role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SingleDisposition {
    pub attempt_id: String,
    pub action: DispositionAction,
    pub correlation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<ResolutionAudit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ResolutionOutcome>,
}

/// A validated bulk selector. Every action requires every field except
/// `flow_id`; the exact matched IDs are materialized by the SQL statement
/// before mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BulkSelector {
    pub connection_name: String,
    pub connection_generation: String,
    pub window_start: String,
    pub window_end: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_id: Option<String>,
}

/// One bounded bulk operation. The exact matching attempt ids and their stable
/// ordinals are materialized by the store statement before any append.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BulkDisposition {
    pub selector: BulkSelector,
    pub action: DispositionAction,
    pub correlation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<ResolutionAudit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ResolutionOutcome>,
}

/// A contextual validation error with a stable refusal code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidDisposition {
    code: &'static str,
}

impl InvalidDisposition {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for InvalidDisposition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for InvalidDisposition {}

/// Validate shape and the settled project-role matrix before any store access.
/// The SQL repeats every security-relevant check against pinned durable facts.
pub fn validate_single(
    actor: &AuthenticatedActor,
    request: &SingleDisposition,
) -> Result<(), InvalidDisposition> {
    validate_single_shape(request)?;
    if request.action == DispositionAction::Resolve
        && actor.role != ProjectDispositionRole::ProjectAdmin
    {
        return Err(InvalidDisposition::new("resolve-role-required"));
    }
    validate_action_shape(
        request.action,
        request.audit.as_ref(),
        request.outcome.as_ref(),
    )
}

/// Validate a platform break-glass single request without manufacturing a
/// project principal. Database-session privilege and audit identity are
/// enforced independently by [`platform_break_glass_single_sql`].
pub fn validate_platform_single(request: &SingleDisposition) -> Result<(), InvalidDisposition> {
    validate_single_shape(request)?;
    validate_action_shape(
        request.action,
        request.audit.as_ref(),
        request.outcome.as_ref(),
    )
}

fn validate_single_shape(request: &SingleDisposition) -> Result<(), InvalidDisposition> {
    if request.attempt_id.is_empty() {
        return Err(InvalidDisposition::new("attempt-id-required"));
    }
    if request.correlation_id.is_empty() {
        return Err(InvalidDisposition::new("correlation-id-required"));
    }
    Ok(())
}

/// Validate the mandatory bounded selector and role/outcome matrix before any
/// store access. The SQL independently repeats these checks against immutable
/// attempt facts and materializes the exact selected set.
pub fn validate_bulk(
    actor: &AuthenticatedActor,
    request: &BulkDisposition,
) -> Result<(), InvalidDisposition> {
    validate_bulk_shape(request)?;
    if request.action == DispositionAction::Resolve
        && actor.role != ProjectDispositionRole::ProjectAdmin
    {
        return Err(InvalidDisposition::new("resolve-role-required"));
    }
    validate_action_shape(
        request.action,
        request.audit.as_ref(),
        request.outcome.as_ref(),
    )
}

/// Validate a platform break-glass bulk request without accepting a
/// caller-selected identity. The store derives the actor from `SESSION_USER`.
pub fn validate_platform_bulk(request: &BulkDisposition) -> Result<(), InvalidDisposition> {
    validate_bulk_shape(request)?;
    validate_action_shape(
        request.action,
        request.audit.as_ref(),
        request.outcome.as_ref(),
    )
}

fn validate_bulk_shape(request: &BulkDisposition) -> Result<(), InvalidDisposition> {
    if request.selector.connection_name.is_empty() {
        return Err(InvalidDisposition::new("connection-name-required"));
    }
    if request.selector.connection_generation.is_empty() {
        return Err(InvalidDisposition::new("connection-generation-required"));
    }
    if request.selector.window_start.is_empty() || request.selector.window_end.is_empty() {
        return Err(InvalidDisposition::new("bounded-window-required"));
    }
    if request
        .selector
        .flow_id
        .as_ref()
        .is_some_and(String::is_empty)
    {
        return Err(InvalidDisposition::new("flow-id-empty"));
    }
    if request.correlation_id.is_empty() {
        return Err(InvalidDisposition::new("correlation-id-required"));
    }
    Ok(())
}

fn validate_action_shape(
    action: DispositionAction,
    audit: Option<&ResolutionAudit>,
    outcome: Option<&ResolutionOutcome>,
) -> Result<(), InvalidDisposition> {
    match action {
        DispositionAction::Park | DispositionAction::Release => {
            if audit.is_some() || outcome.is_some() {
                return Err(InvalidDisposition::new("outcome-not-permitted"));
            }
        }
        DispositionAction::Resolve => {
            let audit =
                audit.ok_or_else(|| InvalidDisposition::new("resolution-audit-required"))?;
            if audit.evidence_ref.is_empty() {
                return Err(InvalidDisposition::new("evidence-required"));
            }
            match outcome.ok_or_else(|| InvalidDisposition::new("resolution-outcome-required"))? {
                ResolutionOutcome::Succeeded { port, context, .. } => {
                    if port.is_empty() {
                        return Err(InvalidDisposition::new("success-port-required"));
                    }
                    if context.as_ref().is_some_and(|value| !value.is_object()) {
                        return Err(InvalidDisposition::new("success-context-object-required"));
                    }
                }
                ResolutionOutcome::Failed { detail, .. } => {
                    let Some(object) = detail.as_object() else {
                        return Err(InvalidDisposition::new("failure-detail-object-required"));
                    };
                    if !object.get("message").is_some_and(Value::is_string) {
                        return Err(InvalidDisposition::new("failure-message-required"));
                    }
                    if object
                        .get("code")
                        .is_some_and(|value| !value.is_null() && !value.is_string())
                    {
                        return Err(InvalidDisposition::new("failure-code-invalid"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Fenced automatic park for a lost `never-replay` outcome.
///
/// Params: the normal executor fence `$1..$4`, node id `$5`, occurrence `$6`,
/// and complete checkpoint state JSON `$7`. The statement appends the system
/// disposition, sets the existing queue condition to an indefinite wake, and
/// releases the lease atomically. It never terminalizes or mutates the attempt.
pub fn park_effect_uncertain_sql() -> String {
    "SELECT result_code, run_status \
       FROM park_effect_uncertain( \
           $1::text, $2::text, $3::text, $4::bigint, \
           $5::text, $6::int, $7::text::jsonb)"
        .to_string()
}

/// Read a resolution for the exact effect attempt currently projected by one
/// outstanding node occurrence.
///
/// Params: run id, node id, occurrence. The statement returns no row unless
/// the current immutable attempt is a dispatched, still-incomplete
/// `never-replay` attempt with an appended resolution. In particular, a stale
/// resolution for a predecessor attempt cannot advance the current frontier.
/// The runner performs this read before calling `begin_attempt_sql`, so a
/// released unresolved attempt is classified and parked again without a send.
pub fn select_current_resolution_sql() -> &'static str {
    "SELECT d.resolution_status, d.success_payload::text, d.success_port, \
            d.success_context::text, d.failure_kind, d.failure_detail::text \
       FROM node_runs AS n \
       JOIN effect_attempts AS e \
         ON e.tenant_id = n.tenant_id \
        AND e.attempt_id = n.current_effect_attempt_id \
        AND e.run_id = n.run_id AND e.node_id = n.node_id \
        AND e.occurrence = n.occurrence \
       JOIN effect_attempt_dispatches AS x \
         ON x.tenant_id = e.tenant_id AND x.attempt_id = e.attempt_id \
       JOIN effect_dispositions AS d \
         ON d.tenant_id = e.tenant_id AND d.attempt_id = e.attempt_id \
        AND d.action = 'resolve' \
       LEFT JOIN effect_attempt_outcomes AS o \
         ON o.tenant_id = e.tenant_id AND o.attempt_id = e.attempt_id \
      WHERE n.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
        AND n.run_id = $1 AND n.node_id = $2 AND n.occurrence = $3 \
        AND n.status = 'started' AND e.recovery_class = 'never-replay' \
        AND o.attempt_id IS NULL"
}

/// Project-adapter single-attempt disposition.
///
/// Parameters: attempt UUID text, action, authenticated principal, effective
/// project role, basis, evidence, correlation, resolution status, success
/// payload JSON text, success port, optional context JSON text, failure kind,
/// and failure-detail JSON text. The application adapter must call
/// [`validate_single`] first; the statement independently enforces role,
/// provenance separation, immutable attempt state, and pinned-port validity.
/// The adapter must execute this statement in a `SERIALIZABLE` transaction and
/// retry SQLSTATE `40001` from a fresh transaction; the statement refuses any
/// weaker isolation level before mutation.
pub fn project_single_sql() -> &'static str {
    SINGLE_PROJECT_SQL
}

/// Honestly privileged single-attempt adapter. It has the same parameter order
/// as [`project_single_sql`] except principal and role parameters are ignored:
/// the statement records `SESSION_USER` and the fixed
/// `platform-admin-break-glass` role. `$14` is the mandatory break-glass reason.
/// The same serializable/retry contract as [`project_single_sql`] applies.
pub fn platform_break_glass_single_sql() -> &'static str {
    SINGLE_PLATFORM_SQL
}

/// Project-authorized bounded bulk disposition. All selector fields except the
/// optional flow id are mandatory for every action. The statement materializes
/// and locks the exact eligible attempt set, validates authorization for the
/// whole set, and appends either every row or none.
/// The adapter must use the serializable/retry contract documented on
/// [`project_single_sql`].
///
/// Parameters: connection name/generation, window start/end, optional flow id,
/// action, authenticated principal/role, basis, evidence, correlation,
/// resolution status, success payload/port/context, failure kind/detail, and a
/// final SQL NULL reserved for the platform break-glass reason.
pub fn project_bulk_sql() -> String {
    bulk_sql(false)
}

/// Privileged bounded bulk disposition. It uses the same parameter order as
/// [`project_bulk_sql`], ignores caller principal/role, derives its actor from
/// the privileged host session, and requires `$18` break-glass reason.
pub fn platform_break_glass_bulk_sql() -> String {
    bulk_sql(true)
}

fn bulk_sql(platform: bool) -> String {
    let (actor_fields, actor_guards, separation_guards, break_glass) = if platform {
        (
            "SESSION_USER::text AS principal, \
             'platform-admin-break-glass'::text AS effective_role, \
             $7::text AS ignored_principal, $8::text AS ignored_role",
            "WHEN NOT (SELECT allowed FROM privileged_session) \
               THEN 'platform-privilege-required' \
             WHEN $18::text IS NULL OR $18::text = '' \
               THEN 'break-glass-reason-required'",
            "",
            "$18::text",
        )
    } else {
        (
            "$7::text AS principal, $8::text AS effective_role, \
             NULL::text AS ignored_principal, NULL::text AS ignored_role",
            "WHEN i.principal IS NULL OR i.principal = '' THEN 'principal-required' \
             WHEN i.effective_role IS NULL \
               OR i.effective_role NOT IN ('project-deployer', 'project-admin') \
               THEN 'role-refused' \
             WHEN i.action = 'resolve' AND i.effective_role <> 'project-admin' \
               THEN 'resolve-role-required' \
             WHEN $18::text IS NOT NULL THEN 'break-glass-reason-not-permitted'",
            "WHEN i.action = 'resolve' AND EXISTS ( \
                  SELECT 1 FROM candidates c \
                   WHERE NULLIF(c.verified_author_principal, '') IS NULL \
                      OR NULLIF(c.verified_publisher_principal, '') IS NULL \
             ) THEN 'provenance-unverified' \
             WHEN i.action = 'resolve' AND EXISTS ( \
                  SELECT 1 FROM candidates c \
                   WHERE i.principal = c.verified_author_principal \
                      OR i.principal = c.verified_publisher_principal \
             ) THEN 'self-resolution'",
            "NULL::text",
        )
    };
    BULK_SQL_TEMPLATE
        .replace("__ACTOR_FIELDS__", actor_fields)
        .replace("__ACTOR_GUARDS__", actor_guards)
        .replace("__SEPARATION_GUARDS__", separation_guards)
        .replace("__BREAK_GLASS__", break_glass)
}

/// Read-model projection for every effect attempt in one run. `pending` means
/// no disposition, while the latest append yields `parked`, `released`, or
/// `resolved`; attempts are never updated to manufacture this state.
pub fn select_run_dispositions_sql() -> &'static str {
    "SELECT e.attempt_id::text, e.node_id, e.occurrence, e.connection_name, \
            e.connection_generation, e.verified_author_principal, \
            e.verified_publisher_principal, \
            COALESCE(CASE WHEN latest.action = 'resolve' THEN 'resolved' \
                          WHEN latest.action = 'park' THEN 'parked' \
                          WHEN latest.action = 'release' THEN 'released' END, 'pending') \
                AS disposition_state, \
            latest.resolution_status, latest.principal, latest.effective_role, \
            latest.basis, latest.evidence_ref, latest.correlation_id, \
            latest.break_glass_reason \
       FROM effect_attempts AS e \
       LEFT JOIN LATERAL ( \
           SELECT d.action, d.resolution_status, q.principal, q.effective_role, \
                  q.basis, q.evidence_ref, q.correlation_id, q.break_glass_reason \
             FROM effect_dispositions AS d \
             JOIN effect_disposition_requests AS q \
               ON q.tenant_id = d.tenant_id AND q.request_id = d.request_id \
            WHERE d.tenant_id = e.tenant_id AND d.attempt_id = e.attempt_id \
            ORDER BY d.append_ordinal DESC LIMIT 1 \
       ) AS latest ON true \
      WHERE e.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
        AND e.run_id = $1 \
      ORDER BY e.seq, e.node_id, e.occurrence, e.attempt_index"
}

// Project and platform statements intentionally remain separate constants so a
// future edit cannot accidentally turn a caller-controlled principal into the
// break-glass audit actor. Both materialize the exact named attempt first.
const SINGLE_PROJECT_SQL: &str = "\
WITH requested AS MATERIALIZED ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           CASE WHEN pg_input_is_valid($1::text, 'uuid') \
                THEN $1::text::uuid END AS attempt_id, \
           COALESCE(pg_input_is_valid($1::text, 'uuid'),false) AS attempt_id_valid, \
           $2::text AS action, $3::text AS principal, \
           $4::text AS effective_role, \
           CASE WHEN pg_input_is_valid($9::text, 'jsonb') \
                THEN $9::text::jsonb END AS success_payload, \
           ($9::text IS NOT NULL AND pg_input_is_valid($9::text, 'jsonb')) \
                AS success_payload_valid, \
           $10::text AS success_port, \
           CASE WHEN pg_input_is_valid($11::text, 'jsonb') \
                THEN $11::text::jsonb END AS success_context, \
           ($11::text IS NULL OR pg_input_is_valid($11::text, 'jsonb')) \
                AS success_context_valid, \
           $12::text AS failure_kind, \
           CASE WHEN pg_input_is_valid($13::text, 'jsonb') \
                THEN $13::text::jsonb END AS failure_detail, \
           ($13::text IS NOT NULL AND pg_input_is_valid($13::text, 'jsonb')) \
                AS failure_detail_valid \
), \
target_attempt AS MATERIALIZED ( \
    SELECT e.* FROM requested AS i \
      LEFT JOIN effect_attempts AS e \
        ON e.tenant_id = i.tenant_id AND e.attempt_id = i.attempt_id \
), \
locked_run AS MATERIALIZED ( \
    SELECT r.* FROM target_attempt AS e \
      JOIN runs AS r ON r.tenant_id = e.tenant_id AND r.run_id = e.run_id \
     FOR UPDATE OF r \
), \
locked_queue AS MATERIALIZED ( \
    SELECT q.* FROM locked_run AS r \
      JOIN run_queue AS q ON q.tenant_id = r.tenant_id AND q.run_id = r.run_id \
     FOR UPDATE OF q \
), \
locked_projection AS MATERIALIZED ( \
    SELECT e.*, n.status AS projection_status \
      FROM target_attempt AS e \
      JOIN locked_run AS r \
        ON r.tenant_id = e.tenant_id AND r.run_id = e.run_id \
      JOIN locked_queue AS q \
        ON q.tenant_id = e.tenant_id AND q.run_id = e.run_id \
      JOIN node_runs AS n \
        ON n.tenant_id = e.tenant_id AND n.run_id = e.run_id \
       AND n.node_id = e.node_id AND n.occurrence = e.occurrence \
       AND n.current_effect_attempt_id = e.attempt_id \
     FOR UPDATE OF n \
), \
latest_disposition AS MATERIALIZED ( \
    SELECT latest.action \
      FROM locked_projection AS n \
      LEFT JOIN LATERAL ( \
          SELECT d.action FROM effect_dispositions AS d \
           WHERE d.tenant_id = n.tenant_id AND d.attempt_id = n.attempt_id \
           ORDER BY d.append_ordinal DESC LIMIT 1 \
      ) AS latest ON true \
), \
pinned_port AS MATERIALIZED ( \
    SELECT EXISTS ( \
        SELECT 1 \
          FROM requested AS i, locked_projection AS n, locked_run AS r \
          JOIN catalog.release_flows AS rf \
            ON rf.tenant_id = r.tenant_id AND rf.catalog_id = r.catalog_id \
           AND rf.catalog_version = r.catalog_version AND rf.flow_id = r.flow_id \
           AND rf.flow_version = r.flow_version \
          JOIN catalog.flow_artifacts AS a \
            ON a.tenant_id = rf.tenant_id AND a.flow_id = rf.flow_id \
           AND a.flow_version = rf.flow_version \
          CROSS JOIN LATERAL jsonb_array_elements(a.graph_json -> 'nodes') AS node(value) \
          CROSS JOIN LATERAL jsonb_array_elements(a.interface_bundle_json::jsonb) AS impl(value) \
         WHERE a.artifact_hash = r.invocation_context #>> '{principal,artifact-digest}' \
           AND node.value ->> 'id' = n.node_id \
           AND impl.value #>> '{interface,node-type}' = node.value ->> 'type' \
           AND i.success_port <> 'error' \
           AND EXISTS (SELECT 1 FROM jsonb_array_elements_text( \
                       impl.value #> '{interface,output-ports}') AS port(value) \
                        WHERE port.value = i.success_port) \
    ) AS valid \
), \
classified AS ( \
    SELECT CASE \
             WHEN i.tenant_id IS NULL THEN 'tenant-required' \
             WHEN current_setting('transaction_isolation') <> 'serializable' \
               THEN 'serializable-required' \
             WHEN NOT i.attempt_id_valid THEN 'attempt-id-invalid' \
             WHEN e.attempt_id IS NULL THEN 'attempt-not-found' \
             WHEN r.run_id IS NULL THEN 'run-not-found' \
             WHEN n.attempt_id IS NULL THEN 'attempt-projection-missing' \
             WHEN q.run_id IS NULL THEN 'queue-not-found' \
             WHEN i.principal IS NULL OR i.principal = '' THEN 'principal-required' \
             WHEN i.effective_role IS NULL \
               OR i.effective_role NOT IN ('project-deployer', 'project-admin') \
               THEN 'role-refused' \
             WHEN i.action = 'resolve' AND i.effective_role <> 'project-admin' \
               THEN 'resolve-role-required' \
             WHEN i.action IS NULL \
               OR i.action NOT IN ('park', 'release', 'resolve') THEN 'action-refused' \
             WHEN $7::text IS NULL OR $7::text = '' THEN 'correlation-id-required' \
             WHEN r.status IN ('completed','failed','cancelled','infrastructure-failure') \
               THEN 'run-terminal' \
             WHEN n.projection_status <> 'started' \
               OR n.recovery_class <> 'never-replay' \
               OR NOT EXISTS (SELECT 1 FROM effect_attempt_dispatches AS x \
                               WHERE x.tenant_id = n.tenant_id \
                                 AND x.attempt_id = n.attempt_id) \
               OR EXISTS (SELECT 1 FROM effect_attempt_outcomes AS o \
                           WHERE o.tenant_id = n.tenant_id \
                             AND o.attempt_id = n.attempt_id) \
               THEN 'not-effect-uncertain' \
             WHEN q.lease_owner IS NOT NULL AND q.lease_expires_at > now() THEN 'run-busy' \
             WHEN latest.action = 'resolve' THEN 'already-resolved' \
             WHEN i.action = 'park' AND latest.action = 'park' THEN 'already-parked' \
             WHEN i.action = 'release' AND latest.action IS DISTINCT FROM 'park' \
               THEN 'not-parked' \
             WHEN i.action = 'resolve' \
                    AND (NULLIF(n.verified_author_principal, '') IS NULL \
                         OR NULLIF(n.verified_publisher_principal, '') IS NULL) \
               THEN 'provenance-unverified' \
             WHEN i.action = 'resolve' AND (i.principal = n.verified_author_principal \
                    OR i.principal = n.verified_publisher_principal) THEN 'self-resolution' \
             WHEN i.action <> 'resolve' AND ($5::text IS NOT NULL \
                    OR $6::text IS NOT NULL OR $8::text IS NOT NULL \
                    OR $9::text IS NOT NULL OR $10::text IS NOT NULL \
                    OR $11::text IS NOT NULL OR $12::text IS NOT NULL \
                    OR $13::text IS NOT NULL) THEN 'outcome-not-permitted' \
             WHEN i.action = 'resolve' AND ($5::text IS NULL OR $5::text NOT IN \
                    ('external-evidence', 'counterparty-confirmation', 'operator-judgment') \
                    OR $6::text IS NULL OR $6::text = '' \
                    OR $8::text IS NULL \
                    OR $8::text NOT IN ('succeeded', 'failed')) \
               THEN 'resolution-audit-required' \
             WHEN i.action = 'resolve' AND $8::text = 'succeeded' \
                    AND (NOT p.valid OR NOT i.success_payload_valid \
                         OR i.success_port IS NULL OR i.success_port = '' \
                         OR NOT i.success_context_valid \
                         OR (i.success_context IS NOT NULL \
                             AND jsonb_typeof(i.success_context) <> 'object') \
                         OR $12::text IS NOT NULL OR $13::text IS NOT NULL) \
               THEN 'invalid-success-emission' \
             WHEN i.action = 'resolve' AND $8::text = 'failed' \
                    AND (i.failure_kind IS NULL \
                         OR i.failure_kind NOT IN ('terminal', 'invalid-input') \
                         OR NOT i.failure_detail_valid \
                         OR jsonb_typeof(i.failure_detail) <> 'object' \
                         OR jsonb_typeof(i.failure_detail -> 'message') \
                              IS DISTINCT FROM 'string' \
                         OR ((i.failure_detail ? 'code') \
                             AND i.failure_detail -> 'code' <> 'null'::jsonb \
                             AND jsonb_typeof(i.failure_detail -> 'code') \
                                 IS DISTINCT FROM 'string') \
                         OR $9::text IS NOT NULL OR $10::text IS NOT NULL \
                         OR $11::text IS NOT NULL) \
               THEN 'invalid-failure-outcome' \
             ELSE 'ready' END AS result_code, i.*, e.run_id \
      FROM requested AS i \
      LEFT JOIN target_attempt AS e ON true \
      LEFT JOIN locked_projection AS n ON true \
      LEFT JOIN locked_run AS r ON true \
      LEFT JOIN locked_queue AS q ON true \
      LEFT JOIN latest_disposition AS latest ON true \
      CROSS JOIN pinned_port AS p \
), \
request AS ( \
    INSERT INTO effect_disposition_requests \
           (tenant_id, action, selection_kind, principal, effective_role, basis, \
            evidence_ref, correlation_id) \
    SELECT c.tenant_id, c.action, 'single', c.principal, c.effective_role, \
           CASE WHEN c.action = 'resolve' THEN $5::text END, \
           CASE WHEN c.action = 'resolve' THEN $6::text END, $7::text \
      FROM classified c WHERE c.result_code = 'ready' \
    RETURNING tenant_id, request_id, action \
), \
inserted AS ( \
    INSERT INTO effect_dispositions \
           (tenant_id, request_id, attempt_id, selection_ordinal, action, resolution_status, \
            success_payload, success_port, success_context, failure_kind, failure_detail) \
    SELECT q.tenant_id, q.request_id, c.attempt_id, 0, q.action, \
           CASE WHEN q.action = 'resolve' THEN $8::text END, \
           CASE WHEN $8::text = 'succeeded' THEN c.success_payload END, \
           CASE WHEN $8::text = 'succeeded' THEN c.success_port END, \
           CASE WHEN $8::text = 'succeeded' THEN c.success_context END, \
           CASE WHEN $8::text = 'failed' THEN c.failure_kind END, \
           CASE WHEN $8::text = 'failed' THEN c.failure_detail END \
      FROM request q CROSS JOIN classified c \
    RETURNING tenant_id, request_id, attempt_id, action \
), \
queued AS ( \
    UPDATE run_queue q SET \
           available_at = CASE WHEN i.action = 'park' THEN 'infinity'::timestamptz ELSE now() END, \
           lease_owner = NULL, lease_expires_at = NULL \
      FROM inserted i JOIN locked_projection n ON n.attempt_id = i.attempt_id \
     WHERE q.tenant_id = i.tenant_id AND q.run_id = n.run_id \
    RETURNING q.run_id \
) \
SELECT CASE WHEN i.attempt_id IS NOT NULL THEN 'applied' ELSE c.result_code END AS result_code, \
       i.request_id::text \
  FROM classified c LEFT JOIN inserted i ON true \
 WHERE (SELECT count(*) FROM queued) >= 0";

const SINGLE_PLATFORM_SQL: &str = "\
WITH requested AS MATERIALIZED ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           CASE WHEN pg_input_is_valid($1::text, 'uuid') \
                THEN $1::text::uuid END AS attempt_id, \
           COALESCE(pg_input_is_valid($1::text, 'uuid'),false) AS attempt_id_valid, \
           $2::text AS action, \
           SESSION_USER::text AS principal, \
           'platform-admin-break-glass'::text AS effective_role, \
           $3::text AS ignored_principal, $4::text AS ignored_role, \
           CASE WHEN pg_input_is_valid($9::text, 'jsonb') \
                THEN $9::text::jsonb END AS success_payload, \
           ($9::text IS NOT NULL AND pg_input_is_valid($9::text, 'jsonb')) \
                AS success_payload_valid, \
           $10::text AS success_port, \
           CASE WHEN pg_input_is_valid($11::text, 'jsonb') \
                THEN $11::text::jsonb END AS success_context, \
           ($11::text IS NULL OR pg_input_is_valid($11::text, 'jsonb')) \
                AS success_context_valid, \
           $12::text AS failure_kind, \
           CASE WHEN pg_input_is_valid($13::text, 'jsonb') \
                THEN $13::text::jsonb END AS failure_detail, \
           ($13::text IS NOT NULL AND pg_input_is_valid($13::text, 'jsonb')) \
                AS failure_detail_valid \
), \
privileged_session AS MATERIALIZED ( \
    SELECT COALESCE((SELECT rolsuper FROM pg_catalog.pg_roles \
                      WHERE rolname=SESSION_USER),false) AS allowed \
), \
target_attempt AS MATERIALIZED ( \
    SELECT e.* FROM requested AS i \
      LEFT JOIN effect_attempts AS e \
        ON e.tenant_id = i.tenant_id AND e.attempt_id = i.attempt_id \
), \
locked_run AS MATERIALIZED ( \
    SELECT r.* FROM target_attempt AS e \
      JOIN runs AS r ON r.tenant_id = e.tenant_id AND r.run_id = e.run_id \
     FOR UPDATE OF r \
), \
locked_queue AS MATERIALIZED ( \
    SELECT q.* FROM locked_run AS r \
      JOIN run_queue AS q ON q.tenant_id = r.tenant_id AND q.run_id = r.run_id \
     FOR UPDATE OF q \
), \
locked_projection AS MATERIALIZED ( \
    SELECT e.*, n.status AS projection_status \
      FROM target_attempt AS e \
      JOIN locked_run AS r \
        ON r.tenant_id = e.tenant_id AND r.run_id = e.run_id \
      JOIN locked_queue AS q \
        ON q.tenant_id = e.tenant_id AND q.run_id = e.run_id \
      JOIN node_runs AS n \
        ON n.tenant_id = e.tenant_id AND n.run_id = e.run_id \
       AND n.node_id = e.node_id AND n.occurrence = e.occurrence \
       AND n.current_effect_attempt_id = e.attempt_id \
     FOR UPDATE OF n \
), \
latest_disposition AS MATERIALIZED ( \
    SELECT latest.action \
      FROM locked_projection AS n \
      LEFT JOIN LATERAL ( \
          SELECT d.action FROM effect_dispositions AS d \
           WHERE d.tenant_id = n.tenant_id AND d.attempt_id = n.attempt_id \
           ORDER BY d.append_ordinal DESC LIMIT 1 \
      ) AS latest ON true \
), \
pinned_port AS MATERIALIZED ( \
    SELECT EXISTS ( \
        SELECT 1 \
          FROM requested AS i, locked_projection AS n, locked_run AS r \
          JOIN catalog.release_flows AS rf \
            ON rf.tenant_id = r.tenant_id AND rf.catalog_id = r.catalog_id \
           AND rf.catalog_version = r.catalog_version AND rf.flow_id = r.flow_id \
           AND rf.flow_version = r.flow_version \
          JOIN catalog.flow_artifacts AS a \
            ON a.tenant_id = rf.tenant_id AND a.flow_id = rf.flow_id \
           AND a.flow_version = rf.flow_version \
          CROSS JOIN LATERAL jsonb_array_elements(a.graph_json -> 'nodes') AS node(value) \
          CROSS JOIN LATERAL jsonb_array_elements(a.interface_bundle_json::jsonb) AS impl(value) \
         WHERE a.artifact_hash = r.invocation_context #>> '{principal,artifact-digest}' \
           AND node.value ->> 'id' = n.node_id \
           AND impl.value #>> '{interface,node-type}' = node.value ->> 'type' \
           AND i.success_port <> 'error' \
           AND EXISTS (SELECT 1 FROM jsonb_array_elements_text( \
                       impl.value #> '{interface,output-ports}') AS port(value) \
                        WHERE port.value = i.success_port) \
    ) AS valid \
), \
classified AS ( \
    SELECT CASE \
             WHEN i.tenant_id IS NULL THEN 'tenant-required' \
             WHEN NOT s.allowed THEN 'platform-privilege-required' \
             WHEN current_setting('transaction_isolation') <> 'serializable' \
               THEN 'serializable-required' \
             WHEN NOT i.attempt_id_valid THEN 'attempt-id-invalid' \
             WHEN e.attempt_id IS NULL THEN 'attempt-not-found' \
             WHEN r.run_id IS NULL THEN 'run-not-found' \
             WHEN n.attempt_id IS NULL THEN 'attempt-projection-missing' \
             WHEN q.run_id IS NULL THEN 'queue-not-found' \
             WHEN i.action IS NULL \
               OR i.action NOT IN ('park', 'release', 'resolve') THEN 'action-refused' \
             WHEN $14::text IS NULL OR $14::text = '' THEN 'break-glass-reason-required' \
             WHEN $7::text IS NULL OR $7::text = '' THEN 'correlation-id-required' \
             WHEN r.status IN ('completed','failed','cancelled','infrastructure-failure') \
               THEN 'run-terminal' \
             WHEN n.projection_status <> 'started' \
               OR n.recovery_class <> 'never-replay' \
               OR NOT EXISTS (SELECT 1 FROM effect_attempt_dispatches AS x \
                               WHERE x.tenant_id = n.tenant_id \
                                 AND x.attempt_id = n.attempt_id) \
               OR EXISTS (SELECT 1 FROM effect_attempt_outcomes AS o \
                           WHERE o.tenant_id = n.tenant_id \
                             AND o.attempt_id = n.attempt_id) \
               THEN 'not-effect-uncertain' \
             WHEN q.lease_owner IS NOT NULL AND q.lease_expires_at > now() THEN 'run-busy' \
             WHEN latest.action = 'resolve' THEN 'already-resolved' \
             WHEN i.action = 'park' AND latest.action = 'park' THEN 'already-parked' \
             WHEN i.action = 'release' AND latest.action IS DISTINCT FROM 'park' \
               THEN 'not-parked' \
             WHEN i.action <> 'resolve' AND ($5::text IS NOT NULL \
                    OR $6::text IS NOT NULL OR $8::text IS NOT NULL \
                    OR $9::text IS NOT NULL OR $10::text IS NOT NULL \
                    OR $11::text IS NOT NULL OR $12::text IS NOT NULL \
                    OR $13::text IS NOT NULL) THEN 'outcome-not-permitted' \
             WHEN i.action = 'resolve' AND ($5::text IS NULL OR $5::text NOT IN \
                    ('external-evidence', 'counterparty-confirmation', 'operator-judgment') \
                    OR $6::text IS NULL OR $6::text = '' \
                    OR $8::text IS NULL \
                    OR $8::text NOT IN ('succeeded', 'failed')) \
               THEN 'resolution-audit-required' \
             WHEN i.action = 'resolve' AND $8::text = 'succeeded' \
                    AND (NOT p.valid OR NOT i.success_payload_valid \
                         OR i.success_port IS NULL OR i.success_port = '' \
                         OR NOT i.success_context_valid \
                         OR (i.success_context IS NOT NULL \
                             AND jsonb_typeof(i.success_context) <> 'object') \
                         OR $12::text IS NOT NULL OR $13::text IS NOT NULL) \
               THEN 'invalid-success-emission' \
             WHEN i.action = 'resolve' AND $8::text = 'failed' \
                    AND (i.failure_kind IS NULL \
                         OR i.failure_kind NOT IN ('terminal', 'invalid-input') \
                         OR NOT i.failure_detail_valid \
                         OR jsonb_typeof(i.failure_detail) <> 'object' \
                         OR jsonb_typeof(i.failure_detail -> 'message') \
                              IS DISTINCT FROM 'string' \
                         OR ((i.failure_detail ? 'code') \
                             AND i.failure_detail -> 'code' <> 'null'::jsonb \
                             AND jsonb_typeof(i.failure_detail -> 'code') \
                                 IS DISTINCT FROM 'string') \
                         OR $9::text IS NOT NULL OR $10::text IS NOT NULL \
                         OR $11::text IS NOT NULL) \
               THEN 'invalid-failure-outcome' \
             ELSE 'ready' END AS result_code, i.*, e.run_id \
      FROM requested AS i \
      LEFT JOIN target_attempt AS e ON true \
      LEFT JOIN locked_projection AS n ON true \
      LEFT JOIN locked_run AS r ON true \
      LEFT JOIN locked_queue AS q ON true \
      LEFT JOIN latest_disposition AS latest ON true \
      CROSS JOIN pinned_port AS p CROSS JOIN privileged_session AS s \
), \
request AS ( \
    INSERT INTO effect_disposition_requests \
           (tenant_id, action, selection_kind, principal, effective_role, basis, \
            evidence_ref, correlation_id, break_glass_reason) \
    SELECT c.tenant_id, c.action, 'single', c.principal, c.effective_role, \
           CASE WHEN c.action = 'resolve' THEN $5::text END, \
           CASE WHEN c.action = 'resolve' THEN $6::text END, $7::text, $14::text \
      FROM classified c WHERE c.result_code = 'ready' \
    RETURNING tenant_id, request_id, action \
), \
inserted AS ( \
    INSERT INTO effect_dispositions \
           (tenant_id, request_id, attempt_id, selection_ordinal, action, resolution_status, \
            success_payload, success_port, success_context, failure_kind, failure_detail) \
    SELECT q.tenant_id, q.request_id, c.attempt_id, 0, q.action, \
           CASE WHEN q.action = 'resolve' THEN $8::text END, \
           CASE WHEN $8::text = 'succeeded' THEN c.success_payload END, \
           CASE WHEN $8::text = 'succeeded' THEN c.success_port END, \
           CASE WHEN $8::text = 'succeeded' THEN c.success_context END, \
           CASE WHEN $8::text = 'failed' THEN c.failure_kind END, \
           CASE WHEN $8::text = 'failed' THEN c.failure_detail END \
      FROM request q CROSS JOIN classified c \
    RETURNING tenant_id, request_id, attempt_id, action \
), \
queued AS ( \
    UPDATE run_queue q SET \
           available_at = CASE WHEN i.action = 'park' THEN 'infinity'::timestamptz ELSE now() END, \
           lease_owner = NULL, lease_expires_at = NULL \
      FROM inserted i JOIN locked_projection n ON n.attempt_id = i.attempt_id \
     WHERE q.tenant_id = i.tenant_id AND q.run_id = n.run_id \
    RETURNING q.run_id \
) \
SELECT CASE WHEN i.attempt_id IS NOT NULL THEN 'applied' ELSE c.result_code END AS result_code, \
       i.request_id::text \
  FROM classified c LEFT JOIN inserted i ON true \
 WHERE (SELECT count(*) FROM queued) >= 0";

const BULK_SQL_TEMPLATE: &str = "\
WITH input AS MATERIALIZED ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           NULLIF($1::text, '') AS connection_name, \
           NULLIF($2::text, '') AS connection_generation, \
           $3::text AS window_start_raw, $4::text AS window_end_raw, \
           NULLIF($5::text, '') AS flow_id, $6::text AS action, \
           __ACTOR_FIELDS__, \
           $9::text AS basis, $10::text AS evidence_ref, \
           $11::text AS correlation_id, $12::text AS resolution_status, \
           CASE WHEN pg_input_is_valid($13::text, 'jsonb') \
                THEN $13::text::jsonb END AS success_payload, \
           ($13::text IS NOT NULL AND pg_input_is_valid($13::text, 'jsonb')) \
                AS success_payload_valid, \
           $14::text AS success_port, \
           CASE WHEN pg_input_is_valid($15::text, 'jsonb') \
                THEN $15::text::jsonb END AS success_context, \
           ($15::text IS NULL OR pg_input_is_valid($15::text, 'jsonb')) \
                AS success_context_valid, \
           $16::text AS failure_kind, \
           CASE WHEN pg_input_is_valid($17::text, 'jsonb') \
                THEN $17::text::jsonb END AS failure_detail, \
           ($17::text IS NOT NULL AND pg_input_is_valid($17::text, 'jsonb')) \
                AS failure_detail_valid, \
           CASE WHEN pg_input_is_valid($3::text, 'timestamp with time zone') \
                THEN $3::text::timestamptz END AS window_start, \
           CASE WHEN pg_input_is_valid($4::text, 'timestamp with time zone') \
                THEN $4::text::timestamptz END AS window_end, \
           pg_input_is_valid($3::text, 'timestamp with time zone') \
                AS window_start_valid, \
           pg_input_is_valid($4::text, 'timestamp with time zone') \
                AS window_end_valid \
), \
privileged_session AS MATERIALIZED ( \
    SELECT COALESCE((SELECT rolsuper FROM pg_catalog.pg_roles \
                      WHERE rolname=SESSION_USER),false) AS allowed \
), \
bounded_attempts AS MATERIALIZED ( \
    SELECT e.* FROM effect_attempts e, input i \
     WHERE e.tenant_id = i.tenant_id \
       AND e.connection_name = i.connection_name \
       AND e.connection_generation = i.connection_generation \
       AND e.attempt_started_at >= i.window_start \
       AND e.attempt_started_at < i.window_end \
), \
locked_runs AS MATERIALIZED ( \
    SELECT r.* FROM runs r \
      JOIN (SELECT tenant_id, run_id FROM bounded_attempts \
             GROUP BY tenant_id, run_id) ids \
        ON ids.tenant_id = r.tenant_id AND ids.run_id = r.run_id \
     ORDER BY r.tenant_id, r.run_id \
     FOR UPDATE OF r \
), \
locked_queues AS MATERIALIZED ( \
    SELECT q.* FROM run_queue q \
      JOIN locked_runs r \
        ON r.tenant_id = q.tenant_id AND r.run_id = q.run_id \
     ORDER BY q.tenant_id, q.run_id \
     FOR UPDATE OF q \
), \
locked_projections AS MATERIALIZED ( \
    SELECT e.*, n.status AS projection_status, r.status AS run_status \
      FROM bounded_attempts e \
      JOIN locked_runs r \
        ON r.tenant_id = e.tenant_id AND r.run_id = e.run_id \
      JOIN locked_queues q \
        ON q.tenant_id = e.tenant_id AND q.run_id = e.run_id \
      JOIN input i ON true \
      JOIN node_runs n \
        ON n.tenant_id = e.tenant_id AND n.run_id = e.run_id \
       AND n.node_id = e.node_id AND n.occurrence = e.occurrence \
       AND n.current_effect_attempt_id = e.attempt_id \
     WHERE i.flow_id IS NULL OR r.flow_id = i.flow_id \
     ORDER BY n.tenant_id, n.run_id, n.node_id, n.occurrence \
     FOR UPDATE OF n \
), \
candidates AS MATERIALIZED ( \
    SELECT n.*, \
           (row_number() OVER (ORDER BY n.attempt_started_at, n.attempt_id) - 1)::int \
                AS selection_ordinal, \
           q.run_id AS queue_run_id, q.lease_owner, q.lease_expires_at, \
           latest.action AS latest_action \
      FROM locked_projections n \
      LEFT JOIN locked_queues q \
        ON q.tenant_id = n.tenant_id AND q.run_id = n.run_id \
      LEFT JOIN LATERAL ( \
          SELECT d.action FROM effect_dispositions d \
           WHERE d.tenant_id = n.tenant_id AND d.attempt_id = n.attempt_id \
           ORDER BY d.append_ordinal DESC LIMIT 1 \
      ) latest ON true \
     WHERE n.projection_status = 'started' \
       AND n.recovery_class = 'never-replay' \
       AND EXISTS (SELECT 1 FROM effect_attempt_dispatches x \
                    WHERE x.tenant_id = n.tenant_id \
                      AND x.attempt_id = n.attempt_id) \
       AND NOT EXISTS (SELECT 1 FROM effect_attempt_outcomes o \
                        WHERE o.tenant_id = n.tenant_id \
                          AND o.attempt_id = n.attempt_id) \
), \
pinned_ports AS MATERIALIZED ( \
    SELECT c.attempt_id, CASE WHEN i.resolution_status <> 'succeeded' THEN true \
           ELSE EXISTS ( \
               SELECT 1 FROM locked_runs r \
                 JOIN catalog.release_flows rf \
                   ON rf.tenant_id = r.tenant_id \
                  AND rf.catalog_id = r.catalog_id \
                  AND rf.catalog_version = r.catalog_version \
                  AND rf.flow_id = r.flow_id AND rf.flow_version = r.flow_version \
                 JOIN catalog.flow_artifacts a \
                   ON a.tenant_id = rf.tenant_id AND a.flow_id = rf.flow_id \
                  AND a.flow_version = rf.flow_version \
                 CROSS JOIN LATERAL jsonb_array_elements(a.graph_json -> 'nodes') node(value) \
                 CROSS JOIN LATERAL \
                    jsonb_array_elements(a.interface_bundle_json::jsonb) impl(value) \
                WHERE r.tenant_id = c.tenant_id AND r.run_id = c.run_id \
                  AND a.artifact_hash = \
                      r.invocation_context #>> '{principal,artifact-digest}' \
                  AND node.value ->> 'id' = c.node_id \
                  AND impl.value #>> '{interface,node-type}' = node.value ->> 'type' \
                  AND i.success_port <> 'error' \
                  AND EXISTS (SELECT 1 FROM jsonb_array_elements_text( \
                              impl.value #> '{interface,output-ports}') port(value) \
                               WHERE port.value = i.success_port) \
           ) END AS valid \
      FROM candidates c CROSS JOIN input i \
), \
authorized AS MATERIALIZED ( \
    SELECT CASE \
             WHEN i.tenant_id IS NULL THEN 'tenant-required' \
             WHEN i.connection_name IS NULL THEN 'connection-name-required' \
             WHEN i.connection_generation IS NULL \
               THEN 'connection-generation-required' \
             WHEN NOT i.window_start_valid OR NOT i.window_end_valid \
               OR i.window_start IS NULL OR i.window_end IS NULL \
               OR NOT isfinite(i.window_start) OR NOT isfinite(i.window_end) \
               OR i.window_start >= i.window_end THEN 'bounded-window-required' \
             WHEN i.action IS NULL \
               OR i.action NOT IN ('park', 'release', 'resolve') THEN 'action-refused' \
             __ACTOR_GUARDS__ \
             WHEN current_setting('transaction_isolation') <> 'serializable' \
               THEN 'serializable-required' \
             WHEN i.correlation_id IS NULL OR i.correlation_id = '' \
               THEN 'correlation-id-required' \
             WHEN NOT EXISTS (SELECT 1 FROM candidates) THEN 'selection-empty' \
             WHEN EXISTS (SELECT 1 FROM candidates c WHERE c.queue_run_id IS NULL) \
               THEN 'queue-not-found' \
             WHEN EXISTS (SELECT 1 FROM candidates c \
                           WHERE c.run_status IN \
                             ('completed','failed','cancelled','infrastructure-failure')) \
               THEN 'run-terminal' \
             WHEN EXISTS (SELECT 1 FROM candidates c \
                           WHERE c.lease_owner IS NOT NULL \
                             AND c.lease_expires_at > now()) THEN 'run-busy' \
             WHEN i.action = 'resolve' AND EXISTS ( \
                  SELECT 1 FROM candidates c WHERE c.latest_action = 'resolve' \
             ) THEN 'already-resolved' \
             WHEN i.action = 'park' AND EXISTS ( \
                  SELECT 1 FROM candidates c WHERE c.latest_action = 'park' \
             ) THEN 'already-parked' \
             WHEN i.action = 'release' AND EXISTS ( \
                  SELECT 1 FROM candidates c \
                   WHERE c.latest_action IS DISTINCT FROM 'park' \
             ) THEN 'not-parked' \
             __SEPARATION_GUARDS__ \
             WHEN i.action <> 'resolve' AND (i.basis IS NOT NULL \
                    OR i.evidence_ref IS NOT NULL OR i.resolution_status IS NOT NULL \
                    OR $13::text IS NOT NULL OR i.success_port IS NOT NULL \
                    OR $15::text IS NOT NULL OR i.failure_kind IS NOT NULL \
                    OR $17::text IS NOT NULL) THEN 'outcome-not-permitted' \
             WHEN i.action = 'resolve' AND (i.basis IS NULL OR i.basis NOT IN \
                    ('external-evidence', 'counterparty-confirmation', \
                     'operator-judgment') \
                    OR i.evidence_ref IS NULL OR i.evidence_ref = '' \
                    OR i.resolution_status IS NULL \
                    OR i.resolution_status NOT IN ('succeeded', 'failed')) \
               THEN 'resolution-audit-required' \
             WHEN i.action = 'resolve' AND i.resolution_status = 'succeeded' \
                    AND (NOT i.success_payload_valid \
                         OR i.success_port IS NULL OR i.success_port = '' \
                         OR NOT i.success_context_valid \
                         OR (i.success_context IS NOT NULL \
                             AND jsonb_typeof(i.success_context) <> 'object') \
                         OR i.failure_kind IS NOT NULL OR $17::text IS NOT NULL \
                         OR EXISTS (SELECT 1 FROM pinned_ports p WHERE NOT p.valid)) \
               THEN 'invalid-success-emission' \
             WHEN i.action = 'resolve' AND i.resolution_status = 'failed' \
                    AND (i.failure_kind IS NULL \
                         OR i.failure_kind NOT IN ('terminal', 'invalid-input') \
                         OR NOT i.failure_detail_valid \
                         OR jsonb_typeof(i.failure_detail) <> 'object' \
                         OR jsonb_typeof(i.failure_detail -> 'message') \
                              IS DISTINCT FROM 'string' \
                         OR (i.failure_detail ? 'code' \
                             AND i.failure_detail -> 'code' <> 'null'::jsonb \
                             AND jsonb_typeof(i.failure_detail -> 'code') \
                                 IS DISTINCT FROM 'string') \
                         OR $13::text IS NOT NULL OR i.success_port IS NOT NULL \
                         OR $15::text IS NOT NULL) \
               THEN 'invalid-failure-outcome' \
             ELSE 'ready' END AS result_code \
      FROM input i \
), \
request AS ( \
    INSERT INTO effect_disposition_requests \
           (tenant_id,action,selection_kind,principal,effective_role,basis,evidence_ref, \
            correlation_id,break_glass_reason,connection_name,connection_generation, \
            flow_id,window_start,window_end) \
    SELECT i.tenant_id,i.action,'bulk',i.principal,i.effective_role, \
           CASE WHEN i.action='resolve' THEN i.basis END, \
           CASE WHEN i.action='resolve' THEN i.evidence_ref END, \
           i.correlation_id,__BREAK_GLASS__,i.connection_name,i.connection_generation, \
           i.flow_id,i.window_start,i.window_end \
      FROM input i CROSS JOIN authorized a WHERE a.result_code='ready' \
    RETURNING tenant_id,request_id,action \
), \
inserted AS ( \
    INSERT INTO effect_dispositions \
           (tenant_id,request_id,attempt_id,selection_ordinal,action,resolution_status, \
            success_payload,success_port,success_context,failure_kind,failure_detail) \
    SELECT q.tenant_id,q.request_id,c.attempt_id,c.selection_ordinal,q.action, \
           CASE WHEN q.action='resolve' THEN i.resolution_status END, \
           CASE WHEN i.resolution_status='succeeded' THEN i.success_payload END, \
           CASE WHEN i.resolution_status='succeeded' THEN i.success_port END, \
           CASE WHEN i.resolution_status='succeeded' THEN i.success_context END, \
           CASE WHEN i.resolution_status='failed' THEN i.failure_kind END, \
           CASE WHEN i.resolution_status='failed' THEN i.failure_detail END \
      FROM request q CROSS JOIN candidates c CROSS JOIN input i \
     ORDER BY c.selection_ordinal \
    RETURNING tenant_id,request_id,attempt_id,action \
), \
target_runs AS MATERIALIZED ( \
    SELECT tenant_id,run_id FROM candidates GROUP BY tenant_id,run_id \
), \
queued AS ( \
    UPDATE run_queue q SET \
           available_at = CASE WHEN r.action='park' \
                               THEN 'infinity'::timestamptz ELSE now() END, \
           lease_owner=NULL,lease_expires_at=NULL \
      FROM request r CROSS JOIN target_runs t \
     WHERE q.tenant_id=t.tenant_id AND q.run_id=t.run_id \
    RETURNING q.run_id \
) \
SELECT CASE WHEN q.request_id IS NOT NULL THEN 'applied' ELSE a.result_code END \
           AS result_code, \
       q.request_id::text AS request_id, \
       (SELECT count(*)::bigint FROM candidates) AS selection_count \
  FROM authorized a LEFT JOIN request q ON true \
 WHERE (SELECT count(*) FROM queued) >= 0";

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn actor(role: ProjectDispositionRole) -> AuthenticatedActor {
        AuthenticatedActor::new("principal:alice", role).unwrap()
    }

    #[test]
    fn resolve_requires_admin_complete_emission_and_evidence() {
        let request = SingleDisposition {
            attempt_id: "a".into(),
            action: DispositionAction::Resolve,
            correlation_id: "corr".into(),
            audit: Some(ResolutionAudit {
                basis: ResolutionBasis::ExternalEvidence,
                evidence_ref: "case:7".into(),
            }),
            outcome: Some(ResolutionOutcome::Succeeded {
                payload: json!(null),
                port: "main".into(),
                context: Some(json!({"verified": true})),
            }),
        };
        assert!(validate_single(&actor(ProjectDispositionRole::ProjectAdmin), &request).is_ok());
        assert_eq!(
            validate_single(&actor(ProjectDispositionRole::ProjectDeployer), &request)
                .unwrap_err()
                .code(),
            "resolve-role-required"
        );
        assert!(validate_platform_single(&request).is_ok());

        let mut missing_port = request.clone();
        let Some(ResolutionOutcome::Succeeded { port, .. }) = missing_port.outcome.as_mut() else {
            unreachable!()
        };
        port.clear();
        assert_eq!(
            validate_single(&actor(ProjectDispositionRole::ProjectAdmin), &missing_port)
                .unwrap_err()
                .code(),
            "success-port-required"
        );
    }

    #[test]
    fn failure_vocabulary_has_no_retry_cancel_or_rate_limit_variant() {
        assert_eq!(ResolvedFailureKind::Terminal.as_sql(), "terminal");
        assert_eq!(ResolvedFailureKind::InvalidInput.as_sql(), "invalid-input");
        let serialized = serde_json::to_string(&ResolvedFailureKind::Terminal).unwrap();
        assert!(!serialized.contains("retry"));
        assert!(!serialized.contains("cancel"));
        assert!(!serialized.contains("rate"));
    }

    fn bulk_request(action: DispositionAction) -> BulkDisposition {
        BulkDisposition {
            selector: BulkSelector {
                connection_name: "erp".to_string(),
                connection_generation: "generation:7".to_string(),
                window_start: "2026-08-07T10:00:00Z".to_string(),
                window_end: "2026-08-07T11:00:00Z".to_string(),
                flow_id: Some("settlement".to_string()),
            },
            action,
            correlation_id: "incident:42".to_string(),
            audit: None,
            outcome: None,
        }
    }

    #[test]
    fn every_bulk_action_requires_stable_bounds() {
        for action in [
            DispositionAction::Park,
            DispositionAction::Release,
            DispositionAction::Resolve,
        ] {
            let mut request = bulk_request(action);
            request.selector.connection_generation.clear();
            assert_eq!(
                validate_bulk(&actor(ProjectDispositionRole::ProjectAdmin), &request)
                    .unwrap_err()
                    .code(),
                "connection-generation-required"
            );
        }
    }

    #[test]
    fn bulk_sql_materializes_one_exact_ordered_all_or_nothing_set() {
        let project = project_bulk_sql();
        assert!(project.contains("NULLIF(current_setting('app.tenant', true), '')"));
        assert!(project.contains("bounded_attempts AS MATERIALIZED"));
        assert!(project.contains("candidates AS MATERIALIZED"));
        assert!(
            project.contains("row_number() OVER (ORDER BY n.attempt_started_at, n.attempt_id)")
        );
        assert!(project.contains("'selection-empty'"));
        assert!(project.contains("authorized a WHERE a.result_code='ready'"));
        assert!(project.contains("ORDER BY c.selection_ordinal"));
        assert!(project.contains("'provenance-unverified'"));
        assert!(project.contains("'self-resolution'"));
        assert!(!project.contains("SESSION_USER::text AS principal"));
        let run_lock = project.find("locked_runs AS MATERIALIZED").unwrap();
        let queue_lock = project.find("locked_queues AS MATERIALIZED").unwrap();
        let node_lock = project.find("locked_projections AS MATERIALIZED").unwrap();
        assert!(run_lock < queue_lock && queue_lock < node_lock);
        assert!(project[node_lock..].contains("JOIN locked_queues q"));
        assert!(project.contains("current_setting('transaction_isolation') <> 'serializable'"));
        assert!(project.contains("WHEN i.action IS NULL"));
        assert!(project.contains("OR i.resolution_status IS NULL"));
        assert!(project.contains("AND (i.failure_kind IS NULL"));
        assert!(project.contains("WHEN EXISTS (SELECT 1 FROM candidates c"));
        assert!(project.contains("THEN 'run-terminal'"));

        let platform = platform_break_glass_bulk_sql();
        assert!(platform.contains("SESSION_USER::text AS principal"));
        assert!(platform.contains("'platform-admin-break-glass'::text"));
        assert!(platform.contains("'break-glass-reason-required'"));
        assert!(platform.contains("'platform-privilege-required'"));
        assert!(platform.contains("FROM pg_catalog.pg_roles"));
        assert!(!platform.contains("wamn_platform_admin"));
        assert!(!platform.contains("'self-resolution'"));
    }

    #[test]
    fn project_sql_is_fail_closed_and_platform_actor_is_not_a_parameter() {
        let project = project_single_sql();
        assert!(project.contains("'provenance-unverified'"));
        assert!(project.contains("NULLIF(n.verified_author_principal, '') IS NULL"));
        assert!(project.contains("'self-resolution'"));
        assert!(project.contains("NOT p.valid"));
        assert!(project.contains("i.failure_kind NOT IN ('terminal', 'invalid-input')"));
        assert!(project.contains("pg_input_is_valid($9::text, 'jsonb')"));
        assert!(project.contains("pg_input_is_valid($11::text, 'jsonb')"));
        assert!(project.contains("pg_input_is_valid($13::text, 'jsonb')"));
        assert!(project.contains("current_setting('transaction_isolation') <> 'serializable'"));
        assert!(project.contains("JOIN locked_queue AS q"));
        assert!(project.contains("THEN 'run-terminal'"));
        assert!(!project.contains("SESSION_USER"));

        let platform = platform_break_glass_single_sql();
        assert!(platform.contains("SESSION_USER::text AS principal"));
        assert!(platform.contains("'platform-admin-break-glass'::text"));
        assert!(platform.contains("'break-glass-reason-required'"));
        assert!(platform.contains("'platform-privilege-required'"));
        assert!(platform.contains("FROM pg_catalog.pg_roles"));
        assert!(!platform.contains("wamn_platform_admin"));
        assert!(!platform.contains("$3::text AS principal"));
    }

    #[test]
    fn automatic_park_is_fenced_indefinite_and_never_terminalizes() {
        let sql = park_effect_uncertain_sql();
        assert!(sql.contains("FROM park_effect_uncertain("));
        assert!(!sql.contains("INSERT INTO effect_dispositions"));
        assert!(!sql.contains("status = 'failed'"));
        assert!(!sql.contains("DELETE FROM run_queue"));

        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        assert!(ddl.contains("q.lease_generation IS DISTINCT FROM i.lease_generation"));
        assert!(ddl.contains("n.immutable_recovery_class <> 'never-replay'"));
        assert!(ddl.contains("available_at='infinity'::timestamptz"));
        assert!(ddl.contains("'wamn-system:flowrunner'"));
        assert!(ddl.contains("INSERT INTO effect_dispositions"));
        assert!(ddl.contains("'executor-auth-required'"));
        assert!(ddl.contains("GRANT EXECUTE ON FUNCTION wamn_run.park_effect_uncertain"));
        assert!(ddl.contains(
            "REVOKE INSERT ON wamn_run.effect_disposition_requests FROM wamn_app"
        ));
        assert!(ddl.contains("effect_disposition_requests_insert_guard"));
        assert!(ddl.contains("effect_dispositions_insert_guard"));
        assert!(ddl.contains("append_ordinal bigint GENERATED ALWAYS AS IDENTITY"));
    }

    #[test]
    fn resolution_read_is_exact_current_attempt_and_cannot_dispatch() {
        let sql = select_current_resolution_sql();
        assert!(sql.contains("e.attempt_id = n.current_effect_attempt_id"));
        assert!(sql.contains("e.run_id = n.run_id AND e.node_id = n.node_id"));
        assert!(sql.contains("e.occurrence = n.occurrence"));
        assert!(sql.contains("JOIN effect_attempt_dispatches"));
        assert!(sql.contains("d.action = 'resolve'"));
        assert!(sql.contains("LEFT JOIN effect_attempt_outcomes"));
        assert!(sql.contains("o.attempt_id IS NULL"));
        assert!(sql.contains("e.recovery_class = 'never-replay'"));
        assert!(!sql.contains("INSERT"));
        assert!(!sql.contains("UPDATE"));
        assert!(!sql.contains("effect_attempt_dispatches ("));
    }

    #[test]
    fn run_view_distinguishes_every_disposition_state() {
        let sql = select_run_dispositions_sql();
        for state in ["pending", "parked", "released", "resolved"] {
            assert!(sql.contains(state));
        }
        assert!(sql.contains("e.tenant_id = NULLIF(current_setting('app.tenant', true), '')"));
        assert!(sql.contains("ORDER BY d.append_ordinal DESC"));
    }
}
