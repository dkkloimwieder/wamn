//! Callable-flow admission.
//!
//! Every HTTP and event producer enters the run plane through the ordered
//! same-transaction recipe returned by [`admission_sql`]. Its first statement
//! takes the stable catalog-head key-share lock; its second rechecks the
//! producer-specific definition and writes the run, queue row, and (for HTTP)
//! admission ledger atomically.

use serde_json::Value;
use wamn_pg_core::Identifier;

use crate::queue::PartitionPolicy;

/// Validated schema containing the durable run-state tables and functions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunStateSchema(Identifier);

impl RunStateSchema {
    /// Validate a deployment-supplied run-state schema name.
    pub fn new(value: impl Into<String>) -> Result<Self, wamn_pg_core::InvalidIdentifier> {
        Identifier::new(value).map(Self)
    }

    /// The schema name before PostgreSQL identifier quoting.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    fn qualifier(&self) -> String {
        format!("{}.", self.0.quoted())
    }
}

impl Default for RunStateSchema {
    fn default() -> Self {
        Self(Identifier::new("wamn_run").expect("the canonical run-state schema is valid"))
    }
}

/// Producer variant accepted by the admission transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionProducer {
    Http,
    Event,
}

impl AdmissionProducer {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Event => "event",
        }
    }
}

/// Resolved queue ordering carried by every producer into admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionOrdering {
    partition_key: Option<String>,
    partition_policy: PartitionPolicy,
}

impl AdmissionOrdering {
    /// Unordered work uses the queue's blocking default and no partition key.
    pub fn unordered() -> AdmissionOrdering {
        AdmissionOrdering {
            partition_key: None,
            partition_policy: PartitionPolicy::Blocking,
        }
    }

    /// Validate resolved adapter inputs before binding the admission statement.
    pub fn from_parts(
        partition_key: Option<String>,
        partition_policy: &str,
    ) -> Result<AdmissionOrdering, AdmissionOrderingError> {
        let partition_policy = PartitionPolicy::from_sql(partition_policy)
            .ok_or(AdmissionOrderingError::UnknownPolicy)?;
        match partition_key {
            Some(partition_key) if partition_key.is_empty() => {
                Err(AdmissionOrderingError::EmptyPartitionKey)
            }
            None if partition_policy == PartitionPolicy::Leapfrog => {
                Err(AdmissionOrderingError::UnkeyedLeapfrog)
            }
            partition_key => Ok(AdmissionOrdering {
                partition_key,
                partition_policy,
            }),
        }
    }

    pub fn partition_key(&self) -> Option<&str> {
        self.partition_key.as_deref()
    }

    pub fn partition_policy(&self) -> PartitionPolicy {
        self.partition_policy
    }
}

/// Invalid queue-ordering input at the admission boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionOrderingError {
    UnknownPolicy,
    EmptyPartitionKey,
    UnkeyedLeapfrog,
}

/// Typed result returned by the admission transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionResult {
    Admitted { run_id: String },
    Duplicate { run_id: Option<String> },
    HeadNotFound,
    HeadDrift,
    InactiveDefinition,
    DefinitionDrift,
    RegistrationNotFound,
    RegistrationDrift,
    InvalidRegistrationHash,
    InvalidEventLineage,
    IdempotencyKeyReused,
    IdempotencyScopeChanged,
    ConflictingRunIdentity,
    InvalidProducer,
    InvalidInput,
}

impl AdmissionResult {
    /// Decode the transition's `(result_code, run_id)` row.
    pub fn from_parts(code: &str, run_id: Option<String>) -> Option<Self> {
        match code {
            "admitted" => Some(Self::Admitted { run_id: run_id? }),
            "duplicate" => Some(Self::Duplicate { run_id }),
            "head-not-found" => Some(Self::HeadNotFound),
            "head-drift" => Some(Self::HeadDrift),
            "inactive-definition" => Some(Self::InactiveDefinition),
            "definition-drift" => Some(Self::DefinitionDrift),
            "registration-not-found" => Some(Self::RegistrationNotFound),
            "registration-drift" => Some(Self::RegistrationDrift),
            "invalid-registration-hash" => Some(Self::InvalidRegistrationHash),
            "invalid-event-lineage" => Some(Self::InvalidEventLineage),
            "idempotency-key-reused" => Some(Self::IdempotencyKeyReused),
            "idempotency-scope-changed" => Some(Self::IdempotencyScopeChanged),
            "conflicting-run-identity" => Some(Self::ConflictingRunIdentity),
            "invalid-producer" => Some(Self::InvalidProducer),
            "invalid-input" => Some(Self::InvalidInput),
            _ => None,
        }
    }
}

/// Hash a registration document using the platform's canonical JSON identity.
///
/// Event producers bind both the canonical document and this digest. Admission
/// rechecks the live row against the document and records the digest in the run
/// context; a stale document/hash pair cannot admit after a registration edit.
pub fn registration_hash(document: &Value) -> String {
    wamn_flow::canonical_json_sha256(document)
}

/// Produce the exact registration bytes and hash bound at final admission.
pub fn registration_evidence(document: &Value) -> (String, String) {
    let bytes = wamn_flow::canonical_json_bytes(document);
    (
        String::from_utf8(bytes).expect("canonical JSON is valid UTF-8"),
        registration_hash(document),
    )
}

/// The ordered SQL recipe for one admission transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionSql {
    lock_head: String,
    admit: String,
}

impl AdmissionSql {
    /// Execute first in the transaction to acquire the stable head lock.
    pub fn lock_head(&self) -> &str {
        &self.lock_head
    }

    /// Execute second in the same transaction to recheck and mutate atomically.
    pub fn admit(&self) -> &str {
        &self.admit
    }
}

/// Build the required lock-then-admit transaction recipe.
pub fn admission_sql() -> AdmissionSql {
    AdmissionSql {
        lock_head: lock_catalog_head_sql(),
        admit: admit_sql(),
    }
}

/// Build the admission recipe for a validated deployment-specific schema.
///
/// The canonical schema retains its historical byte shape. Alternate schemas
/// are always PostgreSQL-quoted and can only enter through [`RunStateSchema`].
pub fn admission_sql_for_schema(schema: &RunStateSchema) -> AdmissionSql {
    let canonical = admission_sql();
    if schema.as_str() == RunStateSchema::default().as_str() {
        return canonical;
    }
    let qualifier = schema.qualifier();
    AdmissionSql {
        lock_head: canonical.lock_head.replace("wamn_run.", &qualifier),
        admit: canonical.admit.replace("wamn_run.", &qualifier),
    }
}

/// Acquire the catalog-head key-share lock for an admission transaction.
///
/// This statement and the recipe's admission statement run in one transaction.
///
/// Keeping lock acquisition as its own top-level statement is required under
/// PostgreSQL READ COMMITTED: after waiting behind publication, the admission
/// statement receives a fresh snapshot and observes the winning catalog version.
fn lock_catalog_head_sql() -> String {
    "SELECT wamn_run.lock_catalog_head(\
       NULLIF(current_setting('app.tenant', true), ''), $1, $2) \
     AS applied_catalog_version"
        .to_string()
}

/// Admit one callable-flow run.
///
/// Parameters:
///
/// 1. producer (`http | event`)
/// 2. catalog id
/// 3. environment
/// 4. expected catalog version
/// 5. attachment id (HTTP)
/// 6. expected definition hash (HTTP)
/// 7. flow id
/// 8. flow version
/// 9. run id
/// 10. input JSON text
/// 11. invocation-context JSON text
/// 12. platform revision
/// 13. response deadline
/// 14. run deadline
/// 15. principal digest (HTTP)
/// 16. client-key digest (HTTP)
/// 17. request fingerprint (HTTP)
/// 18. admission expiry (HTTP)
/// 19. inline executor identity (HTTP)
/// 20. inline lease TTL milliseconds (HTTP)
/// 21. registration id (event)
/// 22. event sequence (event)
/// 23. RFC 8785 registration JSON text (event)
/// 24. canonical registration hash (event)
/// 25. immediate source run id (event)
/// 26. causal root run id (event)
/// 27. causal depth (event)
/// 28. resolved partition key (all producers; null means unordered)
/// 29. resolved partition policy (all producers; `blocking | leapfrog`)
///
/// HTTP identity is reserved in the deferred-FK ledger before the run insert.
/// The named unique constraint chooses the concurrent winner without allowing a
/// losing transaction to leave a partial run or queue row.
fn admit_sql() -> String {
    "\
WITH input AS ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           $1::text AS producer, $2::text AS catalog_id, $3::text AS environment, \
           $4::int AS expected_catalog_version, $5::text AS attachment_id, \
           $6::text AS expected_definition_hash, $7::text AS flow_id, \
           $8::int AS flow_version, $9::text AS run_id, \
           $10::text::jsonb AS input_json, $11::text::jsonb AS invocation_context, \
           $12::text AS platform_revision, $13::timestamptz AS response_deadline_at, \
           $14::timestamptz AS run_deadline_at, $15::text AS principal_digest, \
           $16::text AS client_key_digest, $17::text AS request_fingerprint, \
           $18::timestamptz AS admission_expires_at, $19::text AS executor_id, \
           $20::bigint AS lease_ttl_ms, $21::text AS registration_id, \
           $22::bigint AS event_seq, $23::text::jsonb AS registration_document, \
           $24::text AS registration_hash, $25::text AS event_source_run_id, \
           $26::text AS event_root_run_id, $27::int AS event_depth, \
           $28::text AS partition_key, $29::text AS partition_policy \
), \
locked_head AS MATERIALIZED ( \
    SELECT h.applied_catalog_version \
      FROM catalog.catalog_heads AS h, input AS i \
     WHERE h.tenant_id = i.tenant_id AND h.catalog_id = i.catalog_id \
       AND h.environment = i.environment \
), \
active_definition AS MATERIALIZED ( \
    SELECT a.attachment_id, a.attachment_kind, a.definition_hash, \
           a.flow_id, f.flow_version \
      FROM catalog.active_attachments AS a \
      JOIN input AS i ON a.tenant_id = i.tenant_id \
                     AND a.catalog_id = i.catalog_id \
                     AND a.environment = i.environment \
                     AND a.attachment_id = i.attachment_id \
      JOIN catalog.release_flows AS f \
        ON f.tenant_id = a.tenant_id AND f.catalog_id = a.catalog_id \
       AND f.catalog_version = a.catalog_version AND f.flow_id = a.flow_id \
     WHERE i.producer = 'http' AND a.attachment_kind = 'http' \
), \
live_registration AS MATERIALIZED ( \
    SELECT r.registration_id, r.flow_id, r.registration \
      FROM catalog.event_registrations AS r, input AS i \
     WHERE i.producer = 'event' AND r.tenant_id = i.tenant_id \
       AND r.catalog_id = i.catalog_id AND r.registration_id = i.registration_id \
     FOR KEY SHARE OF r \
), \
source_lineage AS MATERIALIZED ( \
    SELECT r.run_id, COALESCE(r.event_root_run_id, r.run_id) AS root_run_id, \
           COALESCE(r.event_depth, 0) AS depth \
      FROM wamn_run.runs AS r, input AS i \
     WHERE i.producer = 'event' AND i.event_source_run_id <> i.run_id \
       AND r.tenant_id = i.tenant_id AND r.run_id = i.event_source_run_id \
     FOR KEY SHARE OF r \
), \
release_flow AS MATERIALIZED ( \
    SELECT f.flow_id, f.flow_version, a.artifact_hash \
      FROM catalog.release_flows AS f \
      JOIN catalog.flow_artifacts AS a \
        ON a.tenant_id = f.tenant_id AND a.flow_id = f.flow_id \
       AND a.flow_version = f.flow_version \
      CROSS JOIN input AS i CROSS JOIN locked_head AS h \
     WHERE f.tenant_id = i.tenant_id AND f.catalog_id = i.catalog_id \
       AND f.catalog_version = h.applied_catalog_version \
       AND f.flow_id = i.flow_id AND f.flow_version = i.flow_version \
), \
existing_http AS MATERIALIZED ( \
    SELECT a.* FROM wamn_run.invocation_admissions AS a, input AS i \
     WHERE i.producer = 'http' AND a.tenant_id = i.tenant_id \
       AND a.catalog_id = i.catalog_id AND a.environment = i.environment \
       AND a.attachment_id = i.attachment_id \
       AND a.principal_digest = i.principal_digest \
       AND a.client_key_digest = i.client_key_digest \
     FOR KEY SHARE OF a \
), \
existing_queue AS MATERIALIZED ( \
    SELECT q.* FROM wamn_run.run_queue AS q, input AS i \
     WHERE q.tenant_id = i.tenant_id \
       AND q.run_id = COALESCE((SELECT run_id FROM existing_http), ( \
         SELECT r.run_id FROM wamn_run.runs AS r \
          WHERE r.tenant_id = i.tenant_id \
            AND (r.run_id = i.run_id OR (i.producer = 'event' \
              AND r.idempotency_key = 'evt:' || i.registration_id || ':' || i.event_seq::text)) \
       ), i.run_id) \
     FOR KEY SHARE OF q \
), \
existing_identity_run AS MATERIALIZED ( \
    SELECT r.* FROM wamn_run.runs AS r, input AS i \
     WHERE r.tenant_id = i.tenant_id \
       AND (r.run_id = i.run_id \
         OR (i.producer = 'event' \
           AND r.idempotency_key = 'evt:' || i.registration_id || ':' || i.event_seq::text)) \
     FOR KEY SHARE OF r \
), \
classified AS ( \
    SELECT CASE \
      WHEN i.producer IS NULL OR i.producer NOT IN ('http', 'event') \
        THEN 'invalid-producer' \
      WHEN i.tenant_id IS NULL OR i.catalog_id IS NULL OR i.catalog_id = '' \
        OR i.environment IS NULL OR i.environment = '' \
        OR i.flow_id IS NULL OR i.flow_id = '' OR i.flow_version IS NULL \
        OR i.flow_version <= 0 OR i.run_id IS NULL OR i.run_id = '' \
        OR i.input_json IS NULL OR i.invocation_context IS NULL \
        OR jsonb_typeof(i.invocation_context) IS DISTINCT FROM 'object' \
        OR i.platform_revision IS NULL OR i.platform_revision = '' THEN 'invalid-input' \
      WHEN i.partition_policy IS NULL \
        OR i.partition_policy NOT IN ('blocking', 'leapfrog') \
        OR i.partition_key = '' \
        OR (i.partition_key IS NULL AND i.partition_policy <> 'blocking') \
        THEN 'invalid-input' \
      WHEN i.response_deadline_at IS NOT NULL AND i.run_deadline_at IS NOT NULL \
        AND i.response_deadline_at > i.run_deadline_at THEN 'invalid-input' \
      WHEN i.producer = 'http' AND (i.attachment_id IS NULL \
        OR i.attachment_id = '' OR i.expected_definition_hash IS NULL \
        OR i.expected_definition_hash = '' OR i.principal_digest IS NULL \
        OR i.principal_digest = '' OR i.client_key_digest IS NULL \
        OR i.client_key_digest = '' OR i.request_fingerprint IS NULL \
        OR i.request_fingerprint = '' OR i.admission_expires_at IS NULL \
        OR i.executor_id IS NULL OR i.executor_id = '' \
        OR i.lease_ttl_ms IS NULL OR i.lease_ttl_ms <= 0 \
        OR i.registration_id IS NOT NULL OR i.event_seq IS NOT NULL \
        OR i.registration_document IS NOT NULL OR i.registration_hash IS NOT NULL \
        OR i.event_source_run_id IS NOT NULL OR i.event_root_run_id IS NOT NULL \
        OR i.event_depth IS NOT NULL) \
        THEN 'invalid-input' \
      WHEN i.producer = 'event' AND (i.registration_id IS NULL \
        OR i.registration_id = '' OR i.event_seq IS NULL OR i.event_seq < 0 \
        OR i.registration_document IS NULL OR i.registration_hash IS NULL \
        OR i.registration_hash = '' OR i.event_source_run_id IS NULL \
        OR i.event_source_run_id = '' OR i.event_root_run_id IS NULL \
        OR i.event_root_run_id = '' OR i.event_depth IS NULL \
        OR i.event_depth < 0 OR i.event_depth > 16 OR i.attachment_id IS NOT NULL \
        OR i.expected_definition_hash IS NOT NULL OR i.response_deadline_at IS NOT NULL \
        OR i.principal_digest IS NOT NULL OR i.client_key_digest IS NOT NULL \
        OR i.request_fingerprint IS NOT NULL OR i.admission_expires_at IS NOT NULL \
        OR i.executor_id IS NOT NULL OR i.lease_ttl_ms IS NOT NULL) \
        THEN 'invalid-input' \
      WHEN h.applied_catalog_version IS NULL THEN 'head-not-found' \
      WHEN h.applied_catalog_version <> i.expected_catalog_version THEN 'head-drift' \
      WHEN rf.flow_id IS NULL THEN 'definition-drift' \
      WHEN i.producer = 'http' AND d.attachment_id IS NULL THEN 'inactive-definition' \
      WHEN i.producer = 'http' \
       AND (d.definition_hash <> i.expected_definition_hash \
         OR d.flow_id <> i.flow_id OR d.flow_version <> i.flow_version) \
        THEN 'definition-drift' \
      WHEN i.producer = 'event' AND er.registration_id IS NULL THEN 'registration-not-found' \
      WHEN i.producer = 'event' \
       AND (er.flow_id <> i.flow_id OR er.registration <> i.registration_document) \
        THEN 'registration-drift' \
      WHEN i.producer = 'event' \
       AND i.registration_hash <> ('sha256:' || encode( \
         sha256(convert_to($23::text, 'UTF8')), 'hex')) \
        THEN 'invalid-registration-hash' \
      WHEN i.producer = 'event' AND ( \
        i.input_json ? 'causation' OR i.invocation_context ? 'causation' \
        OR (i.event_depth = 0 AND (i.event_source_run_id <> i.run_id \
          OR i.event_root_run_id <> i.run_id)) \
        OR (i.event_depth > 0 AND (i.event_source_run_id = i.run_id \
          OR sl.run_id IS NULL OR sl.root_run_id <> i.event_root_run_id \
          OR sl.depth + 1 <> i.event_depth))) \
        THEN 'invalid-event-lineage' \
      WHEN i.producer = 'http' AND eh.run_id IS NOT NULL \
       AND eh.definition_hash <> i.expected_definition_hash \
        THEN 'idempotency-scope-changed' \
      WHEN i.producer = 'http' AND eh.run_id IS NOT NULL \
       AND eh.client_request_fingerprint <> i.request_fingerprint \
        THEN 'idempotency-key-reused' \
      WHEN eq.run_id IS NOT NULL \
       AND (eq.partition_key IS DISTINCT FROM i.partition_key \
         OR eq.partition_policy <> i.partition_policy) \
        THEN 'conflicting-run-identity' \
      WHEN i.producer = 'http' AND eh.run_id IS NOT NULL THEN 'duplicate' \
      WHEN xr.run_id IS NOT NULL \
       AND (xr.trigger_source <> i.producer \
         OR xr.flow_id <> i.flow_id OR xr.flow_version <> i.flow_version \
         OR xr.catalog_id IS DISTINCT FROM i.catalog_id \
         OR xr.catalog_version IS DISTINCT FROM i.expected_catalog_version \
         OR xr.attachment_id IS DISTINCT FROM i.attachment_id \
         OR xr.registration_id IS DISTINCT FROM i.registration_id \
         OR xr.event_source_run_id IS DISTINCT FROM i.event_source_run_id \
         OR xr.event_root_run_id IS DISTINCT FROM i.event_root_run_id \
         OR xr.event_depth IS DISTINCT FROM i.event_depth \
         OR xr.input_json IS DISTINCT FROM i.input_json) \
        THEN 'conflicting-run-identity' \
      WHEN xr.run_id IS NOT NULL THEN 'duplicate' \
      ELSE 'ready' END AS result_code, \
      i.*, rf.artifact_hash, eh.run_id AS admitted_run_id, xr.run_id AS existing_run_id \
    FROM input AS i \
    LEFT JOIN locked_head AS h ON true \
    LEFT JOIN active_definition AS d ON true \
    LEFT JOIN live_registration AS er ON true \
    LEFT JOIN source_lineage AS sl ON true \
    LEFT JOIN release_flow AS rf ON true \
    LEFT JOIN existing_http AS eh ON true \
    LEFT JOIN existing_queue AS eq ON true \
    LEFT JOIN existing_identity_run AS xr ON true \
), \
created_http AS ( \
    INSERT INTO wamn_run.invocation_admissions \
      (tenant_id, catalog_id, environment, attachment_id, definition_hash, \
       principal_digest, client_key_digest, client_request_fingerprint, \
       admitted_catalog_version, admitted_flow_version, run_id, expires_at) \
    SELECT c.tenant_id, c.catalog_id, c.environment, c.attachment_id, \
           c.expected_definition_hash, c.principal_digest, c.client_key_digest, \
           c.request_fingerprint, c.expected_catalog_version, c.flow_version, \
           c.run_id, c.admission_expires_at \
      FROM classified AS c \
     WHERE c.producer = 'http' AND c.result_code = 'ready' \
    ON CONFLICT ON CONSTRAINT invocation_admissions_identity DO NOTHING \
    RETURNING tenant_id, run_id \
), \
created_run AS ( \
    INSERT INTO wamn_run.runs \
      (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment, \
       attachment_id, registration_id, status, trigger_source, input_json, \
       event_source_run_id, event_root_run_id, event_depth, \
       invocation_context, admission_context_version, platform_revision, idempotency_key, \
       response_deadline_at, run_deadline_at) \
    SELECT c.tenant_id, c.run_id, c.flow_id, c.flow_version, c.catalog_id, \
           c.expected_catalog_version, c.environment, \
           CASE WHEN c.producer = 'http' THEN c.attachment_id END, \
           CASE WHEN c.producer = 'event' THEN c.registration_id END, \
           'dispatched', c.producer, c.input_json, \
           CASE WHEN c.producer = 'event' THEN c.event_source_run_id END, \
           CASE WHEN c.producer = 'event' THEN c.event_root_run_id END, \
           CASE WHEN c.producer = 'event' THEN c.event_depth END, \
           jsonb_build_object( \
             'version', '0.1', \
             'principal', jsonb_build_object( \
               'tenant-id', c.tenant_id, 'environment', c.environment, \
               'catalog-id', c.catalog_id, 'catalog-version', c.expected_catalog_version, \
               'run-id', c.run_id, \
               'flow-id', c.flow_id, 'flow-version', c.flow_version, \
               'artifact-digest', c.artifact_hash), \
             'source', CASE WHEN c.producer = 'event' \
               THEN jsonb_set(c.invocation_context, '{registration-hash}', \
                              to_jsonb(c.registration_hash), true) \
               ELSE c.invocation_context END), \
           '0.1', c.platform_revision, \
           CASE WHEN c.producer = 'event' \
             THEN 'evt:' || c.registration_id || ':' || c.event_seq::text END, \
           c.response_deadline_at, c.run_deadline_at \
      FROM classified AS c WHERE c.result_code = 'ready' \
       AND (c.producer <> 'http' OR EXISTS (SELECT 1 FROM created_http)) \
    ON CONFLICT DO NOTHING \
    RETURNING tenant_id, run_id \
), \
created_queue AS ( \
    INSERT INTO wamn_run.run_queue \
      (tenant_id, run_id, partition_key, partition_policy, available_at, \
       lease_owner, lease_expires_at, lease_generation, stream_seq) \
    SELECT r.tenant_id, r.run_id, c.partition_key, c.partition_policy, now(), \
           CASE WHEN c.producer = 'http' THEN c.executor_id END, \
           CASE WHEN c.producer = 'http' \
             THEN now() + (c.lease_ttl_ms * interval '1 millisecond') END, \
           CASE WHEN c.producer = 'http' THEN 1 ELSE 0 END, \
           CASE WHEN c.producer = 'event' THEN c.event_seq ELSE 0 END \
      FROM created_run AS r JOIN classified AS c USING (tenant_id, run_id) \
    RETURNING tenant_id, run_id \
) \
SELECT CASE \
         WHEN c.result_code = 'ready' AND c.producer = 'http' \
          AND NOT EXISTS (SELECT 1 FROM created_http) THEN 'duplicate' \
         WHEN q.run_id IS NOT NULL THEN 'admitted' \
         WHEN c.result_code = 'ready' THEN 'duplicate' \
         ELSE c.result_code END AS result_code, \
       CASE WHEN c.result_code = 'ready' AND q.run_id IS NULL THEN NULL \
         ELSE COALESCE(q.run_id, c.admitted_run_id, c.existing_run_id) END AS run_id \
 FROM classified AS c LEFT JOIN created_queue AS q USING (tenant_id, run_id) \
 WHERE (SELECT count(*) FROM created_http) >= 0"
        .to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn producer_literals_are_stable() {
        assert_eq!(AdmissionProducer::Http.as_sql(), "http");
        assert_eq!(AdmissionProducer::Event.as_sql(), "event");
        assert!(
            admission_sql()
                .admit()
                .contains("producer NOT IN ('http', 'event')")
        );
    }

    #[test]
    fn default_admission_schema_remains_wamn_run() {
        let schema = RunStateSchema::default();
        let configured = admission_sql_for_schema(&schema);
        assert_eq!(schema.as_str(), "wamn_run");
        assert_eq!(configured, admission_sql());
    }

    #[test]
    fn custom_admission_schema_qualifies_every_run_state_reference() {
        let schema = RunStateSchema::new("wamn_runner_demo").unwrap();
        let canonical = admission_sql();
        let configured = admission_sql_for_schema(&schema);
        let canonical_references = canonical.lock_head.matches("wamn_run.").count()
            + canonical.admit.matches("wamn_run.").count();
        let configured_sql = format!("{} {}", configured.lock_head, configured.admit);

        assert_eq!(
            canonical_references, 9,
            "update this pin when admission adds a run-state boundary"
        );
        assert_eq!(configured_sql.matches("\"wamn_runner_demo\".").count(), 9);
        assert!(!configured_sql.contains("wamn_run."));
    }

    #[test]
    fn run_state_schema_rejects_invalid_postgresql_identifiers() {
        assert!(RunStateSchema::new("").is_err());
        assert!(RunStateSchema::new("bad\0schema").is_err());
        assert!(RunStateSchema::new("x".repeat(64)).is_err());
        let quoted = admission_sql_for_schema(&RunStateSchema::new("odd schema").unwrap());
        assert!(
            quoted
                .lock_head()
                .contains("\"odd schema\".lock_catalog_head")
        );
    }

    #[test]
    fn admission_ordering_rejects_untyped_or_incoherent_inputs() {
        assert_eq!(
            AdmissionOrdering::from_parts(Some("site-7".to_string()), "leapfrog")
                .unwrap()
                .partition_policy(),
            PartitionPolicy::Leapfrog
        );
        assert_eq!(
            AdmissionOrdering::unordered().partition_key(),
            None,
            "unordered work has no key"
        );
        assert_eq!(
            AdmissionOrdering::from_parts(None, "leapfrog"),
            Err(AdmissionOrderingError::UnkeyedLeapfrog)
        );
        assert_eq!(
            AdmissionOrdering::from_parts(Some(String::new()), "blocking"),
            Err(AdmissionOrderingError::EmptyPartitionKey)
        );
        assert_eq!(
            AdmissionOrdering::from_parts(Some("site-7".to_string()), "unknown"),
            Err(AdmissionOrderingError::UnknownPolicy)
        );
    }

    #[test]
    fn registration_hash_is_canonical() {
        assert_eq!(
            registration_hash(&json!({"b": 2, "a": 1})),
            registration_hash(&json!({"a": 1, "b": 2}))
        );
    }

    #[test]
    fn registration_evidence_uses_the_hashed_rfc8785_bytes() {
        let document = json!({"z": 1.0, "nested": {"b": "β", "a": true}});
        let (bytes, hash) = registration_evidence(&document);
        assert_eq!(hash, wamn_flow::canonical_json_sha256(&document));
        assert_eq!(
            registration_hash(&serde_json::from_str(&bytes).unwrap()),
            hash
        );
        assert!(bytes.starts_with("{\"nested\":"));
    }

    #[test]
    fn admission_recipe_locks_then_mutates_in_one_transaction() {
        let recipe = admission_sql();
        let sql = recipe.admit();
        assert_eq!(sql.matches("WITH input AS").count(), 1);
        assert!(sql.contains("FROM catalog.catalog_heads AS h"));
        assert!(recipe.lock_head().contains("wamn_run.lock_catalog_head"));
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        assert!(ddl.contains("FOR SHARE OF head"));
        assert!(ddl.contains("SECURITY DEFINER"));
        assert!(sql.contains("INSERT INTO wamn_run.runs"));
        assert!(sql.contains("INSERT INTO wamn_run.run_queue"));
        assert!(sql.contains("c.partition_key, c.partition_policy, now()"));
        assert!(!sql.contains("UPDATE wamn_run.run_queue"));
        assert!(sql.contains("INSERT INTO wamn_run.invocation_admissions"));
        assert!(sql.contains("FROM created_run AS r"));
        assert!(sql.contains("EXISTS (SELECT 1 FROM created_http)"));
        assert!(ddl.contains("DEFERRABLE INITIALLY DEFERRED"));
        assert_ne!(recipe.lock_head(), recipe.admit());
    }

    #[test]
    fn admission_persists_the_versioned_release_artifact_principal() {
        let sql = admission_sql().admit().to_string();
        assert!(sql.contains("JOIN catalog.flow_artifacts AS a"));
        assert!(sql.contains("a.artifact_hash"));
        for field in [
            "'version', '0.1'",
            "'principal'",
            "'tenant-id', c.tenant_id",
            "'environment', c.environment",
            "'catalog-id', c.catalog_id",
            "'catalog-version', c.expected_catalog_version",
            "'run-id', c.run_id",
            "'flow-id', c.flow_id",
            "'flow-version', c.flow_version",
            "'artifact-digest', c.artifact_hash",
            "'source'",
            "invocation_context, admission_context_version",
        ] {
            assert!(sql.contains(field), "missing trusted-context field {field}");
        }
        assert!(sql.contains("jsonb_typeof(i.invocation_context) IS DISTINCT FROM 'object'"));
    }

    #[test]
    fn producer_specific_checks_and_queue_states_are_pinned() {
        let sql = admission_sql().admit().to_string();
        for refusal in [
            "head-drift",
            "invalid-input",
            "inactive-definition",
            "definition-drift",
            "registration-drift",
            "invalid-registration-hash",
            "invalid-event-lineage",
            "idempotency-key-reused",
            "idempotency-scope-changed",
            "conflicting-run-identity",
        ] {
            assert!(sql.contains(refusal), "missing {refusal}");
        }
        assert!(sql.contains("THEN 1 ELSE 0 END"));
        assert!(sql.contains("THEN c.executor_id END"));
        assert!(sql.contains("THEN c.event_seq ELSE 0 END"));
        assert!(sql.contains("'evt:' || c.registration_id"));
        assert!(sql.contains("i.partition_policy NOT IN ('blocking', 'leapfrog')"));
        assert!(sql.contains("i.partition_key IS NULL AND i.partition_policy <> 'blocking'"));
        assert!(sql.contains("eq.partition_key IS DISTINCT FROM i.partition_key"));
        assert!(sql.contains("sl.depth + 1 <> i.event_depth"));
        assert!(sql.contains("xr.event_root_run_id IS DISTINCT FROM i.event_root_run_id"));
    }

    #[test]
    fn result_codes_decode_without_collapsing_refusals() {
        assert_eq!(
            AdmissionResult::from_parts("admitted", Some("r1".to_string())),
            Some(AdmissionResult::Admitted {
                run_id: "r1".to_string()
            })
        );
        assert_eq!(
            AdmissionResult::from_parts("duplicate", None),
            Some(AdmissionResult::Duplicate { run_id: None })
        );
        assert_eq!(
            AdmissionResult::from_parts("idempotency-key-reused", None),
            Some(AdmissionResult::IdempotencyKeyReused)
        );
        assert_eq!(
            AdmissionResult::from_parts("registration-drift", None),
            Some(AdmissionResult::RegistrationDrift)
        );
    }
}
