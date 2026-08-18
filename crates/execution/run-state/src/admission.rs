//! Callable-flow and private management admission.
//!
//! Producers enter the run plane through the same-transaction recipe
//! returned by [`admission_transaction`]. Its first statement takes the stable
//! catalog-head key-share lock; its second rechecks the producer-specific
//! definition and writes the run, queue row, and any producer ledger atomically.
//!
//! [`AdmissionTransition::ManagementRun`] is the private cross-plane seam
//! `wamn-0h0g.7.5` ratified. It is a second STATEMENT, never a second run
//! creation path: it takes the same head lock, writes the same ordinary
//! `wamn_run.runs` and `wamn_run.run_queue` facts, and answers in the same
//! [`AdmissionResult`] vocabulary. Two properties a shared statement cannot hold
//! keep it separate. It names `capture_mode` in its run insert, and PostgreSQL
//! checks column privileges per NAMED column, so folding it into the callable
//! statement would oblige `wamn_app` to hold `INSERT (capture_mode)` — the exact
//! authority `wamn-0h0g.7.5` denies every guest-visible role. And its producer
//! key needs parameters the callable statement does not carry, whose numbers are
//! already fixed by the guests binding that statement positionally.

use serde_json::Value;
use wamn_pg_core::Identifier;

use crate::capture::CaptureMode;

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

/// Producer variant accepted by the admission transitions.
///
/// `Http` and `Event` are the callable-flow producers. `DraftRun` and `TestCase`
/// are the private management producers: they carry a stable producer key instead
/// of a caller-minted identity and RECEIVE their capture value rather than
/// presenting one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionProducer {
    Http,
    Event,
    DraftRun,
    TestCase,
}

impl AdmissionProducer {
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Event => "event",
            Self::DraftRun => "draft-run",
            Self::TestCase => "test-case",
        }
    }

    /// The `runs.trigger_source` literal this producer admits under.
    ///
    /// A draft run's literal is `scenario-draft`, not its producer name: that
    /// exact value is the DDL's `runs_capture_mode_source_check` operand and half
    /// of the draft-safe connection-grant predicate the effect claim path checks,
    /// so it is not free to rename here.
    pub const fn trigger_source(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Event => "event",
            Self::DraftRun => "scenario-draft",
            Self::TestCase => "test-case",
        }
    }

    /// The capture value admission fills for this producer.
    ///
    /// Capture is derived from the typed producer and is never an admission
    /// parameter: a caller able to choose it would hold management-only
    /// authority over an admitted run's node I/O.
    pub const fn capture_mode(self) -> CaptureMode {
        match self {
            Self::Http | Self::Event | Self::TestCase => CaptureMode::Off,
            Self::DraftRun => CaptureMode::Full,
        }
    }
}

/// The stable coordinate one private management admission is idempotent under.
///
/// A draft run is keyed by its authoring command id; a test case is keyed by its
/// report id and case ordinal. Admission persists the key as the run's
/// `idempotency_key`, so the partial unique index over that column — not an
/// application check — is what refuses a second run under one coordinate, and it
/// is also how a crashed producer's control-side reconciler recovers the run id
/// it never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementProducerKey<'a> {
    DraftRun { command_id: &'a str },
    TestCase { report_id: &'a str, ordinal: i32 },
}

impl ManagementProducerKey<'_> {
    /// The producer this coordinate admits as.
    pub const fn producer(self) -> AdmissionProducer {
        match self {
            Self::DraftRun { .. } => AdmissionProducer::DraftRun,
            Self::TestCase { .. } => AdmissionProducer::TestCase,
        }
    }

    /// The exact `runs.idempotency_key` the management statement composes.
    pub fn idempotency_key(self) -> String {
        match self {
            Self::DraftRun { command_id } => format!("draft:{command_id}"),
            Self::TestCase { report_id, ordinal } => format!("case:{report_id}:{ordinal}"),
        }
    }
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
    MissingRootPlan,
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
            "missing-root-plan" => Some(Self::MissingRootPlan),
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

/// Admission transition composed by a producer-owned transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionTransition<'a> {
    /// Shared callable-flow admission for HTTP and event producers.
    CallableFlow { schema: &'a RunStateSchema },
    /// Private management admission for draft-run and test-case producers.
    ManagementRun { schema: &'a RunStateSchema },
}

/// The ordered statements for one admission transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionTransaction {
    lock_head: String,
    admit: String,
}

impl AdmissionTransaction {
    /// Execute first in the transaction to acquire the stable head lock.
    pub fn lock_head(&self) -> &str {
        &self.lock_head
    }

    /// Execute second in the same transaction to recheck and mutate atomically.
    pub fn admit(&self) -> &str {
        &self.admit
    }
}

/// Compose the stable admission statements for a producer-owned transaction.
pub fn admission_transaction(transition: AdmissionTransition<'_>) -> AdmissionTransaction {
    match transition {
        AdmissionTransition::CallableFlow { schema } => admission_sql_for_schema(schema),
        AdmissionTransition::ManagementRun { schema } => {
            management_admission_sql_for_schema(schema)
        }
    }
}

/// Build the required lock-then-admit transaction recipe.
pub(crate) fn admission_sql() -> AdmissionTransaction {
    AdmissionTransaction {
        lock_head: lock_catalog_head_sql(),
        admit: admit_sql(),
    }
}

/// Build the private management lock-then-admit transaction recipe.
///
/// The head lock is byte-identical to the callable recipe's: management admits
/// against the same stable head under the same conflict, and a second bridge
/// would be a second way to promote a run past a moving catalog.
pub(crate) fn management_admission_sql() -> AdmissionTransaction {
    AdmissionTransaction {
        lock_head: lock_catalog_head_sql(),
        admit: management_admit_sql(),
    }
}

/// Build the admission recipe for a validated deployment-specific schema.
///
/// The canonical schema retains its historical byte shape. Alternate schemas
/// are always PostgreSQL-quoted and can only enter through [`RunStateSchema`].
pub(crate) fn admission_sql_for_schema(schema: &RunStateSchema) -> AdmissionTransaction {
    qualify_run_state_schema(admission_sql(), schema)
}

/// Build the management recipe for a validated deployment-specific schema.
pub(crate) fn management_admission_sql_for_schema(schema: &RunStateSchema) -> AdmissionTransaction {
    qualify_run_state_schema(management_admission_sql(), schema)
}

/// Rewrite one canonical recipe onto a validated deployment-specific schema.
fn qualify_run_state_schema(
    canonical: AdmissionTransaction,
    schema: &RunStateSchema,
) -> AdmissionTransaction {
    if schema.as_str() == RunStateSchema::default().as_str() {
        return canonical;
    }
    let qualifier = schema.qualifier();
    AdmissionTransaction {
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
/// 18. registration id (event)
/// 19. event sequence (event)
/// 20. RFC 8785 registration JSON text (event)
/// 21. canonical registration hash (event)
/// 22. immediate source run id (event)
/// 23. causal root run id (event)
/// 24. causal depth (event)
///
/// HTTP identity is reserved in the deferred-FK ledger before the run insert.
/// The named unique constraint chooses the concurrent winner without allowing a
/// losing transaction to leave a partial run or queue row.
///
/// ARM ORDER — DRIFT BEFORE IDENTITY, the reverse of `management_admit_sql`,
/// and deliberately so (`wamn-0h0g.15.160`). Do not "fix" the asymmetry. A
/// keyed callable retry that must replay a stored outcome never reaches this
/// statement at all: `lookup_invocation_recovery_sql` reads the admissions
/// ledger before admission is even composed, and that lookup carries no head or
/// drift operand, which is how `wamn-0h0g.7.1`'s unconditional same-key replay
/// survives a moving catalog. A keyless request has no reusable identity by
/// design. So everything arriving here is new work, resolved against the head
/// the caller just read, and new work owes the head it names. The management
/// statement has no such pre-read and replays parameters frozen at reservation
/// time, so its exact retry has to survive its own classifier.
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
           $18::text AS registration_id, $19::bigint AS event_seq, \
           $20::text::jsonb AS registration_document, $21::text AS registration_hash, \
           $22::text AS event_source_run_id, $23::text AS event_root_run_id, \
           $24::int AS event_depth \
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
    SELECT f.tenant_id, f.flow_id, f.flow_version, f.execution_bundle_hash, \
           a.artifact_hash \
      FROM catalog.release_flows AS f \
      JOIN catalog.flow_artifacts AS a \
        ON a.tenant_id = f.tenant_id AND a.flow_id = f.flow_id \
       AND a.flow_version = f.flow_version \
      CROSS JOIN input AS i CROSS JOIN locked_head AS h \
     WHERE f.tenant_id = i.tenant_id AND f.catalog_id = i.catalog_id \
       AND f.catalog_version = h.applied_catalog_version \
       AND f.flow_id = i.flow_id AND f.flow_version = i.flow_version \
), \
root_plan AS MATERIALIZED ( \
    SELECT rf.* \
      FROM release_flow AS rf \
      JOIN catalog.execution_bundles AS bundle \
        ON bundle.tenant_id = rf.tenant_id \
       AND bundle.execution_bundle_hash = rf.execution_bundle_hash \
), \
existing_http AS MATERIALIZED ( \
    SELECT a.* FROM wamn_run.invocation_admissions AS a, input AS i \
     WHERE i.producer = 'http' AND a.tenant_id = i.tenant_id \
       AND a.catalog_id = i.catalog_id AND a.environment = i.environment \
       AND a.attachment_id = i.attachment_id \
       AND a.principal_digest = i.principal_digest \
       AND i.client_key_digest IS NOT NULL \
       AND a.client_key_digest = i.client_key_digest \
     FOR KEY SHARE OF a \
), \
existing_identity_run AS MATERIALIZED ( \
    SELECT r.* FROM wamn_run.runs AS r, input AS i \
     WHERE r.tenant_id = i.tenant_id \
       AND (r.run_id = COALESCE((SELECT run_id FROM existing_http), i.run_id) \
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
      WHEN i.response_deadline_at IS NOT NULL AND i.run_deadline_at IS NOT NULL \
        AND i.response_deadline_at > i.run_deadline_at THEN 'invalid-input' \
      WHEN i.producer = 'http' AND (i.attachment_id IS NULL \
        OR i.attachment_id = '' OR i.expected_definition_hash IS NULL \
        OR i.expected_definition_hash = '' OR i.principal_digest IS NULL \
        OR i.principal_digest = '' OR i.client_key_digest = '' \
        OR i.request_fingerprint IS NULL OR i.request_fingerprint = '' \
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
        OR i.request_fingerprint IS NOT NULL) \
        THEN 'invalid-input' \
      WHEN h.applied_catalog_version IS NULL THEN 'head-not-found' \
      WHEN h.applied_catalog_version <> i.expected_catalog_version THEN 'head-drift' \
      WHEN rf.flow_id IS NULL THEN 'definition-drift' \
      WHEN rp.execution_bundle_hash IS NULL THEN 'missing-root-plan' \
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
         sha256(convert_to($20::text, 'UTF8')), 'hex')) \
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
      WHEN xr.run_id IS NOT NULL \
       AND (xr.trigger_source <> i.producer \
         OR xr.flow_id <> i.flow_id OR xr.flow_version <> i.flow_version \
         OR xr.catalog_id IS DISTINCT FROM i.catalog_id \
         OR xr.catalog_version IS DISTINCT FROM i.expected_catalog_version \
         OR xr.environment IS DISTINCT FROM i.environment \
         OR xr.execution_bundle_hash IS DISTINCT FROM rp.execution_bundle_hash \
         OR xr.attachment_id IS DISTINCT FROM i.attachment_id \
         OR xr.registration_id IS DISTINCT FROM i.registration_id \
         OR xr.event_source_run_id IS DISTINCT FROM i.event_source_run_id \
         OR xr.event_root_run_id IS DISTINCT FROM i.event_root_run_id \
         OR xr.event_depth IS DISTINCT FROM i.event_depth \
         OR xr.capture_mode IS DISTINCT FROM 'off' \
         OR xr.input_json IS DISTINCT FROM i.input_json) \
        THEN 'conflicting-run-identity' \
      WHEN i.producer = 'http' AND eh.run_id IS NOT NULL THEN 'duplicate' \
      WHEN xr.run_id IS NOT NULL THEN 'duplicate' \
      ELSE 'ready' END AS result_code, \
      i.*, rp.artifact_hash, rp.execution_bundle_hash, \
      eh.run_id AS admitted_run_id, xr.run_id AS existing_run_id \
    FROM input AS i \
    LEFT JOIN locked_head AS h ON true \
    LEFT JOIN active_definition AS d ON true \
    LEFT JOIN live_registration AS er ON true \
    LEFT JOIN source_lineage AS sl ON true \
    LEFT JOIN release_flow AS rf ON true \
    LEFT JOIN root_plan AS rp ON true \
    LEFT JOIN existing_http AS eh ON true \
    LEFT JOIN existing_identity_run AS xr ON true \
), \
created_http AS ( \
    INSERT INTO wamn_run.invocation_admissions \
      (tenant_id, catalog_id, environment, attachment_id, definition_hash, \
       principal_digest, client_key_digest, client_request_fingerprint, \
       admitted_catalog_version, admitted_flow_version, run_id) \
    SELECT c.tenant_id, c.catalog_id, c.environment, c.attachment_id, \
           c.expected_definition_hash, c.principal_digest, c.client_key_digest, \
           c.request_fingerprint, c.expected_catalog_version, c.flow_version, \
           c.run_id \
      FROM classified AS c \
     WHERE c.producer = 'http' AND c.result_code = 'ready' \
    ON CONFLICT ON CONSTRAINT invocation_admissions_identity DO NOTHING \
    RETURNING tenant_id, run_id \
), \
created_run AS ( \
    INSERT INTO wamn_run.runs \
      (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment, \
       execution_bundle_hash, attachment_id, registration_id, status, trigger_source, input_json, \
       event_source_run_id, event_root_run_id, event_depth, \
       invocation_context, admission_context_version, platform_revision, idempotency_key, \
       response_deadline_at, run_deadline_at) \
    SELECT c.tenant_id, c.run_id, c.flow_id, c.flow_version, c.catalog_id, \
           c.expected_catalog_version, c.environment, \
           c.execution_bundle_hash, \
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
      (tenant_id, run_id, available_at, stream_seq) \
    SELECT r.tenant_id, r.run_id, now(), \
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

/// Admit one private management run under its stable producer coordinate.
///
/// Parameters:
///
/// 1. producer (`draft-run | test-case`)
/// 2. catalog id
/// 3. environment
/// 4. expected catalog version
/// 5. flow id
/// 6. flow version
/// 7. run id
/// 8. input JSON text
/// 9. invocation-context JSON text
/// 10. platform revision
/// 11. run deadline
/// 12. authoring command id (draft-run)
/// 13. report id (test case)
/// 14. case ordinal (test case)
///
/// The producer key, `trigger_source`, and `capture_mode` are DERIVED from
/// parameter 1 rather than presented, so no caller can admit a draft-only
/// capture value or an unpaired producer marker. The run id stays a parameter
/// because the control-plane reservation records it BEFORE this transaction; a
/// crash between the two commits is recovered by re-executing this statement,
/// which returns the same run under the same key.
///
/// SR12a — what the pure tests cannot cover. Neither existence lookup takes a
/// row lock. `FOR KEY SHARE` on `runs` would oblige this authority to hold
/// `UPDATE` on at least one of its columns, which `wamn-0h0g.7.5` denies it, so
/// the run primary key and the partial unique index over `idempotency_key` are
/// the whole enforcement: a losing concurrent admission conflicts out of
/// `created_run` and creates nothing.
///
/// A loser reports `duplicate` with NO run id, exactly as the callable
/// statement does. It cannot report `c.run_id`: the run id and the producer key
/// are INDEPENDENT parameters here, so a stale snapshot can conflict on the key
/// alone, and answering with the presented id would hand a caller an id no run
/// carries. Its retry sees the committed row and classifies deterministically.
/// Only the live PostgreSQL race proof observes any of this.
///
/// ARM ORDER — IDENTITY BEFORE DRIFT, the reverse of `admit_sql`, and
/// deliberately so (`wamn-0h0g.15.160`). Do not "fix" the asymmetry: the two
/// statements answer for different producers. A management producer replays the
/// parameters a durable control reservation froze BEFORE the run existed, so
/// its `expected_catalog_version` is the one its run already carries, not the
/// one the world now holds. Drift-first turned an exact retry into `head-drift`
/// the moment any publish moved the head, and took with it the run id a crashed
/// producer never recorded. The drift arms exist to stop NEW work entering
/// against a stale view, and an exact retry enters nothing new — the run
/// already exists — so the honest question order is "is this the same command?"
/// before "is this new work coherent with the world?". No identity arm can
/// reach `ready`, so this order can only turn a refusal into the stored answer;
/// it can never admit anything past a moved head.
///
/// The divergence check's one head-derived operand — the root bundle resolved
/// at the LIVE head — is therefore gated on the head still standing where the
/// caller named it, which is exactly the reachability the four drift arms used
/// to guarantee it. Ungated, the reorder would merely retype the same false
/// refusal as `conflicting-run-identity`. Every other operand is a presented
/// parameter, so the check stays a question about the command alone.
fn management_admit_sql() -> String {
    "\
WITH input AS ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           $1::text AS producer, $2::text AS catalog_id, $3::text AS environment, \
           $4::int AS expected_catalog_version, $5::text AS flow_id, \
           $6::int AS flow_version, $7::text AS run_id, \
           $8::text::jsonb AS input_json, $9::text::jsonb AS invocation_context, \
           $10::text AS platform_revision, $11::timestamptz AS run_deadline_at, \
           $12::text AS command_id, $13::text AS report_id, $14::int AS case_ordinal \
), \
keyed AS ( \
    SELECT i.*, \
           CASE i.producer \
             WHEN 'draft-run' THEN 'draft:' || i.command_id \
             WHEN 'test-case' THEN 'case:' || i.report_id || ':' || i.case_ordinal::text \
           END AS producer_key, \
           CASE i.producer \
             WHEN 'draft-run' THEN 'scenario-draft'::text \
             WHEN 'test-case' THEN 'test-case'::text \
           END AS trigger_source, \
           CASE i.producer \
             WHEN 'draft-run' THEN 'full'::text \
             WHEN 'test-case' THEN 'off'::text \
           END AS capture_mode \
      FROM input AS i \
), \
locked_head AS MATERIALIZED ( \
    SELECT h.applied_catalog_version \
      FROM catalog.catalog_heads AS h, keyed AS k \
     WHERE h.tenant_id = k.tenant_id AND h.catalog_id = k.catalog_id \
       AND h.environment = k.environment \
), \
release_flow AS MATERIALIZED ( \
    SELECT f.tenant_id, f.flow_id, f.flow_version, f.execution_bundle_hash, \
           a.artifact_hash \
      FROM catalog.release_flows AS f \
      JOIN catalog.flow_artifacts AS a \
        ON a.tenant_id = f.tenant_id AND a.flow_id = f.flow_id \
       AND a.flow_version = f.flow_version \
      CROSS JOIN keyed AS k CROSS JOIN locked_head AS h \
     WHERE f.tenant_id = k.tenant_id AND f.catalog_id = k.catalog_id \
       AND f.catalog_version = h.applied_catalog_version \
       AND f.flow_id = k.flow_id AND f.flow_version = k.flow_version \
), \
root_plan AS MATERIALIZED ( \
    SELECT rf.* \
      FROM release_flow AS rf \
      JOIN catalog.execution_bundles AS bundle \
        ON bundle.tenant_id = rf.tenant_id \
       AND bundle.execution_bundle_hash = rf.execution_bundle_hash \
), \
keyed_run AS MATERIALIZED ( \
    SELECT r.run_id FROM wamn_run.runs AS r, keyed AS k \
     WHERE r.tenant_id = k.tenant_id AND k.producer_key IS NOT NULL \
       AND r.idempotency_key = k.producer_key \
), \
existing_run AS MATERIALIZED ( \
    SELECT r.run_id, r.idempotency_key, r.trigger_source, r.capture_mode, \
           r.flow_id, r.flow_version, r.catalog_id, r.catalog_version, \
           r.environment, r.execution_bundle_hash, r.input_json \
      FROM wamn_run.runs AS r, keyed AS k \
     WHERE r.tenant_id = k.tenant_id AND r.run_id = k.run_id \
), \
classified AS ( \
    SELECT CASE \
      WHEN k.producer IS NULL OR k.producer NOT IN ('draft-run', 'test-case') \
        THEN 'invalid-producer' \
      WHEN k.tenant_id IS NULL OR k.catalog_id IS NULL OR k.catalog_id = '' \
        OR k.environment IS NULL OR k.environment = '' \
        OR k.flow_id IS NULL OR k.flow_id = '' OR k.flow_version IS NULL \
        OR k.flow_version <= 0 OR k.run_id IS NULL OR k.run_id = '' \
        OR k.input_json IS NULL OR k.invocation_context IS NULL \
        OR jsonb_typeof(k.invocation_context) IS DISTINCT FROM 'object' \
        OR k.platform_revision IS NULL OR k.platform_revision = '' \
        OR k.run_deadline_at IS NULL THEN 'invalid-input' \
      WHEN k.producer = 'draft-run' AND (k.command_id IS NULL OR k.command_id = '' \
        OR k.report_id IS NOT NULL OR k.case_ordinal IS NOT NULL) \
        THEN 'invalid-input' \
      WHEN k.producer = 'test-case' AND (k.report_id IS NULL OR k.report_id = '' \
        OR k.case_ordinal IS NULL OR k.case_ordinal < 0 \
        OR k.command_id IS NOT NULL \
        OR k.invocation_context ->> 'producer' = 'draft-scenario') \
        THEN 'invalid-input' \
      WHEN kr.run_id IS NOT NULL AND kr.run_id <> k.run_id \
        THEN 'conflicting-run-identity' \
      WHEN xr.run_id IS NOT NULL \
       AND (xr.idempotency_key IS DISTINCT FROM k.producer_key \
         OR xr.trigger_source IS DISTINCT FROM k.trigger_source \
         OR xr.capture_mode IS DISTINCT FROM k.capture_mode \
         OR xr.flow_id <> k.flow_id OR xr.flow_version <> k.flow_version \
         OR xr.catalog_id IS DISTINCT FROM k.catalog_id \
         OR xr.catalog_version IS DISTINCT FROM k.expected_catalog_version \
         OR xr.environment IS DISTINCT FROM k.environment \
         OR (plan.execution_bundle_hash IS NOT NULL \
           AND h.applied_catalog_version = k.expected_catalog_version \
           AND xr.execution_bundle_hash IS DISTINCT FROM plan.execution_bundle_hash) \
         OR xr.input_json IS DISTINCT FROM k.input_json) \
        THEN 'conflicting-run-identity' \
      WHEN xr.run_id IS NOT NULL THEN 'duplicate' \
      WHEN h.applied_catalog_version IS NULL THEN 'head-not-found' \
      WHEN h.applied_catalog_version <> k.expected_catalog_version THEN 'head-drift' \
      WHEN rf.flow_id IS NULL THEN 'definition-drift' \
      WHEN plan.execution_bundle_hash IS NULL THEN 'missing-root-plan' \
      ELSE 'ready' END AS result_code, \
      k.*, plan.artifact_hash, plan.execution_bundle_hash, \
      COALESCE(xr.run_id, kr.run_id) AS existing_run_id \
    FROM keyed AS k \
    LEFT JOIN locked_head AS h ON true \
    LEFT JOIN release_flow AS rf ON true \
    LEFT JOIN root_plan AS plan ON true \
    LEFT JOIN keyed_run AS kr ON true \
    LEFT JOIN existing_run AS xr ON true \
), \
created_run AS ( \
    INSERT INTO wamn_run.runs \
      (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment, \
       execution_bundle_hash, status, trigger_source, capture_mode, input_json, \
       invocation_context, admission_context_version, platform_revision, idempotency_key, \
       run_deadline_at) \
    SELECT c.tenant_id, c.run_id, c.flow_id, c.flow_version, c.catalog_id, \
           c.expected_catalog_version, c.environment, \
           c.execution_bundle_hash, 'dispatched', c.trigger_source, c.capture_mode, \
           c.input_json, \
           jsonb_build_object( \
             'version', '0.1', \
             'principal', jsonb_build_object( \
               'tenant-id', c.tenant_id, 'environment', c.environment, \
               'catalog-id', c.catalog_id, 'catalog-version', c.expected_catalog_version, \
               'run-id', c.run_id, \
               'flow-id', c.flow_id, 'flow-version', c.flow_version, \
               'artifact-digest', c.artifact_hash), \
             'source', CASE WHEN c.producer = 'draft-run' \
               THEN jsonb_set(c.invocation_context, '{producer}', \
                              to_jsonb('draft-scenario'::text), true) \
               ELSE c.invocation_context END), \
           '0.1', c.platform_revision, c.producer_key, c.run_deadline_at \
      FROM classified AS c WHERE c.result_code = 'ready' \
    ON CONFLICT DO NOTHING \
    RETURNING tenant_id, run_id \
), \
created_queue AS ( \
    INSERT INTO wamn_run.run_queue \
      (tenant_id, run_id, available_at, stream_seq) \
    SELECT r.tenant_id, r.run_id, now(), 0 \
      FROM created_run AS r \
    RETURNING tenant_id, run_id \
) \
SELECT CASE \
         WHEN c.result_code = 'ready' AND q.run_id IS NOT NULL THEN 'admitted' \
         WHEN c.result_code = 'ready' THEN 'duplicate' \
         ELSE c.result_code END AS result_code, \
       CASE WHEN c.result_code = 'ready' AND q.run_id IS NULL THEN NULL \
         ELSE COALESCE(q.run_id, c.existing_run_id) END AS run_id \
 FROM classified AS c LEFT JOIN created_queue AS q USING (tenant_id, run_id)"
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
        assert_eq!(AdmissionProducer::DraftRun.as_sql(), "draft-run");
        assert_eq!(AdmissionProducer::TestCase.as_sql(), "test-case");
        assert!(
            admission_sql()
                .admit()
                .contains("producer NOT IN ('http', 'event')")
        );
        assert!(
            management_admission_sql()
                .admit()
                .contains("producer NOT IN ('draft-run', 'test-case')")
        );
    }

    #[test]
    fn callable_admission_has_no_expiry_and_allows_an_absent_http_client_key() {
        let sql = admission_sql().admit;

        assert!(!sql.contains("admission_expires_at"));
        assert!(sql.contains("$24"));
        assert!(!sql.contains("$25"));
        assert!(!sql.contains("executor_id"));
        assert!(!sql.contains("lease_ttl_ms"));
        assert!(sql.contains("OR i.client_key_digest = ''"));
        assert!(!sql.contains("OR i.client_key_digest IS NULL"));
        assert!(sql.contains("AND i.client_key_digest IS NOT NULL"));
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
            canonical_references, 7,
            "update this pin when admission adds a run-state boundary"
        );
        assert_eq!(configured_sql.matches("\"wamn_runner_demo\".").count(), 7);
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
        assert!(sql.contains("SELECT r.tenant_id, r.run_id, now()"));
        assert!(!sql.contains("UPDATE wamn_run.run_queue"));
        assert!(sql.contains("INSERT INTO wamn_run.invocation_admissions"));
        assert!(sql.contains("FROM created_run AS r"));
        assert!(sql.contains("EXISTS (SELECT 1 FROM created_http)"));
        assert!(ddl.contains("DEFERRABLE INITIALLY DEFERRED"));
        assert_ne!(recipe.lock_head(), recipe.admit());
    }

    #[test]
    fn callable_admission_forces_capture_off_without_new_input() {
        let sql = admission_sql().admit().to_string();
        assert!(!sql.contains("$25"));
        assert!(!sql.contains("event_source_run_id, event_root_run_id, event_depth, capture_mode"));
        assert!(
            sql.contains(
                "CASE WHEN c.producer = 'event' THEN c.event_depth END, jsonb_build_object"
            )
        );
        assert!(sql.contains("xr.capture_mode IS DISTINCT FROM 'off'"));
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
    fn admission_derives_root_bundle_from_authoritative_member() {
        let recipe = admission_sql();
        let sql = recipe.admit();

        assert!(sql.contains("f.execution_bundle_hash"));
        assert!(sql.contains("JOIN catalog.execution_bundles AS bundle"));
        assert!(sql.contains("bundle.execution_bundle_hash = rf.execution_bundle_hash"));
        assert!(sql.contains("execution_bundle_hash, attachment_id"));
        assert!(sql.contains("xr.execution_bundle_hash IS DISTINCT FROM rp.execution_bundle_hash"));
        assert!(sql.contains("THEN 'missing-root-plan'"));
    }

    #[test]
    fn producer_specific_checks_and_unleased_queue_state_are_pinned() {
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
        assert!(sql.contains("(tenant_id, run_id, available_at, stream_seq)"));
        assert!(!sql.contains("lease_owner"));
        assert!(!sql.contains("lease_expires_at"));
        assert!(!sql.contains("lease_generation"));
        assert!(sql.contains("THEN c.event_seq ELSE 0 END"));
        assert!(sql.contains("'evt:' || c.registration_id"));
        assert!(!sql.contains("partition_key"));
        assert!(!sql.contains("partition_policy"));
        assert!(!sql.contains("existing_queue"));
        assert!(sql.contains("sl.depth + 1 <> i.event_depth"));
        assert!(sql.contains("xr.event_root_run_id IS DISTINCT FROM i.event_root_run_id"));
    }

    // Enablement is environment-owned operational state, and the admission
    // transaction is its enforcement point: the classifier's operand is bound
    // to `catalog.active_attachments`, which exists only to filter on
    // `catalog.attachment_activation.enabled`. Route bytes may move to a
    // mounted manifest, but this refusal may not become a document field, a
    // cached overlay, or an admission parameter — an emergency off has to stay
    // one `UPDATE` effective at the next admission.
    #[test]
    fn attachment_enablement_stays_a_live_catalog_read_inside_the_admission_statement() {
        let sql = admission_sql().admit().to_string();
        let catalog_ddl = include_str!("../../../../deploy/sql/catalog-schema.sql");

        assert!(sql.contains("active_definition AS MATERIALIZED"));
        assert!(sql.contains("FROM catalog.active_attachments AS a"));
        assert!(sql.contains("LEFT JOIN active_definition AS d ON true"));
        assert!(sql.contains(
            "WHEN i.producer = 'http' AND d.attachment_id IS NULL THEN 'inactive-definition'"
        ));

        assert!(catalog_ddl.contains("CREATE VIEW catalog.active_attachments"));
        assert!(catalog_ddl.contains("JOIN catalog.attachment_activation activation"));
        assert!(
            catalog_ddl.contains("WHERE activation.enabled"),
            "catalog.active_attachments must remain the enablement filter"
        );

        // Enablement enters through that relation, never as admission input.
        assert!(!sql.contains("enabled"));
        assert_eq!(
            AdmissionResult::from_parts("inactive-definition", None),
            Some(AdmissionResult::InactiveDefinition)
        );
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

    #[test]
    fn missing_root_plan_decodes_as_distinct_admission_refusal() {
        assert_eq!(
            AdmissionResult::from_parts("missing-root-plan", None),
            Some(AdmissionResult::MissingRootPlan)
        );
    }

    #[test]
    fn management_admission_shares_the_head_lock_and_the_refusal_vocabulary() {
        let callable = admission_sql();
        let management = management_admission_sql();

        assert_eq!(callable.lock_head(), management.lock_head());
        assert_ne!(callable.admit(), management.admit());
        assert!(
            management
                .lock_head()
                .contains("wamn_run.lock_catalog_head")
        );
        assert_eq!(management.admit().matches("WITH input AS").count(), 1);
        // Management reuses the existing typed refusals; it adds no wire arm.
        for code in [
            "invalid-producer",
            "invalid-input",
            "head-not-found",
            "head-drift",
            "definition-drift",
            "missing-root-plan",
            "conflicting-run-identity",
            "duplicate",
            "admitted",
        ] {
            assert!(
                management.admit().contains(code),
                "management admission dropped {code}"
            );
            assert!(
                AdmissionResult::from_parts(code, Some("r1".to_string())).is_some(),
                "{code} does not decode"
            );
        }
    }

    #[test]
    fn management_producers_receive_the_ratified_capture_and_trigger_facts() {
        assert_eq!(
            AdmissionProducer::DraftRun.trigger_source(),
            "scenario-draft"
        );
        assert_eq!(AdmissionProducer::TestCase.trigger_source(), "test-case");
        assert_eq!(AdmissionProducer::Http.trigger_source(), "http");
        assert_eq!(AdmissionProducer::Event.trigger_source(), "event");
        assert_eq!(
            AdmissionProducer::DraftRun.capture_mode(),
            CaptureMode::Full
        );
        for off in [
            AdmissionProducer::Http,
            AdmissionProducer::Event,
            AdmissionProducer::TestCase,
        ] {
            assert_eq!(off.capture_mode(), CaptureMode::Off, "{off:?} stays off");
        }

        let sql = management_admission_sql().admit;
        assert!(sql.contains(&format!(
            "WHEN 'draft-run' THEN '{}'::text",
            AdmissionProducer::DraftRun.trigger_source()
        )));
        assert!(sql.contains(&format!(
            "WHEN 'test-case' THEN '{}'::text",
            AdmissionProducer::TestCase.trigger_source()
        )));
        assert!(sql.contains(&format!(
            "WHEN 'draft-run' THEN '{}'::text",
            CaptureMode::Full.as_str()
        )));
        assert!(sql.contains(&format!(
            "WHEN 'test-case' THEN '{}'::text",
            CaptureMode::Off.as_str()
        )));
        // Capture is derived, never bound: the parameter list stops at the
        // producer coordinate.
        assert!(sql.contains("$14::int AS case_ordinal"));
        assert!(!sql.contains("$15"));
        assert!(sql.contains("END AS capture_mode"));
        assert!(!sql.contains("::text AS capture_mode"));
        // The DDL pairs a full-capture run with exactly this trigger literal.
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        assert!(ddl.contains(&format!(
            "capture_mode <> 'full' OR trigger_source IS NOT DISTINCT FROM '{}'",
            AdmissionProducer::DraftRun.trigger_source()
        )));
    }

    #[test]
    fn management_producer_keys_are_the_stable_admitted_coordinates() {
        let draft = ManagementProducerKey::DraftRun {
            command_id: "cmd-1",
        };
        let case = ManagementProducerKey::TestCase {
            report_id: "report-a",
            ordinal: 3,
        };
        let next = ManagementProducerKey::TestCase {
            report_id: "report-a",
            ordinal: 4,
        };

        assert_eq!(draft.idempotency_key(), "draft:cmd-1");
        assert_eq!(case.idempotency_key(), "case:report-a:3");
        assert_eq!(next.idempotency_key(), "case:report-a:4");
        assert_eq!(draft.producer(), AdmissionProducer::DraftRun);
        assert_eq!(case.producer(), AdmissionProducer::TestCase);
        // Another ordinal is another coordinate, and no management coordinate
        // can shadow the event producer's key space.
        assert_ne!(case.idempotency_key(), next.idempotency_key());
        for key in [draft.idempotency_key(), case.idempotency_key()] {
            assert!(!key.starts_with("evt:"), "{key} collides with event keys");
        }

        let sql = management_admission_sql().admit;
        assert!(sql.contains("WHEN 'draft-run' THEN 'draft:' || i.command_id"));
        assert!(
            sql.contains("WHEN 'test-case' THEN 'case:' || i.report_id || ':' || i.case_ordinal")
        );
        // The key is persisted, so the unique index — not a read — enforces it,
        // and a crashed producer's reconciler can recover the run id from it.
        assert!(sql.contains("c.producer_key, c.run_deadline_at"));
        assert!(sql.contains("r.idempotency_key = k.producer_key"));
        assert!(
            include_str!("../../../../deploy/sql/run-state.sql")
                .contains("CREATE UNIQUE INDEX runs_idempotency ON wamn_run.runs")
        );
    }

    #[test]
    fn management_admission_writes_one_ordinary_run_and_unleased_queue_row() {
        let sql = management_admission_sql().admit;

        assert!(sql.contains("INSERT INTO wamn_run.runs"));
        assert!(sql.contains("status, trigger_source, capture_mode, input_json"));
        assert!(sql.contains("'dispatched', c.trigger_source, c.capture_mode"));
        assert!(sql.contains("INSERT INTO wamn_run.run_queue"));
        assert!(sql.contains("(tenant_id, run_id, available_at, stream_seq)"));
        assert!(sql.contains("SELECT r.tenant_id, r.run_id, now(), 0"));
        assert!(sql.contains("ON CONFLICT DO NOTHING"));
        assert_eq!(sql.matches("INSERT INTO").count(), 2);
        // The authority admits; it never claims, leases, ledgers, or mutates.
        for absent in [
            "invocation_admissions",
            "lease_owner",
            "lease_expires_at",
            "lease_generation",
            "UPDATE wamn_run.",
            "DELETE FROM",
            "FOR KEY SHARE",
            "FOR UPDATE",
            "effect_attempts",
            "operator_run_actions",
            "SECURITY DEFINER",
        ] {
            assert!(
                !sql.contains(absent),
                "management admission must not use {absent}"
            );
        }
    }

    #[test]
    fn management_admission_refuses_a_diverged_coordinate_before_mutation() {
        let sql = management_admission_sql().admit;

        assert!(sql.contains("WHEN kr.run_id IS NOT NULL AND kr.run_id <> k.run_id"));
        for compared in [
            "xr.idempotency_key IS DISTINCT FROM k.producer_key",
            "xr.trigger_source IS DISTINCT FROM k.trigger_source",
            "xr.capture_mode IS DISTINCT FROM k.capture_mode",
            "xr.flow_id <> k.flow_id OR xr.flow_version <> k.flow_version",
            "xr.catalog_id IS DISTINCT FROM k.catalog_id",
            "xr.catalog_version IS DISTINCT FROM k.expected_catalog_version",
            "xr.environment IS DISTINCT FROM k.environment",
            "xr.execution_bundle_hash IS DISTINCT FROM plan.execution_bundle_hash",
            "xr.input_json IS DISTINCT FROM k.input_json",
        ] {
            assert!(sql.contains(compared), "conflict check missing {compared}");
        }
        assert_eq!(sql.matches("THEN 'conflicting-run-identity'").count(), 2);
        assert!(sql.contains("WHEN xr.run_id IS NOT NULL THEN 'duplicate'"));
        // Both writes are fenced behind the classifier, so a refusal is atomic.
        assert!(sql.contains("FROM classified AS c WHERE c.result_code = 'ready'"));
        assert!(sql.contains("FROM created_run AS r"));
        // A refusal names the run already holding the coordinate. A ready
        // admission that LOST the insert race answers with no run id at all,
        // because run id and producer key are independent parameters and a
        // caller must never record an id no run carries.
        assert!(sql.contains("CASE WHEN c.result_code = 'ready' AND q.run_id IS NULL THEN NULL"));
        assert!(sql.contains("ELSE COALESCE(q.run_id, c.existing_run_id) END AS run_id"));
        assert!(!sql.contains("c.admitted_run_id"));
        assert_eq!(
            AdmissionResult::from_parts("conflicting-run-identity", Some("r1".to_string())),
            Some(AdmissionResult::ConflictingRunIdentity)
        );
    }

    /// The callable and management classifiers ask their questions in OPPOSITE
    /// orders, and `wamn-0h0g.15.160` ruled that divergence deliberate. A
    /// management producer replays parameters frozen before its run existed, so
    /// a publish must not turn its exact retry into a drift refusal. A callable
    /// retry that must replay an outcome never reaches its statement at all.
    #[test]
    fn an_exact_management_retry_is_answered_before_the_world_is_rechecked() {
        let management = management_admission_sql().admit;
        let callable = admission_sql().admit;
        let duplicate_arm = "WHEN xr.run_id IS NOT NULL THEN 'duplicate'";
        let head_arm = "WHEN h.applied_catalog_version IS NULL THEN 'head-not-found'";

        let management_duplicate = management.find(duplicate_arm).expect("duplicate arm");
        let management_head = management.find(head_arm).expect("management head arm");
        assert!(
            management_duplicate < management_head,
            "the management identity arms must precede its drift arms"
        );

        let callable_duplicate = callable.find(duplicate_arm).expect("callable duplicate arm");
        let callable_head = callable.find(head_arm).expect("callable head arm");
        assert!(
            callable_head < callable_duplicate,
            "the callable statement's drift-first order is unchanged"
        );

        // The divergence check's only head-derived operand stays inert unless
        // the head still stands where the caller named it. Ungated, the reorder
        // would merely retype the same refusal as a conflicting identity.
        assert!(management.contains(
            "OR (plan.execution_bundle_hash IS NOT NULL \
             AND h.applied_catalog_version = k.expected_catalog_version \
             AND xr.execution_bundle_hash IS DISTINCT FROM plan.execution_bundle_hash)"
        ));
    }

    /// The effect claim path authorizes a draft-safe connection only when
    /// `runs.trigger_source = 'scenario-draft'` and
    /// `invocation_context #>> '{source,producer}' = 'draft-scenario'` AGREE
    /// (`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs`). A
    /// half-set pair silently refuses every draft effect, so admission writes
    /// the marker itself and refuses a test case that presents it.
    #[test]
    fn management_admission_owns_the_draft_scenario_marker() {
        let sql = management_admission_sql().admit;

        assert!(sql.contains(
            "'source', CASE WHEN c.producer = 'draft-run' \
             THEN jsonb_set(c.invocation_context, '{producer}', \
             to_jsonb('draft-scenario'::text), true) \
             ELSE c.invocation_context END)"
        ));
        assert!(sql.contains("k.invocation_context ->> 'producer' = 'draft-scenario'"));
        assert!(sql.contains("jsonb_typeof(k.invocation_context) IS DISTINCT FROM 'object'"));
    }

    #[test]
    fn callable_admission_is_unchanged_by_management_admission() {
        let callable = admission_sql().admit;

        assert!(callable.contains("producer NOT IN ('http', 'event')"));
        for management_only in [
            "draft-run",
            "test-case",
            "scenario-draft",
            "producer_key",
            "command_id",
            "case_ordinal",
        ] {
            assert!(
                !callable.contains(management_only),
                "callable admission leaked {management_only}"
            );
        }
        // `wamn_app` holds no INSERT privilege on `capture_mode`, and PostgreSQL
        // checks column privileges per NAMED column, so the callable statement
        // must never name it in its run insert.
        assert!(callable.contains("registration_id, status, trigger_source, input_json"));
        assert!(!callable.contains("trigger_source, capture_mode"));
        // The guests binding this statement positionally fix its parameter count.
        assert!(callable.contains("$24"));
        assert!(!callable.contains("$25"));
    }

    #[test]
    fn custom_management_schema_qualifies_every_run_state_reference() {
        let schema = RunStateSchema::new("wamn_runner_demo").unwrap();
        let canonical = management_admission_sql();
        let configured = management_admission_sql_for_schema(&schema);
        let canonical_references = canonical.lock_head.matches("wamn_run.").count()
            + canonical.admit.matches("wamn_run.").count();
        let configured_sql = format!("{} {}", configured.lock_head, configured.admit);

        assert_eq!(
            canonical_references, 5,
            "update this pin when management admission adds a run-state boundary"
        );
        assert_eq!(configured_sql.matches("\"wamn_runner_demo\".").count(), 5);
        assert!(!configured_sql.contains("wamn_run."));
        assert_eq!(
            admission_transaction(AdmissionTransition::ManagementRun { schema: &schema }),
            configured
        );
        assert_eq!(
            management_admission_sql_for_schema(&RunStateSchema::default()),
            canonical
        );
    }
}
