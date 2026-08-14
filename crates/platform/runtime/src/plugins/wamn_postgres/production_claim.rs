//! Host-only composition of one production queue claim transaction.

use std::fmt::{Display, Formatter};

use deadpool_postgres::Object;
use tokio_postgres::Row;
use tokio_postgres::types::{FromSql, ToSql};
use wamn_run_state::queue::{
    ProductionClaimClass, classify_production_claim, grant_production_claim_sql,
    reset_pre_effect_claim_sql, select_claim_effect_attempt_sql, select_exhausted_production_sql,
    select_production_claim_sql, serialize_effect_intent_sql,
    terminalize_effect_uncertain_claim_sql, terminalize_exhausted_production_sql,
    terminalize_resolution_refusal_claim_sql,
};
use wamn_run_state::sql::{
    materialize_run_flow_resolutions_sql, select_candidate_resolution_override_sql,
    select_release_resolution_bindings_sql, select_release_resolution_plans_sql,
};
use wamn_run_state::{
    BoundConnectionRequirement, CandidatePlanOverride, CatalogResolutionPlan,
    EffectUncertainFailure, FailKind, FlowResolutionRefusal, PinnedRunResolution, RunStatus,
    resolve_run_flow_resolutions,
};

use super::WamnPostgres;

/// Stable category for a production-claim failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionClaimErrorKind {
    /// Required host-injected identity was absent.
    Identity,
    /// PostgreSQL checkout, transaction, query, or commit failed.
    Storage,
    /// Stored data or a typed database result violated the claim contract.
    Contract,
}

/// Contextual failure from the host-only production claim boundary.
#[derive(Debug)]
pub struct ProductionClaimError {
    kind: ProductionClaimErrorKind,
    operation: &'static str,
    detail: String,
}

impl ProductionClaimError {
    fn new(
        kind: ProductionClaimErrorKind,
        operation: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation,
            detail: detail.into(),
        }
    }

    /// Return the stable failure category.
    pub fn kind(&self) -> ProductionClaimErrorKind {
        self.kind
    }

    /// Return the operation that failed.
    pub fn operation(&self) -> &'static str {
        self.operation
    }
}

impl Display for ProductionClaimError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "production claim {} failed: {}",
            self.operation, self.detail
        )
    }
}

impl std::error::Error for ProductionClaimError {}

/// Result of one host-only production queue turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionClaimResult {
    /// No eligible row was visible to this tenant.
    Empty,
    /// A complete immutable map and fresh lease committed for guest execution.
    Ready {
        run_id: String,
        payload: String,
        lease_generation: i64,
    },
    /// Claim-time classification or resolution removed the row without execution.
    Terminalized {
        run_id: String,
        status: RunStatus,
        fail_kind: FailKind,
    },
}

/// Result of one host-owned crash-budget janitor turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionReapResult {
    /// No exhausted row was visible to this tenant.
    Empty,
    /// Immutable effect evidence owns this row; the ordinary claimant must
    /// terminalize it as effect-uncertain.
    EffectAttempt { run_id: String },
    /// The selected pre-effect row was marked infrastructure-failure and
    /// dequeued with exact caller compare-and-set semantics.
    Reaped { run_id: String },
}

#[derive(Debug)]
struct SelectedClaim {
    tenant_id: String,
    run_id: String,
    had_prior_lease: bool,
    status: RunStatus,
    root_flow_id: String,
    flow_version: i32,
    catalog_id: String,
    catalog_version: i32,
    environment: String,
    execution_bundle_hash: String,
    payload: String,
}

#[derive(Debug, Clone)]
struct LoadedCandidateOverride {
    validated_draft_hash: String,
    catalog_id: String,
    catalog_version: i32,
    environment: String,
    flow_id: String,
    runtime_flow_version: i32,
    execution_bundle_hash: String,
    draft_artifact_hash: String,
    binding_base_artifact_hash: String,
    exact_bytes: Vec<u8>,
}

enum CandidateOverrideLoad {
    Ready(Option<CandidatePlanOverride>),
    Refused(FlowResolutionRefusal),
}

#[derive(Debug)]
struct ExhaustedClaim {
    run_id: String,
    status: RunStatus,
    root_flow_id: String,
    flow_version: i32,
}

impl WamnPostgres {
    /// Select, classify, resolve, and grant at most one production run.
    ///
    /// Tenant, project, schema, and lease owner come only from host-injected
    /// component identity. A `Ready` result is returned only after COMMIT, so
    /// the guest's separate plan-supply transactions can observe the complete
    /// immutable resolution map.
    pub async fn claim_next_production(
        &self,
        component_id: &str,
        lease_ttl_ms: i64,
    ) -> Result<ProductionClaimResult, ProductionClaimError> {
        if lease_ttl_ms <= 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate lease TTL",
                "lease TTL must be positive",
            ));
        }
        let tenant = self.tenant_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve tenant",
                "component has no host-injected tenant",
            )
        })?;
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (connection, policy) = self.checkout(&project).await.map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "checkout project connection",
                format!("{error:?}"),
            )
        })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "begin tenant transaction",
                format!("{error:?}"),
            ));
        }

        let result = claim_in_transaction(&connection, &runner, lease_ttl_ms).await;
        match result {
            Ok(result) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(ProductionClaimError::new(
                        ProductionClaimErrorKind::Storage,
                        "commit production claim",
                        error.to_string(),
                    ));
                }
                Ok(result)
            }
            Err(error) => {
                if let Err(rollback_error) = connection.batch_execute("ROLLBACK").await {
                    tracing::warn!(
                        error = %rollback_error,
                        operation = error.operation(),
                        "production claim rollback failed; destroying connection"
                    );
                    self.destroy(connection);
                }
                Err(error)
            }
        }
    }

    /// Reap at most one crash-budget-exhausted pre-effect run.
    ///
    /// The candidate and run rows are locked before a fresh effect-evidence
    /// snapshot. Caller JSON and its RFC 8785 hash are computed by the host,
    /// never from PostgreSQL's non-canonical `jsonb::text` rendering.
    pub async fn reap_one_exhausted_production(
        &self,
        component_id: &str,
        grace_ms: i64,
    ) -> Result<ProductionReapResult, ProductionClaimError> {
        if grace_ms < 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate janitor grace",
                "janitor grace must not be negative",
            ));
        }
        let tenant = self.tenant_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve janitor tenant",
                "component has no host-injected tenant",
            )
        })?;
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve janitor runner",
                "component has no host-injected runner",
            )
        })?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (connection, policy) = self.checkout(&project).await.map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "checkout janitor connection",
                format!("{error:?}"),
            )
        })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "begin janitor transaction",
                format!("{error:?}"),
            ));
        }

        let result = reap_in_transaction(&connection, grace_ms).await;
        match result {
            Ok(result) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(ProductionClaimError::new(
                        ProductionClaimErrorKind::Storage,
                        "commit janitor turn",
                        error.to_string(),
                    ));
                }
                Ok(result)
            }
            Err(error) => {
                if let Err(rollback_error) = connection.batch_execute("ROLLBACK").await {
                    tracing::warn!(
                        error = %rollback_error,
                        operation = error.operation(),
                        "production janitor rollback failed; destroying connection"
                    );
                    self.destroy(connection);
                }
                Err(error)
            }
        }
    }
}

async fn claim_in_transaction(
    connection: &Object,
    runner: &str,
    lease_ttl_ms: i64,
) -> Result<ProductionClaimResult, ProductionClaimError> {
    let select_sql = select_production_claim_sql();
    let select = connection
        .prepare_cached(&select_sql)
        .await
        .map_err(|error| storage("prepare production candidate", error))?;
    let Some(row) = connection
        .query_opt(&select, &[])
        .await
        .map_err(|error| storage("select production candidate", error))?
    else {
        return Ok(ProductionClaimResult::Empty);
    };
    let selected = decode_selected_claim(&row)?;
    if !matches!(selected.status, RunStatus::Dispatched | RunStatus::Running) {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "validate selected run",
            format!(
                "queue row {} references non-runnable status {}",
                selected.run_id,
                selected.status.as_sql()
            ),
        ));
    }

    serialize_effect_intent(connection, &selected.run_id, "production claim").await?;

    let effect_sql = select_claim_effect_attempt_sql();
    let effect_statement = connection
        .prepare_cached(&effect_sql)
        .await
        .map_err(|error| storage("prepare effect-attempt classification", error))?;
    let effect_row = connection
        .query_one(&effect_statement, &[&selected.run_id])
        .await
        .map_err(|error| storage("classify effect-attempt evidence", error))?;
    let has_effect_attempt = row_value(&effect_row, 0, "effect-attempt evidence")?;

    match classify_production_claim(selected.had_prior_lease, has_effect_attempt) {
        ProductionClaimClass::ExpiredWithAttempt => {
            return terminalize_effect_uncertain(connection, &selected).await;
        }
        ProductionClaimClass::ExpiredPreEffect => {
            let reset_sql = reset_pre_effect_claim_sql();
            let reset = connection
                .prepare_cached(&reset_sql)
                .await
                .map_err(|error| storage("prepare pre-effect reset", error))?;
            connection
                .query_one(&reset, &[&selected.run_id])
                .await
                .map_err(|error| storage("reset pre-effect projection", error))?;
        }
        ProductionClaimClass::Ordinary => {}
    }

    let candidate = match load_candidate_override(connection, &selected).await? {
        CandidateOverrideLoad::Ready(candidate) => candidate,
        CandidateOverrideLoad::Refused(refusal) => {
            return terminalize_refusal(connection, &selected, &refusal).await;
        }
    };
    let release_plans = load_release_plans(connection, &selected.run_id).await?;
    let bindings = load_release_bindings(connection, &selected.run_id).await?;
    let pinned = PinnedRunResolution {
        tenant_id: selected.tenant_id.clone(),
        run_id: selected.run_id.clone(),
        root_flow_id: selected.root_flow_id.clone(),
        catalog_id: selected.catalog_id.clone(),
        catalog_version: selected.catalog_version,
        environment: selected.environment.clone(),
        execution_bundle_hash: selected.execution_bundle_hash.clone(),
    };
    let resolutions = match resolve_run_flow_resolutions(
        &pinned,
        &release_plans,
        &bindings,
        candidate.as_ref(),
    ) {
        Ok(resolutions) => resolutions,
        Err(refusal) => return terminalize_refusal(connection, &selected, &refusal).await,
    };

    let map_json = serde_json::to_string(&resolutions).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "serialize resolution map",
            error.to_string(),
        )
    })?;
    let materialize_sql = materialize_run_flow_resolutions_sql();
    let materialize = connection
        .prepare_cached(&materialize_sql)
        .await
        .map_err(|error| storage("prepare resolution materialization", error))?;
    let row = connection
        .query_one(&materialize, &[&selected.run_id, &map_json])
        .await
        .map_err(|error| storage("materialize resolution map", error))?;
    let result_code: String = row_value(&row, 0, "resolution result code")?;
    let fail_kind: Option<String> = row_value(&row, 1, "resolution fail kind")?;
    match result_code.as_str() {
        "resolved" if fail_kind.is_none() => {}
        "refused" => {
            let fail_kind = fail_kind
                .as_deref()
                .and_then(FailKind::from_sql)
                .ok_or_else(|| {
                    ProductionClaimError::new(
                        ProductionClaimErrorKind::Contract,
                        "decode materialization refusal",
                        format!("unknown fail kind {fail_kind:?}"),
                    )
                })?;
            let refusal = FlowResolutionRefusal {
                kind: fail_kind,
                flow_id: selected.root_flow_id.clone(),
                reason: "immutable resolution-map materialization refused".to_string(),
            };
            return terminalize_refusal(connection, &selected, &refusal).await;
        }
        _ => {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "decode materialization result",
                format!("unexpected result {result_code:?} with {fail_kind:?}"),
            ));
        }
    }

    let grant_sql = grant_production_claim_sql();
    let grant = connection
        .prepare_cached(&grant_sql)
        .await
        .map_err(|error| storage("prepare production lease grant", error))?;
    let row = connection
        .query_opt(&grant, &[&selected.run_id, &runner, &lease_ttl_ms])
        .await
        .map_err(|error| storage("grant production lease", error))?
        .ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "grant production lease",
                "selected queue row disappeared while locked",
            )
        })?;
    let lease_generation = row_value(&row, 0, "lease generation")?;
    Ok(ProductionClaimResult::Ready {
        run_id: selected.run_id,
        payload: selected.payload,
        lease_generation,
    })
}

async fn reap_in_transaction(
    connection: &Object,
    grace_ms: i64,
) -> Result<ProductionReapResult, ProductionClaimError> {
    let select_sql = select_exhausted_production_sql();
    let select = connection
        .prepare_cached(&select_sql)
        .await
        .map_err(|error| storage("prepare exhausted candidate", error))?;
    let Some(row) = connection
        .query_opt(&select, &[&grace_ms])
        .await
        .map_err(|error| storage("select exhausted candidate", error))?
    else {
        return Ok(ProductionReapResult::Empty);
    };
    let status_text: String = row_value(&row, 2, "exhausted run status")?;
    let status = RunStatus::from_sql(&status_text).ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode exhausted candidate",
            format!("unknown run status {status_text:?}"),
        )
    })?;
    let selected = ExhaustedClaim {
        run_id: row_value(&row, 1, "exhausted run id")?,
        status,
        root_flow_id: row_value(&row, 3, "exhausted root flow id")?,
        flow_version: row_value(&row, 4, "exhausted root flow version")?,
    };
    if !matches!(selected.status, RunStatus::Dispatched | RunStatus::Running) {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "validate exhausted candidate",
            format!("non-runnable status {}", selected.status.as_sql()),
        ));
    }

    serialize_effect_intent(connection, &selected.run_id, "exhausted-run reaper").await?;

    let effect_sql = select_claim_effect_attempt_sql();
    let effect = connection
        .prepare_cached(&effect_sql)
        .await
        .map_err(|error| storage("prepare exhausted effect classification", error))?;
    let row = connection
        .query_one(&effect, &[&selected.run_id])
        .await
        .map_err(|error| storage("classify exhausted effect evidence", error))?;
    if row_value(&row, 0, "exhausted effect-attempt evidence")? {
        return Ok(ProductionReapResult::EffectAttempt {
            run_id: selected.run_id,
        });
    }

    let (body, body_hash) = generic_failure_outcome(
        "infrastructure-failure",
        &selected.root_flow_id,
        selected.flow_version,
        &selected.run_id,
    )?;
    let terminalize_sql = terminalize_exhausted_production_sql();
    let terminalize = connection
        .prepare_cached(&terminalize_sql)
        .await
        .map_err(|error| storage("prepare exhausted terminalization", error))?;
    let row = connection
        .query_opt(&terminalize, &[&selected.run_id, &body, &body_hash])
        .await
        .map_err(|error| storage("terminalize exhausted run", error))?
        .ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "terminalize exhausted run",
                "selected run or queue row disappeared while locked",
            )
        })?;
    let status: String = row_value(&row, 0, "exhausted terminal status")?;
    if status != RunStatus::InfrastructureFailure.as_sql() {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "terminalize exhausted run",
            format!("unexpected status {status:?}"),
        ));
    }
    Ok(ProductionReapResult::Reaped {
        run_id: selected.run_id,
    })
}

async fn serialize_effect_intent(
    connection: &Object,
    run_id: &str,
    owner: &'static str,
) -> Result<(), ProductionClaimError> {
    let sql = serialize_effect_intent_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare effect-intent fence", error))?;
    connection
        .query_one(&statement, &[&run_id])
        .await
        .map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "acquire effect-intent fence",
                format!("{owner}: {error}"),
            )
        })?;
    Ok(())
}

async fn load_candidate_override(
    connection: &Object,
    selected: &SelectedClaim,
) -> Result<CandidateOverrideLoad, ProductionClaimError> {
    let sql = select_candidate_resolution_override_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare candidate override load", error))?;
    let row = connection
        .query_opt(&statement, &[&selected.run_id])
        .await
        .map_err(|error| storage("load candidate override", error))?
        .ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "load candidate override",
                "selected run disappeared while locked",
            )
        })?;
    let requested_hash: Option<String> = row_value(&row, 0, "requested candidate hash")?;
    let Some(requested_hash) = requested_hash else {
        return Ok(CandidateOverrideLoad::Ready(None));
    };
    let loaded = match decode_loaded_candidate_override(&row)? {
        Some(loaded) => loaded,
        None => {
            return Ok(CandidateOverrideLoad::Refused(candidate_refusal(
                selected,
                "validated draft identity is absent or incomplete",
            )));
        }
    };
    match validate_candidate_override(selected, &requested_hash, loaded) {
        Ok(candidate) => Ok(CandidateOverrideLoad::Ready(Some(candidate))),
        Err(refusal) => Ok(CandidateOverrideLoad::Refused(refusal)),
    }
}

fn decode_loaded_candidate_override(
    row: &Row,
) -> Result<Option<LoadedCandidateOverride>, ProductionClaimError> {
    let validated_draft_hash: Option<String> = row_value(row, 1, "validated draft hash")?;
    let catalog_id: Option<String> = row_value(row, 2, "candidate catalog id")?;
    let catalog_version: Option<i32> = row_value(row, 3, "candidate catalog version")?;
    let environment: Option<String> = row_value(row, 4, "candidate environment")?;
    let flow_id: Option<String> = row_value(row, 5, "candidate flow id")?;
    let runtime_flow_version: Option<i32> = row_value(row, 6, "candidate flow version")?;
    let execution_bundle_hash: Option<String> = row_value(row, 7, "candidate bundle hash")?;
    let draft_artifact_hash: Option<String> = row_value(row, 8, "candidate artifact hash")?;
    let binding_base_artifact_hash: Option<String> =
        row_value(row, 9, "candidate binding-base artifact hash")?;
    let exact_bytes: Option<Vec<u8>> = row_value(row, 10, "candidate plan bytes")?;
    let (
        Some(validated_draft_hash),
        Some(catalog_id),
        Some(catalog_version),
        Some(environment),
        Some(flow_id),
        Some(runtime_flow_version),
        Some(execution_bundle_hash),
        Some(draft_artifact_hash),
        Some(binding_base_artifact_hash),
        Some(exact_bytes),
    ) = (
        validated_draft_hash,
        catalog_id,
        catalog_version,
        environment,
        flow_id,
        runtime_flow_version,
        execution_bundle_hash,
        draft_artifact_hash,
        binding_base_artifact_hash,
        exact_bytes,
    )
    else {
        return Ok(None);
    };
    Ok(Some(LoadedCandidateOverride {
        validated_draft_hash,
        catalog_id,
        catalog_version,
        environment,
        flow_id,
        runtime_flow_version,
        execution_bundle_hash,
        draft_artifact_hash,
        binding_base_artifact_hash,
        exact_bytes,
    }))
}

fn validate_candidate_override(
    selected: &SelectedClaim,
    requested_hash: &str,
    loaded: LoadedCandidateOverride,
) -> Result<CandidatePlanOverride, FlowResolutionRefusal> {
    let exact_pins_match = loaded.validated_draft_hash == requested_hash
        && loaded.catalog_id == selected.catalog_id
        && loaded.catalog_version == selected.catalog_version
        && loaded.environment == selected.environment
        && loaded.flow_id == selected.root_flow_id
        && loaded.runtime_flow_version == selected.flow_version
        && loaded.execution_bundle_hash == selected.execution_bundle_hash;
    if !exact_pins_match {
        return Err(candidate_refusal(
            selected,
            "validated draft identity differs from the admitted run pins",
        ));
    }
    Ok(CandidatePlanOverride {
        flow_id: loaded.flow_id,
        execution_bundle_hash: loaded.execution_bundle_hash,
        draft_artifact_hash: loaded.draft_artifact_hash,
        source_artifact_hash: loaded.binding_base_artifact_hash,
        exact_bytes: loaded.exact_bytes,
    })
}

fn candidate_refusal(selected: &SelectedClaim, reason: &str) -> FlowResolutionRefusal {
    FlowResolutionRefusal {
        kind: FailKind::ForeignRevision,
        flow_id: selected.root_flow_id.clone(),
        reason: reason.to_string(),
    }
}

async fn load_release_plans(
    connection: &Object,
    run_id: &str,
) -> Result<Vec<CatalogResolutionPlan>, ProductionClaimError> {
    let sql = select_release_resolution_plans_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare release plan load", error))?;
    connection
        .query(&statement, &[&run_id])
        .await
        .map_err(|error| storage("load release plans", error))?
        .iter()
        .map(|row| {
            Ok(CatalogResolutionPlan {
                flow_id: row_value(row, 0, "release flow id")?,
                execution_bundle_hash: row_value(row, 1, "release execution bundle hash")?,
                source_artifact_hash: row_value(row, 2, "release source artifact hash")?,
                exact_bytes: row_value(row, 3, "release plan bytes")?,
            })
        })
        .collect()
}

async fn load_release_bindings(
    connection: &Object,
    run_id: &str,
) -> Result<Vec<BoundConnectionRequirement>, ProductionClaimError> {
    let sql = select_release_resolution_bindings_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare release binding load", error))?;
    connection
        .query(&statement, &[&run_id])
        .await
        .map_err(|error| storage("load release bindings", error))?
        .iter()
        .map(|row| {
            Ok(BoundConnectionRequirement {
                source_artifact_hash: row_value(row, 0, "binding source artifact hash")?,
                requirement_name: row_value(row, 1, "binding requirement name")?,
                requirement_type: row_value(row, 2, "binding requirement type")?,
                contract: row_value(row, 3, "binding contract")?,
            })
        })
        .collect()
}

async fn terminalize_effect_uncertain(
    connection: &Object,
    selected: &SelectedClaim,
) -> Result<ProductionClaimResult, ProductionClaimError> {
    let failure = EffectUncertainFailure::new(selected.run_id.clone()).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "build effect-uncertain outcome",
            error.to_string(),
        )
    })?;
    let body = serde_json::to_string(&failure.as_json()).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "serialize effect-uncertain outcome",
            error.to_string(),
        )
    })?;
    let hash = failure.canonical_json_hash();
    let sql = terminalize_effect_uncertain_claim_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare effect-uncertain terminalization", error))?;
    let row = connection
        .query_opt(&statement, &[&selected.run_id, &body, &hash])
        .await
        .map_err(|error| storage("terminalize effect uncertainty", error))?
        .ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "terminalize effect uncertainty",
                "selected run or queue row disappeared while locked",
            )
        })?;
    let status: String = row_value(&row, 0, "effect-uncertain status")?;
    if status != RunStatus::EffectUncertain.as_sql() {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "terminalize effect uncertainty",
            format!("unexpected status {status:?}"),
        ));
    }
    Ok(ProductionClaimResult::Terminalized {
        run_id: selected.run_id.clone(),
        status: RunStatus::EffectUncertain,
        fail_kind: FailKind::EffectUncertain,
    })
}

async fn terminalize_refusal(
    connection: &Object,
    selected: &SelectedClaim,
    refusal: &FlowResolutionRefusal,
) -> Result<ProductionClaimResult, ProductionClaimError> {
    let (body_json, body_hash) = generic_failure_outcome(
        refusal.kind.as_sql(),
        &selected.root_flow_id,
        selected.flow_version,
        &selected.run_id,
    )?;
    let fail_kind = refusal.kind.as_sql();
    let sql = terminalize_resolution_refusal_claim_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare resolution refusal terminalization", error))?;
    let params: [&(dyn ToSql + Sync); 6] = [
        &selected.run_id,
        &fail_kind,
        &refusal.flow_id,
        &refusal.reason,
        &body_json,
        &body_hash,
    ];
    let row = connection
        .query_opt(&statement, &params)
        .await
        .map_err(|error| storage("terminalize resolution refusal", error))?
        .ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "terminalize resolution refusal",
                "selected run or queue row disappeared while locked",
            )
        })?;
    let status: String = row_value(&row, 0, "resolution-refusal status")?;
    if status != RunStatus::Failed.as_sql() {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "terminalize resolution refusal",
            format!("unexpected status {status:?}"),
        ));
    }
    Ok(ProductionClaimResult::Terminalized {
        run_id: selected.run_id.clone(),
        status: RunStatus::Failed,
        fail_kind: refusal.kind,
    })
}

fn generic_failure_outcome(
    code: &str,
    root_flow_id: &str,
    flow_version: i32,
    run_id: &str,
) -> Result<(String, String), ProductionClaimError> {
    let flow_version = u32::try_from(flow_version).map_err(|_| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "build generic failure outcome",
            format!("invalid root flow version {flow_version}"),
        )
    })?;
    let body = serde_json::json!({
        "error": {
            "code": code,
            "flow-id": root_flow_id,
            "flow-version": flow_version,
            "run-id": run_id,
        }
    });
    let body_hash = wamn_flow::canonical_json_sha256(&body);
    let body_json = serde_json::to_string(&body).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "serialize generic failure outcome",
            error.to_string(),
        )
    })?;
    Ok((body_json, body_hash))
}

fn decode_selected_claim(row: &Row) -> Result<SelectedClaim, ProductionClaimError> {
    let status: String = row_value(row, 3, "run status")?;
    let status = RunStatus::from_sql(&status).ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production candidate",
            format!("unknown run status {status:?}"),
        )
    })?;
    Ok(SelectedClaim {
        tenant_id: row_value(row, 0, "tenant id")?,
        run_id: row_value(row, 1, "run id")?,
        had_prior_lease: row_value(row, 2, "prior lease evidence")?,
        status,
        root_flow_id: row_value(row, 4, "root flow id")?,
        flow_version: row_value(row, 5, "root flow version")?,
        catalog_id: row_value(row, 6, "catalog id")?,
        catalog_version: row_value(row, 7, "catalog version")?,
        environment: row_value(row, 8, "environment")?,
        execution_bundle_hash: row_value(row, 9, "execution bundle hash")?,
        payload: row_value(row, 10, "authoritative input")?,
    })
}

fn row_value<T>(row: &Row, index: usize, field: &'static str) -> Result<T, ProductionClaimError>
where
    for<'value> T: FromSql<'value>,
{
    row.try_get(index).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production claim row",
            format!("{field}: {error}"),
        )
    })
}

fn storage(operation: &'static str, error: tokio_postgres::Error) -> ProductionClaimError {
    ProductionClaimError::new(
        ProductionClaimErrorKind::Storage,
        operation,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_refusal_body_is_exact_and_message_free() {
        let (json, hash) = generic_failure_outcome("foreign-revision", "root", 7, "run-1").unwrap();
        let body = serde_json::from_str::<serde_json::Value>(&json).unwrap();
        assert_eq!(
            body,
            serde_json::from_str::<serde_json::Value>(
                r#"{"error":{"code":"foreign-revision","flow-id":"root","flow-version":7,"run-id":"run-1"}}"#,
            )
            .unwrap()
        );
        assert!(body["error"].get("message").is_none());
        assert_eq!(hash, wamn_flow::canonical_json_sha256(&body));
    }

    #[test]
    fn janitor_failure_body_uses_host_jcs_not_database_json_text() {
        let (json, hash) =
            generic_failure_outcome("infrastructure-failure", "root-flow", 19, "run-exhausted")
                .unwrap();
        assert_eq!(
            json,
            r#"{"error":{"code":"infrastructure-failure","flow-id":"root-flow","flow-version":19,"run-id":"run-exhausted"}}"#
        );
        let body = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, wamn_flow::canonical_json_sha256(&body));
    }

    fn selected_candidate() -> SelectedClaim {
        SelectedClaim {
            tenant_id: "tenant-a".into(),
            run_id: "run-a".into(),
            had_prior_lease: false,
            status: RunStatus::Dispatched,
            root_flow_id: "root".into(),
            flow_version: 7,
            catalog_id: "catalog-a".into(),
            catalog_version: 11,
            environment: "test".into(),
            execution_bundle_hash: format!("sha256:{}", "a".repeat(64)),
            payload: "{}".into(),
        }
    }

    fn loaded_candidate(selected: &SelectedClaim) -> LoadedCandidateOverride {
        LoadedCandidateOverride {
            validated_draft_hash: "validated-a".into(),
            catalog_id: selected.catalog_id.clone(),
            catalog_version: selected.catalog_version,
            environment: selected.environment.clone(),
            flow_id: selected.root_flow_id.clone(),
            runtime_flow_version: selected.flow_version,
            execution_bundle_hash: selected.execution_bundle_hash.clone(),
            draft_artifact_hash: "draft-artifact-a".into(),
            binding_base_artifact_hash: "release-artifact-a".into(),
            exact_bytes: b"plan".to_vec(),
        }
    }

    #[test]
    fn draft_candidate_override_requires_every_admitted_pin() {
        let selected = selected_candidate();
        let loaded = loaded_candidate(&selected);
        let candidate = validate_candidate_override(&selected, "validated-a", loaded.clone())
            .expect("every exact pin matches");
        assert_eq!(candidate.flow_id, "root");
        assert_eq!(candidate.source_artifact_hash, "release-artifact-a");
        assert_eq!(candidate.draft_artifact_hash, "draft-artifact-a");

        let mut mismatches = Vec::new();
        let mut row = loaded.clone();
        row.validated_draft_hash = "other".into();
        mismatches.push(row);
        let mut row = loaded.clone();
        row.catalog_version += 1;
        mismatches.push(row);
        let mut row = loaded.clone();
        row.environment = "prod".into();
        mismatches.push(row);
        let mut row = loaded.clone();
        row.flow_id = "other-root".into();
        mismatches.push(row);
        let mut row = loaded.clone();
        row.runtime_flow_version += 1;
        mismatches.push(row);
        let mut row = loaded;
        row.execution_bundle_hash = format!("sha256:{}", "b".repeat(64));
        mismatches.push(row);
        for mismatch in mismatches {
            let refusal = validate_candidate_override(&selected, "validated-a", mismatch)
                .expect_err("one mismatched pin must fail closed");
            assert_eq!(refusal.kind, FailKind::ForeignRevision);
            assert_eq!(refusal.flow_id, "root");
        }
    }

    #[test]
    fn effect_classification_precedes_any_plan_or_candidate_load() {
        let source = include_str!("production_claim.rs");
        let classify = source.find("classify_production_claim(").unwrap();
        let candidate = source.find("load_candidate_override(connection").unwrap();
        let release = source.find("load_release_plans(connection").unwrap();
        assert!(classify < candidate && candidate < release);
    }

    #[test]
    fn claim_and_reaper_fence_before_fresh_effect_snapshot() {
        let source = include_str!("production_claim.rs");
        let claim_start = source.find("async fn claim_in_transaction(").unwrap();
        let reap_start = source.find("async fn reap_in_transaction(").unwrap();
        let fence_helper = source.find("async fn serialize_effect_intent(").unwrap();
        for body in [
            &source[claim_start..reap_start],
            &source[reap_start..fence_helper],
        ] {
            let fence = body
                .find("serialize_effect_intent(connection")
                .expect("composer acquires the shared fence");
            let snapshot = body
                .find("let effect_sql = select_claim_effect_attempt_sql()")
                .expect("composer reads a fresh effect snapshot");
            assert!(fence < snapshot);
        }
        let tests_start = source.find("#[cfg(test)]").unwrap();
        let helper = &source[fence_helper..tests_start];
        assert!(helper.contains("let sql = serialize_effect_intent_sql();"));
        assert!(!helper.contains("SELECT true"));
    }

    #[test]
    fn complete_map_materialization_precedes_fresh_lease_grant() {
        let source = include_str!("production_claim.rs");
        let materialize = source
            .find("materialize_run_flow_resolutions_sql()")
            .unwrap();
        let grant = source.find("grant_production_claim_sql()").unwrap();
        assert!(materialize < grant);
        let lease_sql = grant_production_claim_sql();
        assert!(lease_sql.contains("lease_expires_at = statement_timestamp()"));
        assert!(!lease_sql.contains("lease_expires_at = now()"));
    }
}
