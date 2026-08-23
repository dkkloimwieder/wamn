//! Private management admission.
//!
//! Management enters the run plane through the same-transaction recipe returned
//! by [`management_admission_transaction`]. Its first ordinary statement
//! serializes one stable producer key; its second derives the exact candidate
//! wiring and non-secret binding world, then writes the run and queue row
//! atomically. Hot HTTP and stream ingress no longer create durable runs, so
//! there is no callable admission dialect.

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
    Admitted {
        run_id: String,
        binding_world_json: serde_json::Value,
    },
    Duplicate {
        run_id: String,
        binding_world_json: serde_json::Value,
    },
    CandidateNotFound,
    CandidateIdentityMismatch,
    GateReportMismatch,
    CandidateDefinitionInvalid,
    BindingWorldUnavailable,
    BindingWorldDrift,
    ConflictingRunIdentity,
    InvalidProducer,
    InvalidInput,
}

impl AdmissionResult {
    /// Decode the transition's `(result_code, run_id, binding_world_json)` row.
    pub fn from_parts(
        code: &str,
        run_id: Option<String>,
        binding_world_json: Option<serde_json::Value>,
    ) -> Option<Self> {
        match code {
            "admitted" => Some(Self::Admitted {
                run_id: run_id?,
                binding_world_json: binding_world_json?,
            }),
            "duplicate" => Some(Self::Duplicate {
                run_id: run_id?,
                binding_world_json: binding_world_json?,
            }),
            "candidate-not-found" => Some(Self::CandidateNotFound),
            "candidate-identity-mismatch" => Some(Self::CandidateIdentityMismatch),
            "gate-report-mismatch" => Some(Self::GateReportMismatch),
            "candidate-definition-invalid" => Some(Self::CandidateDefinitionInvalid),
            "binding-world-unavailable" => Some(Self::BindingWorldUnavailable),
            "binding-world-drift" => Some(Self::BindingWorldDrift),
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
    lock_producer: String,
    admit: String,
}

impl AdmissionTransaction {
    /// Execute first to serialize this producer coordinate in the transaction.
    pub fn lock_producer(&self) -> &str {
        &self.lock_producer
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
        lock_producer: lock_management_producer_sql(),
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
        lock_producer: canonical.lock_producer.replace("wamn_run.", &qualifier),
        admit: canonical.admit.replace("wamn_run.", &qualifier),
    }
}

/// Serialize one producer key before the admission statement takes its snapshot.
fn lock_management_producer_sql() -> String {
    "WITH authority AS MATERIALIZED ( \
         SELECT wamn_run.require_management_admission_authority() AS allowed \
     ), input AS ( \
         SELECT NULLIF(current_setting('app.tenant', true), '')::text AS tenant_id, \
                $1::text AS producer, $2::text AS command_id, \
                $3::text AS report_id, $4::int AS case_ordinal \
           FROM authority WHERE authority.allowed \
     ), keyed AS ( \
         SELECT tenant_id, CASE producer \
                  WHEN 'draft-run' THEN 'draft:' || command_id \
                  WHEN 'test-case' THEN 'case:' || report_id || ':' || case_ordinal::text \
                END AS producer_key \
           FROM input \
     ) \
     SELECT pg_catalog.pg_advisory_xact_lock( \
              pg_catalog.hashtextextended(tenant_id || chr(31) || producer_key, 0)) \
       FROM keyed WHERE tenant_id IS NOT NULL AND producer_key IS NOT NULL"
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
           $4::int AS expected_catalog_version, $5::text AS run_id, \
           $6::text::jsonb AS input_json, $7::text::jsonb AS invocation_context, \
           $8::text AS platform_revision, $9::timestamptz AS run_deadline_at, \
           $10::text AS command_id, $11::text AS report_id, $12::int AS case_ordinal, \
           $13::text AS wiring_id, $14::int AS wiring_version, \
           $15::text AS wiring_hash, $16::text AS gate_report_id, \
           $17::text::jsonb AS prior_binding_world_json \
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
           END AS capture_mode, \
           CASE i.producer \
             WHEN 'draft-run' THEN jsonb_set( \
               i.invocation_context, '{producer}', \
               to_jsonb('draft-scenario'::text), true) \
             ELSE i.invocation_context \
           END AS source_context \
      FROM input AS i \
), \
candidate AS MATERIALIZED ( \
    SELECT wiring.version, wiring.wiring_hash, wiring.gate_report_id, \
           wiring.graph_json \
      FROM catalog.wirings AS wiring CROSS JOIN keyed AS k \
     WHERE wiring.tenant_id = k.tenant_id \
       AND wiring.catalog_id = k.catalog_id \
       AND wiring.gated_catalog_version = k.expected_catalog_version \
       AND wiring.wiring_id = k.wiring_id \
       AND wiring.version = k.wiring_version \
), \
candidate_nodes AS MATERIALIZED ( \
    SELECT node.key AS node_id, component.component_digest, \
           component.component IS NOT NULL AS component_admitted \
      FROM candidate AS candidate CROSS JOIN keyed AS k \
      CROSS JOIN LATERAL jsonb_each( \
        CASE WHEN jsonb_typeof(candidate.graph_json -> 'nodes') = 'object' \
             THEN candidate.graph_json -> 'nodes' ELSE '{}'::jsonb END \
      ) AS node \
      LEFT JOIN catalog.component_library AS component \
        ON component.tenant_id = k.tenant_id \
       AND component.catalog_id = k.catalog_id \
       AND component.catalog_version = k.expected_catalog_version \
       AND component.component = node.value ->> 'component' \
       AND component.interface_version = node.value ->> 'interface-version' \
       AND component.operation = node.value ->> 'operation' \
), \
node_summary AS MATERIALIZED ( \
    SELECT count(*) AS node_count, \
           count(*) FILTER (WHERE NOT component_admitted) AS invalid_node_count \
      FROM candidate_nodes \
), \
requirements AS MATERIALIZED ( \
    SELECT requirement.component_digest, requirement.store_alias, \
           requirement.requirement_hash \
      FROM (SELECT DISTINCT component_digest FROM candidate_nodes \
             WHERE component_admitted) AS selected \
      JOIN catalog.connection_requirements AS requirement \
        ON requirement.tenant_id = (SELECT tenant_id FROM keyed) \
       AND requirement.artifact_hash IS NULL \
       AND requirement.requirement_name IS NULL \
       AND requirement.component_digest = selected.component_digest \
), \
resolved_requirements AS MATERIALIZED ( \
    SELECT requirement.component_digest, requirement.store_alias, \
           requirement.requirement_hash, binding.instance_id, \
           instance.revision AS instance_revision, instance.requirement_type, \
           instance.contract, binding.validation_hash, generation.generation, \
           generation.definition_hash, generation.credential_set_handle \
      FROM requirements AS requirement CROSS JOIN keyed AS k \
      JOIN catalog.connection_bindings AS binding \
        ON binding.tenant_id = k.tenant_id \
       AND binding.catalog_id = k.catalog_id \
       AND binding.catalog_version = k.expected_catalog_version \
       AND binding.artifact_hash IS NULL \
       AND binding.requirement_name IS NULL \
       AND binding.component_digest = requirement.component_digest \
       AND binding.store_alias = requirement.store_alias \
       AND binding.environment = k.environment \
       AND binding.binding_status = 'active' \
       AND binding.validation_status = 'valid' \
      JOIN catalog.connection_instances AS instance \
        ON instance.tenant_id = binding.tenant_id \
       AND instance.environment = binding.environment \
       AND instance.instance_id = binding.instance_id \
       AND instance.lifecycle_status = 'enabled' \
       AND instance.active_generation IS NOT NULL \
      JOIN catalog.connection_generations AS generation \
        ON generation.tenant_id = instance.tenant_id \
       AND generation.environment = instance.environment \
       AND generation.instance_id = instance.instance_id \
       AND generation.generation = instance.active_generation \
), \
binding_world AS MATERIALIZED ( \
    SELECT count(requirement.component_digest) AS requirement_count, \
           count(resolved.component_digest) AS resolved_count, \
           COALESCE(jsonb_agg( \
             jsonb_build_object( \
               'component-digest', resolved.component_digest, \
               'store-alias', resolved.store_alias, \
               'requirement-hash', resolved.requirement_hash, \
               'instance-id', resolved.instance_id, \
               'instance-revision', resolved.instance_revision, \
               'requirement-type', resolved.requirement_type, \
               'contract', resolved.contract, \
               'validation-hash', resolved.validation_hash, \
               'generation', resolved.generation, \
               'definition-hash', resolved.definition_hash, \
               'credential-set-handle', resolved.credential_set_handle \
             ) ORDER BY resolved.component_digest, resolved.store_alias \
           ) FILTER (WHERE resolved.component_digest IS NOT NULL), '[]'::jsonb) \
             AS binding_world_json \
      FROM requirements AS requirement \
      LEFT JOIN resolved_requirements AS resolved \
        USING (component_digest, store_alias) \
), \
expected AS MATERIALIZED ( \
    SELECT k.*, candidate.wiring_hash AS actual_wiring_hash, \
           candidate.gate_report_id AS actual_gate_report_id, \
           candidate.graph_json, node_summary.node_count, \
           node_summary.invalid_node_count, binding_world.requirement_count, \
           binding_world.resolved_count, binding_world.binding_world_json, \
           jsonb_build_object( \
             'version', '0.1', \
             'principal', jsonb_build_object( \
               'tenant-id', k.tenant_id, 'environment', k.environment, \
               'catalog-id', k.catalog_id, \
               'catalog-version', k.expected_catalog_version, \
               'run-id', k.run_id, 'wiring-id', k.wiring_id, \
               'wiring-version', k.wiring_version, \
               'wiring-hash', k.wiring_hash, \
               'gate-report-id', k.gate_report_id), \
             'source', k.source_context) AS admitted_context \
      FROM keyed AS k LEFT JOIN candidate ON true \
      CROSS JOIN node_summary CROSS JOIN binding_world \
), \
keyed_run AS MATERIALIZED ( \
    SELECT r.run_id, r.binding_world_json \
      FROM wamn_run.runs AS r, expected AS e \
     WHERE r.tenant_id = e.tenant_id AND e.producer_key IS NOT NULL \
       AND r.idempotency_key = e.producer_key \
), \
existing_run AS MATERIALIZED ( \
    SELECT r.run_id, r.idempotency_key, r.trigger_source, r.capture_mode, \
           r.catalog_id, r.catalog_version, r.environment, r.wiring_id, \
           r.wiring_version, r.wiring_hash, r.gate_report_id, \
           r.binding_world_json, r.input_json, r.invocation_context, \
           r.platform_revision, r.run_deadline_at \
      FROM wamn_run.runs AS r, expected AS e \
     WHERE r.tenant_id = e.tenant_id AND r.run_id = e.run_id \
), \
classified AS ( \
    SELECT CASE \
      WHEN e.producer IS NULL OR e.producer NOT IN ('draft-run', 'test-case') \
        THEN 'invalid-producer' \
      WHEN e.tenant_id IS NULL OR e.catalog_id IS NULL OR e.catalog_id = '' \
        OR e.environment IS NULL OR e.environment = '' \
        OR e.expected_catalog_version IS NULL OR e.expected_catalog_version <= 0 \
        OR e.run_id IS NULL OR e.run_id = '' OR e.input_json IS NULL \
        OR e.invocation_context IS NULL \
        OR jsonb_typeof(e.invocation_context) IS DISTINCT FROM 'object' \
        OR e.platform_revision IS NULL OR e.platform_revision = '' \
        OR e.run_deadline_at IS NULL OR e.wiring_id IS NULL OR e.wiring_id = '' \
        OR e.wiring_version IS NULL OR e.wiring_version <= 0 \
        OR e.wiring_hash IS NULL \
        OR e.wiring_hash !~ '^sha256:[0-9a-f]{64}$' \
        OR e.gate_report_id IS NULL OR e.gate_report_id = '' \
        OR (e.prior_binding_world_json IS NOT NULL \
            AND jsonb_typeof(e.prior_binding_world_json) IS DISTINCT FROM 'array') \
        THEN 'invalid-input' \
      WHEN e.producer = 'draft-run' AND (e.command_id IS NULL OR e.command_id = '' \
        OR e.report_id IS NOT NULL OR e.case_ordinal IS NOT NULL) \
        THEN 'invalid-input' \
      WHEN e.producer = 'test-case' AND (e.report_id IS NULL OR e.report_id = '' \
        OR e.case_ordinal IS NULL OR e.case_ordinal < 0 \
        OR (e.case_ordinal > 0 AND e.prior_binding_world_json IS NULL) \
        OR e.command_id IS NOT NULL \
        OR e.invocation_context ->> 'producer' = 'draft-scenario') \
        THEN 'invalid-input' \
      WHEN e.producer = 'test-case' \
        AND e.report_id IS DISTINCT FROM e.gate_report_id \
        THEN 'gate-report-mismatch' \
      WHEN kr.run_id IS NOT NULL AND kr.run_id <> e.run_id \
        THEN 'conflicting-run-identity' \
      WHEN xr.run_id IS NOT NULL \
       AND (xr.idempotency_key IS DISTINCT FROM e.producer_key \
         OR xr.trigger_source IS DISTINCT FROM e.trigger_source \
         OR xr.capture_mode IS DISTINCT FROM e.capture_mode \
         OR xr.catalog_id IS DISTINCT FROM e.catalog_id \
         OR xr.catalog_version IS DISTINCT FROM e.expected_catalog_version \
         OR xr.environment IS DISTINCT FROM e.environment \
         OR xr.wiring_id IS DISTINCT FROM e.wiring_id \
         OR xr.wiring_version IS DISTINCT FROM e.wiring_version \
         OR xr.wiring_hash IS DISTINCT FROM e.wiring_hash \
         OR xr.gate_report_id IS DISTINCT FROM e.gate_report_id \
         OR xr.input_json IS DISTINCT FROM e.input_json \
         OR xr.invocation_context IS DISTINCT FROM e.admitted_context \
         OR xr.platform_revision IS DISTINCT FROM e.platform_revision \
         OR xr.run_deadline_at IS DISTINCT FROM e.run_deadline_at) \
        THEN 'conflicting-run-identity' \
      WHEN xr.run_id IS NOT NULL AND e.prior_binding_world_json IS NOT NULL \
        AND xr.binding_world_json IS DISTINCT FROM e.prior_binding_world_json \
        THEN 'binding-world-drift' \
      WHEN xr.run_id IS NOT NULL THEN 'duplicate' \
      WHEN e.actual_wiring_hash IS NULL THEN 'candidate-not-found' \
      WHEN e.actual_wiring_hash IS DISTINCT FROM e.wiring_hash \
        THEN 'candidate-identity-mismatch' \
      WHEN e.actual_gate_report_id IS DISTINCT FROM e.gate_report_id \
        THEN 'gate-report-mismatch' \
      WHEN jsonb_typeof(e.graph_json -> 'nodes') IS DISTINCT FROM 'object' \
        OR e.node_count = 0 OR e.invalid_node_count <> 0 \
        THEN 'candidate-definition-invalid' \
      WHEN e.requirement_count <> e.resolved_count \
        THEN 'binding-world-unavailable' \
      WHEN e.prior_binding_world_json IS NOT NULL \
        AND e.binding_world_json IS DISTINCT FROM e.prior_binding_world_json \
        THEN 'binding-world-drift' \
      ELSE 'ready' END AS result_code, \
      e.*, COALESCE(xr.run_id, kr.run_id) AS existing_run_id, \
      xr.binding_world_json AS existing_binding_world_json \
    FROM expected AS e \
    LEFT JOIN keyed_run AS kr ON true \
    LEFT JOIN existing_run AS xr ON true \
), \
created_run AS ( \
    INSERT INTO wamn_run.runs \
      (tenant_id, run_id, catalog_id, catalog_version, environment, \
       wiring_id, wiring_version, wiring_hash, gate_report_id, binding_world_json, \
       status, trigger_source, capture_mode, input_json, invocation_context, \
       admission_context_version, platform_revision, idempotency_key, run_deadline_at) \
    SELECT c.tenant_id, c.run_id, c.catalog_id, c.expected_catalog_version, \
           c.environment, c.wiring_id, c.wiring_version, c.wiring_hash, \
           c.gate_report_id, c.binding_world_json, \
           'dispatched', c.trigger_source, c.capture_mode, \
           c.input_json, c.admitted_context, \
           '0.1', c.platform_revision, c.producer_key, c.run_deadline_at \
      FROM classified AS c WHERE c.result_code = 'ready' \
    ON CONFLICT DO NOTHING \
    RETURNING tenant_id, run_id, binding_world_json \
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
         WHEN c.result_code = 'ready' THEN 'conflicting-run-identity' \
         ELSE c.result_code END AS result_code, \
       CASE WHEN c.result_code = 'duplicate' THEN c.existing_run_id \
            WHEN c.result_code = 'ready' AND q.run_id IS NOT NULL THEN q.run_id \
            ELSE NULL END AS run_id, \
       CASE WHEN c.result_code = 'duplicate' THEN c.existing_binding_world_json \
            WHEN c.result_code = 'ready' AND q.run_id IS NOT NULL \
              THEN c.binding_world_json \
            ELSE NULL END::text AS binding_world_json \
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
            "candidate-not-found",
            "candidate-identity-mismatch",
            "gate-report-mismatch",
            "candidate-definition-invalid",
            "binding-world-unavailable",
            "binding-world-drift",
            "conflicting-run-identity",
            "invalid-producer",
            "invalid-input",
        ] {
            assert!(
                AdmissionResult::from_parts(code, None, None).is_some(),
                "{code}"
            );
        }
        for retired in [
            "head-not-found",
            "head-drift",
            "inactive-wiring",
            "definition-drift",
            "missing-root-plan",
            "inactive-definition",
            "registration-not-found",
            "registration-drift",
            "invalid-registration-hash",
            "invalid-event-lineage",
            "idempotency-key-reused",
            "idempotency-scope-changed",
        ] {
            assert_eq!(
                AdmissionResult::from_parts(retired, None, None),
                None,
                "{retired}"
            );
        }
        let world = serde_json::json!([]);
        assert_eq!(
            AdmissionResult::from_parts(
                "duplicate",
                Some("run-1".to_string()),
                Some(world.clone())
            ),
            Some(AdmissionResult::Duplicate {
                run_id: "run-1".to_string(),
                binding_world_json: world,
            })
        );
    }

    #[test]
    fn both_ordinary_statements_assert_current_user_authority() {
        let recipe = management_admission_transaction(&RunStateSchema::default());
        for statement in [recipe.lock_producer(), recipe.admit()] {
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
        assert!(recipe.lock_producer().contains("pg_advisory_xact_lock"));
        assert!(recipe.lock_producer().contains("case:' || report_id"));
        assert!(!recipe.lock_producer().contains("catalog_heads"));
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
        for statement in [recipe.lock_producer(), recipe.admit()] {
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
            "catalog_heads",
            "wiring_activation",
            "release_flows",
            "flow_artifacts",
            "execution_bundles",
            "execution_bundle_hash",
            "flow_id",
            "flow_version",
            "invocation_admissions",
            "attachment_id",
            "registration_id",
            "client_key_digest",
            "event_seq",
        ] {
            assert!(!sql.contains(retired), "management SQL retained {retired}");
        }
    }

    #[test]
    fn binding_world_is_database_derived_complete_and_canonically_ordered() {
        let sql = management_admission_transaction(&RunStateSchema::default()).admit;
        for required in [
            "FROM catalog.wirings AS wiring",
            "JOIN catalog.connection_requirements AS requirement",
            "JOIN catalog.connection_bindings AS binding",
            "binding.binding_status = 'active'",
            "binding.validation_status = 'valid'",
            "instance.lifecycle_status = 'enabled'",
            "generation.generation = instance.active_generation",
            "count(requirement.component_digest) AS requirement_count",
            "count(resolved.component_digest) AS resolved_count",
            "ORDER BY resolved.component_digest, resolved.store_alias",
            "THEN 'binding-world-unavailable'",
        ] {
            assert!(
                sql.contains(required),
                "missing binding-world arm: {required}"
            );
        }
        for non_secret_field in [
            "'component-digest'",
            "'store-alias'",
            "'requirement-hash'",
            "'instance-id'",
            "'instance-revision'",
            "'requirement-type'",
            "'contract'",
            "'validation-hash'",
            "'generation'",
            "'definition-hash'",
            "'credential-set-handle'",
        ] {
            assert!(sql.contains(non_secret_field), "missing {non_secret_field}");
        }
        assert!(!sql.contains("definition_json"));
    }

    #[test]
    fn test_case_report_is_the_candidate_gate_report() {
        let sql = management_admission_transaction(&RunStateSchema::default()).admit;
        assert!(sql.contains("e.report_id IS DISTINCT FROM e.gate_report_id"));
        assert!(sql.contains("e.actual_gate_report_id IS DISTINCT FROM e.gate_report_id"));
        assert_eq!(sql.matches("THEN 'gate-report-mismatch'").count(), 2);
    }
}
