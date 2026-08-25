//! The PROJECT-database half of sequential test-case composition.
//!
//! Residency (wamn-0h0g.8.5.4): everything here runs on the SECOND connection —
//! a scoped `wamn_management_admitter` generation on this environment's PROJECT
//! database (wamn-0h0g.8.5.3 landed the input). The control store in
//! [`super::test_orchestration`] runs on the FIRST connection, and nothing here
//! opens a transaction that touches both: it cannot, and the in-module statement
//! there that says so remains exactly true. Project facts this composition needs
//! leave this module as already-observed values.
//!
//! The surface is deliberately narrow — resolve one candidate, admit one
//! ordinal, observe one run — because that is the whole of what the admitter
//! credential is granted (`MANAGEMENT_ADMITTER_*` in
//! `crates/control/provision/src/sql.rs`). A column absent from those lists is
//! DENIED, not merely unmentioned, so a wider read here would fail closed at
//! runtime rather than compile-time.

use std::time::SystemTime;

use anyhow::{Context as _, bail};
use serde_json::Value;
use tokio_postgres::{Client, NoTls};

use wamn_control_provision::parse_management_admission_url;
use wamn_execution_contract::TestSetCase;
use wamn_run_state::RunStatus;
use wamn_run_state::admission::{
    AdmissionResult, AdmissionTransaction, ManagementProducerKey, RunStateSchema,
    management_admission_transaction,
};

/// Inject the tenant every project-plane row policy resolves against.
///
/// `catalog.wirings` and `wamn_run.runs` both FORCE row-level security keyed on
/// `NULLIF(current_setting('app.tenant', true), '')`, and the admission
/// statement reads the same setting for its own tenant. One session-level
/// injection therefore scopes the reads and the write identically; an
/// uninjected session sees zero rows rather than another tenant's.
const ADMISSION_SCOPE_SQL: &str = "SELECT \
    pg_catalog.set_config('app.tenant', $1, false), \
    pg_catalog.set_config('search_path', 'pg_catalog', false)";

/// Resolve the candidate wiring a test-set command names.
///
/// Keyed on `wiring_hash` alone: the owner ruled that `validated_draft_id` IS
/// the wiring hash, so the command carries the whole identity and no
/// cross-database mapping exists or is needed. Every remaining admission
/// parameter — `catalog_id`, `wiring_id`, `version`, `gated_catalog_version`,
/// `gate_report_id` — is a column of the row the hash selects, which is why the
/// admitter needs no `catalog.catalog_heads` grant to find them.
const SELECT_CANDIDATE_BY_HASH_SQL: &str = "SELECT catalog_id, wiring_id, version, \
        gated_catalog_version, gate_report_id, graph_json \
    FROM catalog.wirings \
    WHERE tenant_id = $1 AND wiring_hash = $2 \
    ORDER BY catalog_id, wiring_id, version";

/// Name the store aliases the candidate requires and this environment cannot
/// resolve.
///
/// This mirrors the `requirements` / `resolved_requirements` legs of the
/// admission statement, including instance lifecycle and active generation, so
/// a refusal names exactly the aliases whose absence produced
/// [`AdmissionResult::BindingWorldUnavailable`]. It is a diagnostic read only —
/// the admission statement, not this query, decides whether a run is admitted.
const SELECT_UNRESOLVED_STORE_ALIASES_SQL: &str = "WITH node AS ( \
        SELECT entry.value ->> 'component' AS component, \
               entry.value ->> 'interface-version' AS interface_version, \
               entry.value ->> 'operation' AS operation \
          FROM jsonb_each($4::jsonb) AS entry \
    ), component AS ( \
        SELECT DISTINCT library.component_digest \
          FROM node JOIN catalog.component_library AS library \
            ON library.tenant_id = $1 AND library.catalog_id = $2 \
           AND library.catalog_version = $3 \
           AND library.component = node.component \
           AND library.interface_version = node.interface_version \
           AND library.operation = node.operation \
    ), requirement AS ( \
        SELECT required.component_digest, required.store_alias \
          FROM component JOIN catalog.connection_requirements AS required \
            ON required.tenant_id = $1 AND required.artifact_hash IS NULL \
           AND required.requirement_name IS NULL \
           AND required.component_digest = component.component_digest \
    ) \
    SELECT DISTINCT requirement.store_alias \
      FROM requirement \
      LEFT JOIN catalog.connection_bindings AS binding \
        ON binding.tenant_id = $1 AND binding.catalog_id = $2 \
       AND binding.catalog_version = $3 AND binding.artifact_hash IS NULL \
       AND binding.requirement_name IS NULL \
       AND binding.component_digest = requirement.component_digest \
       AND binding.store_alias = requirement.store_alias \
       AND binding.environment = $5 AND binding.binding_status = 'active' \
       AND binding.validation_status = 'valid' \
      LEFT JOIN catalog.connection_instances AS instance \
        ON instance.tenant_id = binding.tenant_id \
       AND instance.environment = binding.environment \
       AND instance.instance_id = binding.instance_id \
       AND instance.lifecycle_status = 'enabled' \
       AND instance.active_generation IS NOT NULL \
      LEFT JOIN catalog.connection_generations AS generation \
        ON generation.tenant_id = instance.tenant_id \
       AND generation.environment = instance.environment \
       AND generation.instance_id = instance.instance_id \
       AND generation.generation = instance.active_generation \
     WHERE generation.generation IS NULL \
     ORDER BY 1";

/// The exact candidate row one test-set command selects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateWiring {
    pub catalog_id: String,
    /// The applied catalog version this definition was gated against. It is the
    /// admission statement's `expected_catalog_version` BY CONSTRUCTION: the
    /// candidate CTE joins `gated_catalog_version = expected_catalog_version`,
    /// so any other value selects no candidate at all.
    pub catalog_version: i32,
    pub wiring_id: String,
    pub wiring_version: i32,
    pub wiring_hash: String,
    /// The gate run that certified this definition. Admission REQUIRES the
    /// test-case `report_id` to equal it, so the control report identity is
    /// derived from the candidate rather than minted by the command.
    pub gate_report_id: String,
    /// The candidate's own `cases` array, riding `graph_json`.
    pub cases: Vec<TestSetCase>,
    /// The candidate's `nodes` object, retained only to diagnose an
    /// unresolvable binding world.
    nodes: Value,
}

/// The terminal facts one admitted run makes available to case evaluation.
///
/// Exactly the granted observation and evaluation columns: `status` says a run
/// reached terminal, the other four say what it produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedRun {
    pub status: RunStatus,
    pub caller_outcome_kind: Option<String>,
    pub caller_outcome_json: Option<Value>,
    pub caller_http_status: Option<i32>,
    pub fail_kind: Option<String>,
}

/// One running management surface's project-database admission connection.
pub struct AdmissionSurface {
    client: Client,
    connection_task: tokio::task::JoinHandle<()>,
    tenant_id: Box<str>,
    schema: RunStateSchema,
    recipe: AdmissionTransaction,
}

impl Drop for AdmissionSurface {
    fn drop(&mut self) {
        self.connection_task.abort();
    }
}

impl AdmissionSurface {
    /// Open the project-database admission credential for one fixed scope.
    ///
    /// Fails closed BEFORE ANY I/O when the input is absent or out of scope, on
    /// the same terms as the control-authoring connection: the parse is pure and
    /// runs first. `serve` already parsed the same value at startup; re-parsing
    /// here holds an in-process caller that never goes through `serve` to the
    /// identical gate.
    pub async fn connect(
        management_admission_database_url: &str,
        org: &str,
        project: &str,
        environment: &str,
        tenant_id: &str,
        schema: RunStateSchema,
    ) -> anyhow::Result<Self> {
        let connection = parse_management_admission_url(
            management_admission_database_url,
            org,
            project,
            environment,
        )?;
        if !wamn_control_registry::identifiers::valid_tenant(tenant_id) {
            bail!("invalid fixed admission tenant identity");
        }
        tracing::info!(
            database = connection.database(),
            role = connection.role(),
            generation = connection.generation().as_str(),
            "management admission credential accepted"
        );
        let (client, driver) = tokio_postgres::connect(management_admission_database_url, NoTls)
            .await
            .context("connect dedicated project admission database credential")?;
        let connection_task = tokio::spawn(async move {
            if let Err(error) = driver.await {
                tracing::error!(%error, "project admission database connection failed");
            }
        });
        let surface = Self {
            client,
            connection_task,
            tenant_id: tenant_id.into(),
            recipe: management_admission_transaction(&schema),
            schema,
        };
        surface.scope().await?;
        Ok(surface)
    }

    async fn scope(&self) -> anyhow::Result<()> {
        self.client
            .query_one(ADMISSION_SCOPE_SQL, &[&self.tenant_id.as_ref()])
            .await
            .context("inject fixed admission tenant scope")?;
        Ok(())
    }

    /// Resolve the one candidate a wiring hash names, or `None`.
    pub async fn candidate_by_hash(
        &self,
        wiring_hash: &str,
    ) -> anyhow::Result<Option<CandidateWiring>> {
        let rows = self
            .client
            .query(
                SELECT_CANDIDATE_BY_HASH_SQL,
                &[&self.tenant_id.as_ref(), &wiring_hash],
            )
            .await
            .context("resolve the candidate wiring by hash")?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        if rows.len() != 1 {
            bail!("one wiring hash selected {} candidate rows", rows.len());
        }
        let graph: Value = row.get(5);
        Ok(Some(CandidateWiring {
            catalog_id: row.get(0),
            catalog_version: row.get(3),
            wiring_id: row.get(1),
            wiring_version: row.get(2),
            wiring_hash: wiring_hash.to_owned(),
            gate_report_id: row.get(4),
            cases: candidate_cases(&graph)?,
            nodes: graph.get("nodes").cloned().unwrap_or(Value::Null),
        }))
    }

    /// Name the candidate's unresolvable store aliases, for one refusal.
    pub async fn unresolved_store_aliases(
        &self,
        candidate: &CandidateWiring,
        environment: &str,
    ) -> anyhow::Result<Vec<String>> {
        let nodes = if candidate.nodes.is_object() {
            candidate.nodes.clone()
        } else {
            Value::Object(serde_json::Map::new())
        };
        let rows = self
            .client
            .query(
                SELECT_UNRESOLVED_STORE_ALIASES_SQL,
                &[
                    &self.tenant_id.as_ref(),
                    &candidate.catalog_id,
                    &candidate.catalog_version,
                    &nodes,
                    &environment,
                ],
            )
            .await
            .context("name the candidate's unresolvable store aliases")?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Admit one ordinal's deterministic run under the private management
    /// authority.
    ///
    /// Both ordinary statements run in ONE transaction, in the order the recipe
    /// fixes: the advisory lock serializes this producer coordinate before the
    /// admitting statement takes its snapshot. `prior_binding_world` is the
    /// frozen world the report's first ordinal returned; admission REFUSES an
    /// ordinal above zero that does not carry one, which is what makes the
    /// per-case worlds identical by construction rather than by convention.
    pub async fn admit_test_case(
        &mut self,
        request: &TestCaseAdmission<'_>,
    ) -> anyhow::Result<AdmissionResult> {
        let key = ManagementProducerKey::TestCase {
            report_id: request.report_id,
            ordinal: request.ordinal,
        };
        let transaction = self
            .client
            .transaction()
            .await
            .context("begin the management admission transaction")?;
        transaction
            .query(
                self.recipe.lock_producer(),
                &[
                    &key.producer().as_sql(),
                    &Option::<&str>::None,
                    &request.report_id,
                    &request.ordinal,
                ],
            )
            .await
            .context("serialize the management producer coordinate")?;
        let row = transaction
            .query_one(
                self.recipe.admit(),
                &[
                    &key.producer().as_sql(),
                    &request.catalog_id,
                    &request.environment,
                    &request.catalog_version,
                    &request.run_id,
                    &request.input_json.to_string(),
                    &EMPTY_INVOCATION_CONTEXT,
                    &ADMITTED_PLATFORM_REVISION,
                    &request.run_deadline_at,
                    &Option::<&str>::None,
                    &request.report_id,
                    &request.ordinal,
                    &request.wiring_id,
                    &request.wiring_version,
                    &request.wiring_hash,
                    &request.gate_report_id,
                    &request.prior_binding_world.map(Value::to_string),
                ],
            )
            .await
            .context("admit one test-case run")?;
        transaction
            .commit()
            .await
            .context("commit the management admission transaction")?;
        let code: String = row.get(0);
        let binding_world: Option<String> = row.get(2);
        let binding_world = binding_world
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .context("decode the admitted binding world")?;
        AdmissionResult::from_parts(&code, row.get(1), binding_world)
            .with_context(|| format!("management admission returned an unknown result {code:?}"))
    }

    /// Read one admitted run's terminal facts.
    pub async fn observe_run(&self, run_id: &str) -> anyhow::Result<Option<ObservedRun>> {
        let statement = format!(
            "SELECT status, caller_outcome_kind, caller_outcome_json, \
                    caller_http_status, fail_kind \
               FROM {}.runs WHERE tenant_id = $1 AND run_id = $2",
            self.schema.as_str(),
        );
        let Some(row) = self
            .client
            .query_opt(&statement, &[&self.tenant_id.as_ref(), &run_id])
            .await
            .context("observe one admitted test-case run")?
        else {
            return Ok(None);
        };
        let status: String = row.get(0);
        let status = RunStatus::from_sql(&status)
            .with_context(|| format!("run status {status:?} is outside the stored vocabulary"))?;
        Ok(Some(ObservedRun {
            status,
            caller_outcome_kind: row.get(1),
            caller_outcome_json: row.get(2),
            caller_http_status: row.get(3),
            fail_kind: row.get(4),
        }))
    }
}

/// The invocation context a test-case run is admitted with.
///
/// Test-case admission does NOT set `producer`: the statement classifies an
/// `invocation_context` carrying `producer: draft-scenario` as invalid input for
/// this producer, and adds no key of its own the way the draft-run leg does.
const EMPTY_INVOCATION_CONTEXT: &str = "{}";

/// The `runs.platform_revision` every test-case admission is written under.
///
/// FROZEN, not derived from the build. An exact retry re-runs the admitting
/// statement against a run row that already exists, and that statement refuses
/// with `conflicting-run-identity` if ANY pinned column — `platform_revision`
/// included — differs from the stored one. A build-derived value would therefore
/// turn a redeploy mid-report into a permanent refusal instead of the
/// convergence this composition is required to have. The cost is that this
/// column records the admission dialect rather than the binary; the run's
/// `invocation_context` already carries the pins that matter.
const ADMITTED_PLATFORM_REVISION: &str = "management-admission-0.1";

/// Everything one ordinal's admission is parameterized by.
///
/// EVERY field is derived from the reservation or the candidate, never from the
/// clock or the process: an exact retry rebuilds this struct identically or the
/// admitting statement refuses it.
#[derive(Clone, Copy, Debug)]
pub struct TestCaseAdmission<'a> {
    pub report_id: &'a str,
    pub ordinal: i32,
    pub run_id: &'a str,
    pub catalog_id: &'a str,
    pub catalog_version: i32,
    pub environment: &'a str,
    pub wiring_id: &'a str,
    pub wiring_version: i32,
    pub wiring_hash: &'a str,
    pub gate_report_id: &'a str,
    pub input_json: &'a Value,
    /// The ordinal's stored `case_deadline_at`, read back from the control
    /// reservation rather than recomputed. The reservation fixed it once; a
    /// clock-derived value here would differ on every retry and refuse.
    pub run_deadline_at: SystemTime,
    pub prior_binding_world: Option<&'a Value>,
}

/// Read a candidate's `cases` array out of its stored graph.
///
/// The array is the flow document's own, so it is decoded with the contract type
/// and held to the contract's bounds rather than to a second declaration of
/// them.
fn candidate_cases(graph: &Value) -> anyhow::Result<Vec<TestSetCase>> {
    let Some(cases) = graph.get("cases") else {
        return Ok(Vec::new());
    };
    serde_json::from_value(cases.clone()).context("decode the candidate's stored cases array")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_candidate_carrying_no_cases_reads_as_an_empty_selection() {
        assert_eq!(candidate_cases(&json!({"nodes": {}})).unwrap(), Vec::new());
        assert_eq!(candidate_cases(&json!({"cases": []})).unwrap(), Vec::new());
    }

    #[test]
    fn the_stored_cases_array_decodes_as_the_contract_type() {
        let cases = candidate_cases(&json!({
            "cases": [{
                "case-id": "roundtrip",
                "input": {"a": 1},
                "expect": {"outcome": "responded", "status": 201},
            }],
        }))
        .expect("a well-formed cases array decodes");
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case_id, "roundtrip");
        assert_eq!(cases[0].expect.status, Some(201));
        // The contract type denies unknown fields, so a foreign array is refused
        // here rather than silently narrowed.
        assert!(
            candidate_cases(&json!({"cases": [{"case-id": "x", "input": {}, "why": 1}]})).is_err()
        );
    }

    /// The admission statement's own classification, restated as the reason this
    /// module never mints a report identity: `report_id` MUST equal
    /// `gate_report_id`, and `gate_report_id` MUST equal the candidate row's.
    #[test]
    fn the_admission_recipe_forces_the_report_to_be_the_candidates_gate_report() {
        let recipe = management_admission_transaction(&RunStateSchema::default());
        assert!(recipe.admit().contains(
            "WHEN e.producer = 'test-case' \
             AND e.report_id IS DISTINCT FROM e.gate_report_id \
             THEN 'gate-report-mismatch'"
        ));
        assert!(
            recipe
                .admit()
                .contains("e.actual_gate_report_id IS DISTINCT FROM e.gate_report_id")
        );
        // And the candidate is selected by the gated catalog version, so
        // `expected_catalog_version` cannot be anything else.
        assert!(
            recipe
                .admit()
                .contains("wiring.gated_catalog_version = k.expected_catalog_version")
        );
    }

    /// Every parameter the admission statement declares is supplied positionally
    /// by [`AdmissionSurface::admit_test_case`], so a reordered recipe is a
    /// compile-time-invisible fault this pins.
    #[test]
    fn the_admission_call_matches_the_recipes_declared_parameters() {
        let admit = management_admission_transaction(&RunStateSchema::default());
        let admit = admit.admit();
        for (index, name) in [
            (1, "producer"),
            (2, "catalog_id"),
            (3, "environment"),
            (4, "expected_catalog_version"),
            (5, "run_id"),
            (6, "input_json"),
            (7, "invocation_context"),
            (8, "platform_revision"),
            (9, "run_deadline_at"),
            (10, "command_id"),
            (11, "report_id"),
            (12, "case_ordinal"),
            (13, "wiring_id"),
            (14, "wiring_version"),
            (15, "wiring_hash"),
            (16, "gate_report_id"),
            (17, "prior_binding_world_json"),
        ] {
            assert!(
                admit.contains(&format!("${index}::text AS {name}"))
                    || admit.contains(&format!("${index}::int AS {name}"))
                    || admit.contains(&format!("${index}::timestamptz AS {name}"))
                    || admit.contains(&format!("${index}::text::jsonb AS {name}")),
                "parameter ${index} is no longer {name}"
            );
        }
    }
}
