//! Private management admission.
//!
//! Management enters the run plane through the same-transaction recipe returned
//! by [`management_admission_transaction`]. Its first ordinary statement takes
//! the stable catalog-head share lock; its second rechecks the management facts
//! and writes the run and queue row atomically. Hot HTTP and stream ingress no
//! longer create durable runs, so there is no callable admission dialect.

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

    /// Return the schema name before PostgreSQL identifier quoting.
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

/// Producer variant accepted by private management admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionProducer {
    DraftRun,
    TestCase,
}

impl AdmissionProducer {
    /// Return the stable SQL literal for this producer.
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::DraftRun => "draft-run",
            Self::TestCase => "test-case",
        }
    }

    /// Return the `runs.trigger_source` literal this producer admits under.
    pub const fn trigger_source(self) -> &'static str {
        match self {
            Self::DraftRun => "scenario-draft",
            Self::TestCase => "test-case",
        }
    }

    /// Return the capture value derived from this trusted producer.
    pub const fn capture_mode(self) -> CaptureMode {
        match self {
            Self::DraftRun => CaptureMode::Full,
            Self::TestCase => CaptureMode::Off,
        }
    }
}

/// Stable coordinate under which one private management admission is idempotent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementProducerKey<'a> {
    DraftRun { command_id: &'a str },
    TestCase { report_id: &'a str, ordinal: i32 },
}

impl ManagementProducerKey<'_> {
    /// Return the producer this coordinate admits as.
    pub const fn producer(self) -> AdmissionProducer {
        match self {
            Self::DraftRun { .. } => AdmissionProducer::DraftRun,
            Self::TestCase { .. } => AdmissionProducer::TestCase,
        }
    }

    /// Compose the exact `runs.idempotency_key` for this coordinate.
    pub fn idempotency_key(self) -> String {
        match self {
            Self::DraftRun { command_id } => format!("draft:{command_id}"),
            Self::TestCase { report_id, ordinal } => format!("case:{report_id}:{ordinal}"),
        }
    }
}

/// Typed result returned by management admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionResult {
    Admitted { run_id: String },
    Duplicate { run_id: Option<String> },
    HeadNotFound,
    HeadDrift,
    InactiveWiring,
    DefinitionDrift,
    MissingRootPlan,
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
            "inactive-wiring" => Some(Self::InactiveWiring),
            "definition-drift" => Some(Self::DefinitionDrift),
            "missing-root-plan" => Some(Self::MissingRootPlan),
            "conflicting-run-identity" => Some(Self::ConflictingRunIdentity),
            "invalid-producer" => Some(Self::InvalidProducer),
            "invalid-input" => Some(Self::InvalidInput),
            _ => None,
        }
    }
}

/// Ordered ordinary statements for one management admission transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionTransaction {
    lock_head: String,
    admit: String,
}

impl AdmissionTransaction {
    /// Execute first to acquire the stable catalog-head lock.
    pub fn lock_head(&self) -> &str {
        &self.lock_head
    }

    /// Execute second in the same transaction to recheck and mutate atomically.
    pub fn admit(&self) -> &str {
        &self.admit
    }
}

/// Compose private management admission for a validated schema.
pub fn management_admission_transaction(schema: &RunStateSchema) -> AdmissionTransaction {
    qualify_run_state_schema(management_admission_sql(), schema)
}

fn management_admission_sql() -> AdmissionTransaction {
    AdmissionTransaction {
        lock_head: lock_current_catalog_head_sql(),
        admit: management_admit_sql(),
    }
}

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

/// Lock the current catalog head under ordinary invoker rights.
fn lock_current_catalog_head_sql() -> String {
    "WITH authority AS MATERIALIZED ( \
         SELECT wamn_run.require_management_admission_authority() AS allowed \
     ) \
     SELECT head.applied_catalog_version \
       FROM authority CROSS JOIN catalog.catalog_heads AS head \
      WHERE authority.allowed \
        AND head.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
        AND head.catalog_id = $1 AND head.environment = $2 \
      FOR SHARE OF head"
        .to_string()
}

/// Admit one draft-run or test-case under the private management authority.
fn management_admit_sql() -> String {
    "\
WITH authority AS MATERIALIZED ( \
    SELECT wamn_run.require_management_admission_authority() AS allowed \
), \
input AS ( \
    SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
           $1::text AS producer, $2::text AS catalog_id, $3::text AS environment, \
           $4::int AS expected_catalog_version, $5::text AS flow_id, \
           $6::int AS flow_version, $7::text AS run_id, \
           $8::text::jsonb AS input_json, $9::text::jsonb AS invocation_context, \
           $10::text AS platform_revision, $11::timestamptz AS run_deadline_at, \
           $12::text AS command_id, $13::text AS report_id, $14::int AS case_ordinal, \
           $15::text AS wiring_id \
      FROM authority WHERE authority.allowed \
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
active_wiring AS MATERIALIZED ( \
    SELECT activation.wiring_id, wiring.version AS wiring_version \
      FROM catalog.wiring_activation AS activation \
      JOIN catalog.wirings AS wiring \
        ON wiring.tenant_id = activation.tenant_id \
       AND wiring.catalog_id = activation.catalog_id \
       AND wiring.wiring_id = activation.wiring_id \
       AND wiring.wiring_hash = activation.confirmed_definition_hash \
      CROSS JOIN keyed AS k \
     WHERE activation.tenant_id = k.tenant_id \
       AND activation.catalog_id = k.catalog_id \
       AND activation.environment = k.environment \
       AND activation.wiring_id = k.wiring_id \
       AND activation.enabled \
       AND NOT EXISTS ( \
           SELECT 1 FROM catalog.wiring_tombstones AS dead \
            WHERE dead.tenant_id = activation.tenant_id \
              AND dead.catalog_id = activation.catalog_id \
              AND dead.environment = activation.environment \
              AND dead.wiring_id = activation.wiring_id) \
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
           r.environment, r.execution_bundle_hash, r.wiring_id, r.wiring_version, r.input_json \
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
        OR k.run_deadline_at IS NULL OR k.wiring_id IS NULL \
        OR k.wiring_id = '' THEN 'invalid-input' \
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
         OR xr.wiring_id IS DISTINCT FROM aw.wiring_id \
         OR xr.wiring_version IS DISTINCT FROM aw.wiring_version \
         OR (plan.execution_bundle_hash IS NOT NULL \
           AND h.applied_catalog_version = k.expected_catalog_version \
           AND xr.execution_bundle_hash IS DISTINCT FROM plan.execution_bundle_hash) \
         OR xr.input_json IS DISTINCT FROM k.input_json) \
        THEN 'conflicting-run-identity' \
      WHEN xr.run_id IS NOT NULL THEN 'duplicate' \
      WHEN h.applied_catalog_version IS NULL THEN 'head-not-found' \
      WHEN h.applied_catalog_version <> k.expected_catalog_version THEN 'head-drift' \
      WHEN aw.wiring_id IS NULL THEN 'inactive-wiring' \
      WHEN rf.flow_id IS NULL THEN 'definition-drift' \
      WHEN plan.execution_bundle_hash IS NULL THEN 'missing-root-plan' \
      ELSE 'ready' END AS result_code, \
      k.*, aw.wiring_version, plan.artifact_hash, plan.execution_bundle_hash, \
      COALESCE(xr.run_id, kr.run_id) AS existing_run_id \
    FROM keyed AS k \
    LEFT JOIN locked_head AS h ON true \
    LEFT JOIN active_wiring AS aw ON true \
    LEFT JOIN release_flow AS rf ON true \
    LEFT JOIN root_plan AS plan ON true \
    LEFT JOIN keyed_run AS kr ON true \
    LEFT JOIN existing_run AS xr ON true \
), \
created_run AS ( \
    INSERT INTO wamn_run.runs \
      (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, environment, \
       execution_bundle_hash, wiring_id, wiring_version, status, trigger_source, capture_mode, input_json, \
       invocation_context, admission_context_version, platform_revision, idempotency_key, \
       run_deadline_at) \
    SELECT c.tenant_id, c.run_id, c.flow_id, c.flow_version, c.catalog_id, \
           c.expected_catalog_version, c.environment, \
           c.execution_bundle_hash, c.wiring_id, c.wiring_version, \
           'dispatched', c.trigger_source, c.capture_mode, \
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
    use super::*;

    #[test]
    fn management_producer_literals_are_closed() {
        assert_eq!(AdmissionProducer::DraftRun.as_sql(), "draft-run");
        assert_eq!(AdmissionProducer::TestCase.as_sql(), "test-case");
        assert_eq!(
            AdmissionProducer::DraftRun.trigger_source(),
            "scenario-draft"
        );
        assert_eq!(
            AdmissionProducer::DraftRun.capture_mode(),
            CaptureMode::Full
        );
        assert_eq!(AdmissionProducer::TestCase.capture_mode(), CaptureMode::Off);
    }

    #[test]
    fn management_coordinates_are_stable() {
        let draft = ManagementProducerKey::DraftRun {
            command_id: "cmd-1",
        };
        let case = ManagementProducerKey::TestCase {
            report_id: "report-1",
            ordinal: 7,
        };
        assert_eq!(draft.idempotency_key(), "draft:cmd-1");
        assert_eq!(case.idempotency_key(), "case:report-1:7");
    }

    #[test]
    fn management_result_vocabulary_is_closed() {
        for code in [
            "head-not-found",
            "head-drift",
            "inactive-wiring",
            "definition-drift",
            "missing-root-plan",
            "conflicting-run-identity",
            "invalid-producer",
            "invalid-input",
        ] {
            assert!(AdmissionResult::from_parts(code, None).is_some(), "{code}");
        }
        for retired in [
            "inactive-definition",
            "registration-not-found",
            "registration-drift",
            "invalid-registration-hash",
            "invalid-event-lineage",
            "idempotency-key-reused",
            "idempotency-scope-changed",
        ] {
            assert_eq!(
                AdmissionResult::from_parts(retired, None),
                None,
                "{retired}"
            );
        }
    }

    #[test]
    fn both_ordinary_statements_assert_current_user_authority() {
        let recipe = management_admission_transaction(&RunStateSchema::default());
        for statement in [recipe.lock_head(), recipe.admit()] {
            assert!(statement.starts_with("WITH authority AS MATERIALIZED"));
            assert_eq!(
                statement
                    .matches("require_management_admission_authority()")
                    .count(),
                1
            );
            assert!(!statement.contains("SECURITY DEFINER"));
            assert!(!statement.contains("wamn_app"));
        }
        assert!(recipe.lock_head().contains("FOR SHARE OF head"));
        assert!(!recipe.lock_head().contains("lock_catalog_head"));
    }

    #[test]
    fn authority_functions_freeze_invoker_identity_and_refusals() {
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        for (function, role, message) in [
            (
                "require_executor_platform_authority",
                "wamn_executor_platform",
                "executor-platform-authority-required",
            ),
            (
                "require_management_admission_authority",
                "wamn_management_admitter",
                "management-admission-authority-required",
            ),
        ] {
            let signature = format!("CREATE FUNCTION wamn_run.{function}()");
            let body = ddl
                .split_once(signature.as_str())
                .unwrap_or_else(|| panic!("missing {function}"))
                .1
                .split_once("$$;")
                .expect("authority function terminator")
                .0;
            assert!(body.contains("SECURITY INVOKER"), "{function}");
            assert!(body.contains("CURRENT_USER"), "{function}");
            assert!(body.contains("pg_has_role"), "{function}");
            assert!(body.contains(role), "{function}");
            assert!(body.contains("ERRCODE = '42501'"), "{function}");
            assert!(body.contains(message), "{function}");
            assert!(!body.contains("SECURITY DEFINER"), "{function}");
            assert!(
                !ddl.contains(&format!(
                    "GRANT EXECUTE ON FUNCTION wamn_run.{function}() TO wamn_app"
                )),
                "function grants must not authorize wamn_app"
            );
        }
    }

    #[test]
    fn alternate_schema_qualifies_both_authority_guards() {
        let schema = RunStateSchema::new("odd schema").unwrap();
        let recipe = management_admission_transaction(&schema);
        for statement in [recipe.lock_head(), recipe.admit()] {
            assert!(statement.contains("\"odd schema\".require_management_admission_authority()"));
            assert!(!statement.contains("wamn_run."));
        }
    }

    #[test]
    fn management_admission_is_one_run_and_one_queue_insert() {
        let sql = management_admission_transaction(&RunStateSchema::default()).admit;
        assert_eq!(sql.matches("INSERT INTO").count(), 2);
        assert!(sql.contains("INSERT INTO wamn_run.runs"));
        assert!(sql.contains("INSERT INTO wamn_run.run_queue"));
        for retired in [
            "invocation_admissions",
            "attachment_id",
            "registration_id",
            "client_key_digest",
            "event_seq",
        ] {
            assert!(!sql.contains(retired), "management SQL retained {retired}");
        }
    }
}
