//! Production provider for `wamn:flow-invocation@0.1.0`.
//!
//! The provider owns final admission, bounded waiter reconciliation, and
//! observed-disconnect cancellation. HTTP adaptation, authentication, mapping,
//! and flow graph execution remain outside this module.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use deadpool_postgres::Pool;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;
use tokio_postgres::{AsyncMessage, NoTls, Row, Transaction};
use wamn_flow_invocation::{
    Admitted, BeginResult, CancelAck, Failure, FlowError, InvokeRequest, InvokeResult, Rejection,
    Response,
};
use wamn_run_state::admission::{AdmissionResult, admission_sql};
use wamn_run_state::invocation::{
    InvocationCancelResult, InvocationOutcome, InvocationPoll, InvocationRecovery,
    InvocationTarget, cancel_inline_invocation_sql, decode_invocation_cancel,
    lookup_invocation_recovery_sql, poll_invocation_outcome_sql, resolve_invocation_target_sql,
};

const INLINE_GENERATION: i64 = 1;

#[derive(Debug, Clone)]
pub struct InvocationServiceConfig {
    pub tenant_id: String,
    pub catalog_id: String,
    pub environment: String,
    pub project: String,
    pub schema: Option<String>,
    pub executor_id: String,
    pub platform_revision: String,
    pub lease_ttl: Duration,
    pub admission_ttl: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineRunClaim {
    pub run_id: String,
    pub lease_owner: String,
    pub lease_generation: i64,
    pub tenant: String,
    pub project: String,
    pub schema: Option<String>,
}

pub trait InlineRunDriver: Send + Sync + 'static {
    fn start(&self, claim: InlineRunClaim) -> anyhow::Result<()>;
}

#[derive(Debug, Clone)]
pub struct HttpAdmission {
    pub target: InvocationTarget,
    pub request: InvokeRequest,
    pub principal_digest: String,
    pub client_key_digest: String,
    pub input: Value,
    pub invocation_context: Value,
    pub response_deadline_at: Option<DateTime<Utc>>,
    pub run_deadline_at: DateTime<Utc>,
}

#[async_trait]
pub trait InvocationBackend: Clone + Send + Sync + 'static {
    async fn resolve_target(
        &self,
        tenant: &str,
        catalog: &str,
        environment: &str,
        attachment: &str,
    ) -> anyhow::Result<Option<InvocationTarget>>;

    #[allow(clippy::too_many_arguments)]
    async fn recover(
        &self,
        tenant: &str,
        catalog: &str,
        environment: &str,
        attachment: &str,
        principal_digest: &str,
        client_key_digest: &str,
        definition_hash: &str,
        fingerprint: &str,
    ) -> anyhow::Result<InvocationRecovery>;

    async fn admit(
        &self,
        config: &InvocationServiceConfig,
        admission: &HttpAdmission,
    ) -> anyhow::Result<AdmissionResult>;

    async fn poll(&self, tenant: &str, run_id: &str) -> anyhow::Result<InvocationPoll>;

    async fn cancel(
        &self,
        tenant: &str,
        run_id: &str,
        executor: &str,
        generation: i64,
    ) -> anyhow::Result<InvocationCancelResult>;
}

#[derive(Clone)]
pub struct InvocationService<B> {
    backend: B,
    notification_database_url: Option<String>,
    config: InvocationServiceConfig,
    driver: Arc<dyn InlineRunDriver>,
    handles: Arc<Mutex<HashMap<String, i64>>>,
}

impl<B: InvocationBackend> InvocationService<B> {
    pub fn new(
        backend: B,
        notification_database_url: Option<String>,
        config: InvocationServiceConfig,
        driver: Arc<dyn InlineRunDriver>,
    ) -> Self {
        Self {
            backend,
            notification_database_url,
            config,
            driver,
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn begin(&self, request: InvokeRequest) -> anyhow::Result<BeginResult> {
        let Some(client_key) = request.idempotency_key.as_deref() else {
            return Ok(rejected(400, "idempotency-key-required"));
        };
        let Some(target) = self
            .backend
            .resolve_target(
                &self.config.tenant_id,
                &self.config.catalog_id,
                &self.config.environment,
                &request.attachment_id,
            )
            .await?
        else {
            return Ok(rejected(404, "attachment-not-found"));
        };

        let principal_digest = digest(request.principal.as_bytes());
        let client_key_digest = digest(client_key.as_bytes());
        let recovery = self
            .backend
            .recover(
                &self.config.tenant_id,
                &self.config.catalog_id,
                &self.config.environment,
                &request.attachment_id,
                &principal_digest,
                &client_key_digest,
                &target.definition_hash,
                &request.client_request_fingerprint,
            )
            .await?;
        match recovery {
            InvocationRecovery::Released(outcome) => {
                return Ok(BeginResult::Admitted(Admitted {
                    run_id: outcome.run_id,
                }));
            }
            InvocationRecovery::InFlight { .. } => return Ok(rejected(409, "in-flight")),
            InvocationRecovery::IdempotencyKeyReused => {
                return Ok(rejected(409, "idempotency-key-reused"));
            }
            InvocationRecovery::IdempotencyScopeChanged => {
                return Ok(rejected(409, "idempotency-scope-changed"));
            }
            InvocationRecovery::OutcomeExpired => return Ok(rejected(409, "outcome-expired")),
            InvocationRecovery::Missing => {}
        }

        if !target.enabled {
            return Ok(rejected(404, "attachment-disabled"));
        }
        if target.definition_hash != request.expected_definition_hash {
            return Ok(rejected(409, "admission-retry"));
        }
        let expected_catalog_version = i32::try_from(request.expected_catalog_version)
            .map_err(|_| anyhow!("catalog version exceeds PostgreSQL int"))?;
        if target.catalog_version != expected_catalog_version {
            // A promotion carrying an unchanged attachment is safe to retry:
            // the definition hash includes the resolved auth source.
            if target.definition_hash != request.expected_definition_hash {
                return Ok(rejected(409, "admission-retry"));
            }
        }

        let input: Value = serde_json::from_str(&request.payload)
            .context("mapped invocation payload is not JSON")?;
        let (response_deadline_at, run_deadline_at) = deadlines(&target, &request)?;
        let invocation_context = invocation_context(&request);
        let mut admission = HttpAdmission {
            target,
            request,
            principal_digest,
            client_key_digest,
            input,
            invocation_context,
            response_deadline_at,
            run_deadline_at,
        };

        // Final admission owns the second drift check. One re-resolution/retry
        // is permitted only when promotion carried the exact definition hash.
        for attempt in 0..=1 {
            match self.backend.admit(&self.config, &admission).await? {
                AdmissionResult::Admitted { run_id } => {
                    self.handles
                        .lock()
                        .await
                        .insert(run_id.clone(), INLINE_GENERATION);
                    self.driver.start(InlineRunClaim {
                        run_id: run_id.clone(),
                        lease_owner: self.config.executor_id.clone(),
                        lease_generation: INLINE_GENERATION,
                        tenant: self.config.tenant_id.clone(),
                        project: self.config.project.clone(),
                        schema: self.config.schema.clone(),
                    })?;
                    return Ok(BeginResult::Admitted(Admitted { run_id }));
                }
                AdmissionResult::Duplicate {
                    run_id: Some(run_id),
                } => {
                    return Ok(BeginResult::Admitted(Admitted { run_id }));
                }
                AdmissionResult::HeadDrift | AdmissionResult::DefinitionDrift if attempt == 0 => {
                    let current = self
                        .backend
                        .resolve_target(
                            &self.config.tenant_id,
                            &self.config.catalog_id,
                            &self.config.environment,
                            &admission.request.attachment_id,
                        )
                        .await?;
                    match current {
                        Some(next) if next.definition_hash == admission.target.definition_hash => {
                            admission.target = next;
                        }
                        _ => return Ok(rejected(409, "admission-retry")),
                    };
                }
                refusal => return Ok(admission_refusal(refusal)),
            }
        }
        Ok(rejected(409, "admission-retry"))
    }

    pub async fn wait(
        &self,
        run_id: String,
        timeout_ms: u32,
    ) -> anyhow::Result<Option<InvokeResult>> {
        if let Some(outcome) = released(self.backend.poll(&self.config.tenant_id, &run_id).await?)?
        {
            return Ok(Some(to_invoke_result(outcome)?));
        }

        if let Some(database_url) = &self.notification_database_url {
            let mut notifications = listen_for_outcomes(database_url).await?;
            // Poll after subscription closes the poll-to-subscribe lost-wake race.
            if let Some(outcome) =
                released(self.backend.poll(&self.config.tenant_id, &run_id).await?)?
            {
                return Ok(Some(to_invoke_result(outcome)?));
            }
            let expected = format!("{}:{run_id}", self.config.tenant_id);
            let deadline = tokio::time::sleep(Duration::from_millis(u64::from(timeout_ms)));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    message = notifications.recv() => {
                        match message {
                            Some(payload) if payload == expected => break,
                            Some(_) => {}
                            None => break,
                        }
                    }
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(u64::from(timeout_ms))).await;
        }

        // Notifications are hints. This final poll is the lost-notification
        // fallback and the only source of outcome truth.
        released(self.backend.poll(&self.config.tenant_id, &run_id).await?)?
            .map(to_invoke_result)
            .transpose()
    }

    pub async fn cancel(&self, run_id: String) -> anyhow::Result<CancelAck> {
        let generation = self.handles.lock().await.get(&run_id).copied();
        if let Some(generation) = generation {
            let _ = self
                .backend
                .cancel(
                    &self.config.tenant_id,
                    &run_id,
                    &self.config.executor_id,
                    generation,
                )
                .await?;
        }
        Ok(CancelAck { run_id })
    }
}

async fn listen_for_outcomes(database_url: &str) -> anyhow::Result<OutcomeListener> {
    let (client, mut connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect invocation outcome listener")?;
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(message) =
            futures_util::future::poll_fn(|context| connection.poll_message(context)).await
        {
            match message {
                Ok(AsyncMessage::Notification(notification)) => {
                    let _ = sender.send(notification.payload().to_string());
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "flow-invocation outcome LISTEN connection failed"
                    );
                    break;
                }
            }
        }
    });
    client
        .batch_execute("LISTEN wamn_run_outcome")
        .await
        .context("LISTEN wamn_run_outcome")?;
    Ok(OutcomeListener {
        _client: client,
        receiver,
    })
}

struct OutcomeListener {
    _client: tokio_postgres::Client,
    receiver: tokio::sync::mpsc::UnboundedReceiver<String>,
}

impl OutcomeListener {
    async fn recv(&mut self) -> Option<String> {
        self.receiver.recv().await
    }
}

#[derive(Clone)]
pub struct PostgresInvocationBackend {
    pool: Pool,
}

impl PostgresInvocationBackend {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub fn from_database_url(database_url: &str) -> anyhow::Result<Self> {
        let config = database_url.parse()?;
        let manager = deadpool_postgres::Manager::new(config, NoTls);
        let pool = Pool::builder(manager).max_size(16).build()?;
        Ok(Self::new(pool))
    }

    async fn transaction(&self) -> anyhow::Result<deadpool_postgres::Object> {
        self.pool
            .get()
            .await
            .context("checkout invocation database")
    }
}

async fn set_tenant(transaction: &Transaction<'_>, tenant: &str) -> anyhow::Result<()> {
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await?;
    Ok(())
}

#[async_trait]
impl InvocationBackend for PostgresInvocationBackend {
    async fn resolve_target(
        &self,
        tenant: &str,
        catalog: &str,
        environment: &str,
        attachment: &str,
    ) -> anyhow::Result<Option<InvocationTarget>> {
        let mut client = self.transaction().await?;
        let transaction = client.transaction().await?;
        set_tenant(&transaction, tenant).await?;
        let row = transaction
            .query_opt(
                resolve_invocation_target_sql(),
                &[&catalog, &environment, &attachment],
            )
            .await?;
        transaction.commit().await?;
        row.map(decode_target).transpose()
    }

    async fn recover(
        &self,
        tenant: &str,
        catalog: &str,
        environment: &str,
        attachment: &str,
        principal_digest: &str,
        client_key_digest: &str,
        definition_hash: &str,
        fingerprint: &str,
    ) -> anyhow::Result<InvocationRecovery> {
        let mut client = self.transaction().await?;
        let transaction = client.transaction().await?;
        set_tenant(&transaction, tenant).await?;
        let row = transaction
            .query_one(
                lookup_invocation_recovery_sql(),
                &[
                    &catalog,
                    &environment,
                    &attachment,
                    &principal_digest,
                    &client_key_digest,
                    &definition_hash,
                    &fingerprint,
                ],
            )
            .await?;
        transaction.commit().await?;
        decode_recovery(&row)
    }

    async fn admit(
        &self,
        config: &InvocationServiceConfig,
        admission: &HttpAdmission,
    ) -> anyhow::Result<AdmissionResult> {
        let mut client = self.transaction().await?;
        let transaction = client.transaction().await?;
        set_tenant(&transaction, &config.tenant_id).await?;
        let run_id: String = transaction
            .query_one("SELECT gen_random_uuid()::text", &[])
            .await?
            .get(0);
        let recipe = admission_sql();
        transaction
            .query_one(
                recipe.lock_head(),
                &[&config.catalog_id, &config.environment],
            )
            .await?;

        let producer = "http";
        let catalog_version = admission.target.catalog_version;
        let flow_version = admission.target.flow_version;
        let input = serde_json::to_string(&admission.input)?;
        let context = serde_json::to_string(&admission.invocation_context)?;
        let expires_at = Utc::now()
            + TimeDelta::from_std(config.admission_ttl).context("admission TTL overflow")?;
        let lease_ttl = i64::try_from(config.lease_ttl.as_millis())
            .context("lease TTL exceeds i64 milliseconds")?;
        let none_text: Option<&str> = None;
        let none_i64: Option<i64> = None;
        let none_i32: Option<i32> = None;
        let row = transaction
            .query_one(
                recipe.admit(),
                &[
                    &producer,
                    &config.catalog_id,
                    &config.environment,
                    &catalog_version,
                    &admission.request.attachment_id,
                    &admission.target.definition_hash,
                    &admission.target.flow_id,
                    &flow_version,
                    &run_id,
                    &input,
                    &context,
                    &config.platform_revision,
                    &admission.response_deadline_at,
                    &admission.run_deadline_at,
                    &admission.principal_digest,
                    &admission.client_key_digest,
                    &admission.request.client_request_fingerprint,
                    &expires_at,
                    &config.executor_id,
                    &lease_ttl,
                    &none_i64,
                    &none_text,
                    &none_text,
                    &none_i64,
                    &none_text,
                    &none_text,
                    &none_text,
                    &none_text,
                    &none_i32,
                    &none_text,
                    &"blocking",
                ],
            )
            .await?;
        let result = AdmissionResult::from_parts(row.get(0), row.get(1))
            .ok_or_else(|| anyhow!("invalid admission result row"))?;
        transaction.commit().await?;
        Ok(result)
    }

    async fn poll(&self, tenant: &str, run_id: &str) -> anyhow::Result<InvocationPoll> {
        let mut client = self.transaction().await?;
        let transaction = client.transaction().await?;
        set_tenant(&transaction, tenant).await?;
        let row = transaction
            .query_one(poll_invocation_outcome_sql(), &[&run_id])
            .await?;
        transaction.commit().await?;
        decode_poll(&row)
    }

    async fn cancel(
        &self,
        tenant: &str,
        run_id: &str,
        executor: &str,
        generation: i64,
    ) -> anyhow::Result<InvocationCancelResult> {
        let mut client = self.transaction().await?;
        let transaction = client.transaction().await?;
        set_tenant(&transaction, tenant).await?;
        let row = transaction
            .query_one(
                cancel_inline_invocation_sql(),
                &[&run_id, &executor, &generation],
            )
            .await?;
        let result = decode_invocation_cancel(row.get(0))
            .ok_or_else(|| anyhow!("invalid invocation cancel result row"))?;
        transaction.commit().await?;
        Ok(result)
    }
}

fn decode_target(row: Row) -> anyhow::Result<InvocationTarget> {
    Ok(InvocationTarget {
        catalog_version: row.get(0),
        definition_hash: row.get(1),
        flow_id: row.get(2),
        flow_version: row.get(3),
        definition: serde_json::from_str(row.get(4))?,
        auth_policy: serde_json::from_str(row.get(5))?,
        enabled: row.get(6),
    })
}

fn decode_outcome(row: &Row) -> anyhow::Result<InvocationOutcome> {
    let body: String = row
        .get::<_, Option<String>>(3)
        .ok_or_else(|| anyhow!("released outcome missing body"))?;
    let status = row
        .get::<_, Option<i32>>(4)
        .map(u16::try_from)
        .transpose()
        .context("stored HTTP status exceeds u16")?;
    Ok(InvocationOutcome {
        run_id: row
            .get::<_, Option<String>>(1)
            .ok_or_else(|| anyhow!("released outcome missing run id"))?,
        kind: row
            .get::<_, Option<String>>(2)
            .ok_or_else(|| anyhow!("released outcome missing kind"))?,
        body: serde_json::from_str(&body)?,
        http_status: status,
        hash: row
            .get::<_, Option<String>>(5)
            .ok_or_else(|| anyhow!("released outcome missing hash"))?,
        flow_id: row
            .get::<_, Option<String>>(6)
            .ok_or_else(|| anyhow!("released outcome missing flow id"))?,
        flow_version: u32::try_from(
            row.get::<_, Option<i32>>(7)
                .ok_or_else(|| anyhow!("released outcome missing flow version"))?,
        )
        .context("stored flow version is negative")?,
    })
}

fn decode_recovery(row: &Row) -> anyhow::Result<InvocationRecovery> {
    Ok(match row.get::<_, &str>(0) {
        "missing" => InvocationRecovery::Missing,
        "in-flight" => InvocationRecovery::InFlight {
            run_id: row
                .get::<_, Option<String>>(1)
                .ok_or_else(|| anyhow!("in-flight admission missing run id"))?,
        },
        "released" => InvocationRecovery::Released(decode_outcome(row)?),
        "idempotency-key-reused" => InvocationRecovery::IdempotencyKeyReused,
        "idempotency-scope-changed" => InvocationRecovery::IdempotencyScopeChanged,
        "outcome-expired" => InvocationRecovery::OutcomeExpired,
        code => return Err(anyhow!("invalid invocation recovery result {code:?}")),
    })
}

fn decode_poll(row: &Row) -> anyhow::Result<InvocationPoll> {
    Ok(match row.get::<_, &str>(0) {
        "running" => InvocationPoll::Running,
        "released" => InvocationPoll::Released(decode_outcome(row)?),
        "not-found" => InvocationPoll::NotFound,
        code => return Err(anyhow!("invalid invocation poll result {code:?}")),
    })
}

fn released(poll: InvocationPoll) -> anyhow::Result<Option<InvocationOutcome>> {
    match poll {
        InvocationPoll::Running => Ok(None),
        InvocationPoll::Released(outcome) => Ok(Some(outcome)),
        InvocationPoll::NotFound => Err(anyhow!("invocation run not found")),
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn invocation_context(request: &InvokeRequest) -> Value {
    let mut context = json!({"request-fingerprint": request.client_request_fingerprint});
    if let Some(trace) = &request.trace {
        context["trace"] = json!({
            "traceparent": trace.traceparent,
            "tracestate": trace.tracestate,
        });
    }
    context
}

fn deadlines(
    target: &InvocationTarget,
    request: &InvokeRequest,
) -> anyhow::Result<(Option<DateTime<Utc>>, DateTime<Utc>)> {
    let run_ms = target
        .definition
        .get("run-deadline-ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("attachment definition missing run-deadline-ms"))?;
    let run_ms = request
        .deadline_override
        .map_or(run_ms, |limit| limit.min(run_ms));
    let response_ms = target
        .definition
        .get("response-deadline-ms")
        .and_then(Value::as_u64)
        .map(|value| value.min(run_ms));
    let now = Utc::now();
    let run = now + TimeDelta::milliseconds(i64::try_from(run_ms)?);
    let response = response_ms
        .map(i64::try_from)
        .transpose()?
        .map(|milliseconds| now + TimeDelta::milliseconds(milliseconds));
    Ok((response, run))
}

fn rejected(status: u16, code: &str) -> BeginResult {
    BeginResult::Rejected(Rejection {
        status,
        code: code.to_string(),
    })
}

fn admission_refusal(result: AdmissionResult) -> BeginResult {
    let (status, code) = match result {
        AdmissionResult::InactiveDefinition => (404, "attachment-disabled"),
        AdmissionResult::IdempotencyKeyReused => (409, "idempotency-key-reused"),
        AdmissionResult::IdempotencyScopeChanged => (409, "idempotency-scope-changed"),
        AdmissionResult::Duplicate { run_id: None }
        | AdmissionResult::HeadDrift
        | AdmissionResult::DefinitionDrift => (409, "admission-retry"),
        AdmissionResult::HeadNotFound => (404, "attachment-not-found"),
        AdmissionResult::ConflictingRunIdentity => (409, "conflicting-run-identity"),
        _ => (400, "invalid-admission"),
    };
    rejected(status, code)
}

fn to_invoke_result(outcome: InvocationOutcome) -> anyhow::Result<InvokeResult> {
    match outcome.kind.as_str() {
        "responded" => Ok(InvokeResult::Responded(Response {
            run_id: outcome.run_id,
            body: serde_json::to_string(&outcome.body)?,
            status_hint: outcome.http_status,
        })),
        "failed" | "cancelled" => {
            let error = outcome
                .body
                .get("error")
                .ok_or_else(|| anyhow!("stored failure missing error envelope"))?;
            let flow_error = FlowError {
                code: required_string(error, "code")?,
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                run_id: required_string(error, "run-id")?,
                flow_id: required_string(error, "flow-id")?,
                flow_version: error
                    .get("flow-version")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("stored failure missing flow-version"))?
                    .try_into()?,
            };
            let failure = Failure {
                status: outcome
                    .http_status
                    .ok_or_else(|| anyhow!("stored failure missing HTTP status"))?,
                error: flow_error,
            };
            if outcome.kind == "failed" {
                Ok(InvokeResult::Failed(failure))
            } else {
                Ok(InvokeResult::Cancelled(failure))
            }
        }
        kind => Err(anyhow!("unknown stored caller outcome kind {kind:?}")),
    }
}

fn required_string(value: &Value, key: &str) -> anyhow::Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("stored failure missing {key}"))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use wamn_flow_invocation::TraceContext;

    #[derive(Clone, Default)]
    struct MockBackend {
        targets: Arc<Mutex<VecDeque<Option<InvocationTarget>>>>,
        recoveries: Arc<Mutex<VecDeque<InvocationRecovery>>>,
        admissions: Arc<Mutex<VecDeque<AdmissionResult>>>,
        admitted_versions: Arc<Mutex<Vec<i32>>>,
        polls: Arc<Mutex<VecDeque<InvocationPoll>>>,
        cancels: Arc<Mutex<Vec<(String, String, i64)>>>,
    }

    #[derive(Default)]
    struct RecordingDriver {
        claims: std::sync::Mutex<Vec<InlineRunClaim>>,
    }

    impl InlineRunDriver for RecordingDriver {
        fn start(&self, claim: InlineRunClaim) -> anyhow::Result<()> {
            self.claims
                .lock()
                .expect("recording driver lock poisoned")
                .push(claim);
            Ok(())
        }
    }

    fn driver() -> Arc<RecordingDriver> {
        Arc::new(RecordingDriver::default())
    }

    #[async_trait]
    impl InvocationBackend for MockBackend {
        async fn resolve_target(
            &self,
            _tenant: &str,
            _catalog: &str,
            _environment: &str,
            _attachment: &str,
        ) -> anyhow::Result<Option<InvocationTarget>> {
            Ok(self.targets.lock().await.pop_front().flatten())
        }

        async fn recover(
            &self,
            _tenant: &str,
            _catalog: &str,
            _environment: &str,
            _attachment: &str,
            _principal_digest: &str,
            _client_key_digest: &str,
            _definition_hash: &str,
            _fingerprint: &str,
        ) -> anyhow::Result<InvocationRecovery> {
            self.recoveries
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| anyhow!("missing recovery fixture"))
        }

        async fn admit(
            &self,
            _config: &InvocationServiceConfig,
            admission: &HttpAdmission,
        ) -> anyhow::Result<AdmissionResult> {
            self.admitted_versions
                .lock()
                .await
                .push(admission.target.catalog_version);
            self.admissions
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| anyhow!("missing admission fixture"))
        }

        async fn poll(&self, _tenant: &str, _run_id: &str) -> anyhow::Result<InvocationPoll> {
            self.polls
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| anyhow!("missing poll fixture"))
        }

        async fn cancel(
            &self,
            _tenant: &str,
            run_id: &str,
            executor: &str,
            generation: i64,
        ) -> anyhow::Result<InvocationCancelResult> {
            self.cancels
                .lock()
                .await
                .push((run_id.to_string(), executor.to_string(), generation));
            Ok(InvocationCancelResult::Requested)
        }
    }

    fn config() -> InvocationServiceConfig {
        InvocationServiceConfig {
            tenant_id: "tenant-a".to_string(),
            catalog_id: "catalog-a".to_string(),
            environment: "prod".to_string(),
            project: "project-a".to_string(),
            schema: Some("tenant_a".to_string()),
            executor_id: "invoke-1".to_string(),
            platform_revision: "rev-test".to_string(),
            lease_ttl: Duration::from_secs(30),
            admission_ttl: Duration::from_secs(86_400),
        }
    }

    fn target(enabled: bool) -> InvocationTarget {
        InvocationTarget {
            catalog_version: 8,
            definition_hash: "sha256:def".to_string(),
            flow_id: "flow-a".to_string(),
            flow_version: 3,
            definition: json!({
                "run-deadline-ms": 60_000,
                "response-deadline-ms": 30_000
            }),
            auth_policy: json!({"scheme": "bearer"}),
            enabled,
        }
    }

    fn request() -> InvokeRequest {
        InvokeRequest {
            attachment_id: "attachment-a".to_string(),
            expected_catalog_version: 8,
            expected_definition_hash: "sha256:def".to_string(),
            client_request_fingerprint: "sha256:request".to_string(),
            payload: r#"{"value":1}"#.to_string(),
            idempotency_key: Some("client-key".to_string()),
            principal: "principal-a".to_string(),
            deadline_override: None,
            trace: Some(TraceContext {
                traceparent: "00-00000000000000000000000000000001-0000000000000001-01".to_string(),
                tracestate: None,
            }),
        }
    }

    fn outcome(kind: &str, code: &str, status: u16) -> InvocationOutcome {
        let body = if kind == "responded" {
            json!({"ok": true})
        } else {
            json!({"error": {
                "code": code,
                "run-id": "run-1",
                "flow-id": "flow-a",
                "flow-version": 3
            }})
        };
        InvocationOutcome {
            run_id: "run-1".to_string(),
            kind: kind.to_string(),
            body,
            http_status: Some(status),
            hash: "sha256:outcome".to_string(),
            flow_id: "flow-a".to_string(),
            flow_version: 3,
        }
    }

    #[tokio::test]
    async fn disabled_attachment_recovers_released_outcome_before_activation_check() {
        let backend = MockBackend::default();
        backend.targets.lock().await.push_back(Some(target(false)));
        backend
            .recoveries
            .lock()
            .await
            .push_back(InvocationRecovery::Released(outcome("responded", "", 200)));
        let service = InvocationService::new(backend.clone(), None, config(), driver());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            BeginResult::Admitted(Admitted {
                run_id: "run-1".to_string()
            })
        );
        assert!(backend.admissions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn disabled_attachment_rejects_new_admission_without_a_run() {
        let backend = MockBackend::default();
        backend.targets.lock().await.push_back(Some(target(false)));
        backend
            .recoveries
            .lock()
            .await
            .push_back(InvocationRecovery::Missing);
        let service = InvocationService::new(backend.clone(), None, config(), driver());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            rejected(404, "attachment-disabled")
        );
        assert!(backend.admissions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn unchanged_promotion_retries_final_admission_once() {
        let backend = MockBackend::default();
        backend.targets.lock().await.extend([
            Some(target(true)),
            Some(InvocationTarget {
                catalog_version: 9,
                ..target(true)
            }),
        ]);
        backend
            .recoveries
            .lock()
            .await
            .push_back(InvocationRecovery::Missing);
        backend.admissions.lock().await.extend([
            AdmissionResult::HeadDrift,
            AdmissionResult::Admitted {
                run_id: "run-promoted".to_string(),
            },
        ]);
        let driver = driver();
        let service = InvocationService::new(backend.clone(), None, config(), driver.clone());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            BeginResult::Admitted(Admitted {
                run_id: "run-promoted".to_string()
            })
        );
        assert_eq!(backend.admitted_versions.lock().await.as_slice(), &[8, 9]);
        assert_eq!(
            driver
                .claims
                .lock()
                .expect("recording driver lock poisoned")
                .as_slice(),
            &[InlineRunClaim {
                run_id: "run-promoted".to_string(),
                lease_owner: "invoke-1".to_string(),
                lease_generation: 1,
                tenant: "tenant-a".to_string(),
                project: "project-a".to_string(),
                schema: Some("tenant_a".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn recovery_conflicts_are_distinct_and_never_admit() {
        for (recovery, code) in [
            (
                InvocationRecovery::IdempotencyKeyReused,
                "idempotency-key-reused",
            ),
            (
                InvocationRecovery::IdempotencyScopeChanged,
                "idempotency-scope-changed",
            ),
            (InvocationRecovery::OutcomeExpired, "outcome-expired"),
            (
                InvocationRecovery::InFlight {
                    run_id: "run-live".to_string(),
                },
                "in-flight",
            ),
        ] {
            let backend = MockBackend::default();
            backend.targets.lock().await.push_back(Some(target(true)));
            backend.recoveries.lock().await.push_back(recovery);
            let service = InvocationService::new(backend.clone(), None, config(), driver());
            let BeginResult::Rejected(rejection) = service.begin(request()).await.unwrap() else {
                panic!("conflict must reject before admission");
            };
            assert_eq!(rejection.code, code);
            assert!(backend.admissions.lock().await.is_empty());
        }
    }

    #[tokio::test]
    async fn lost_notification_falls_back_to_the_final_bounded_poll() {
        let backend = MockBackend::default();
        backend.polls.lock().await.extend([
            InvocationPoll::Running,
            InvocationPoll::Released(outcome("responded", "", 202)),
        ]);
        let service = InvocationService::new(backend, None, config(), driver());

        assert!(matches!(
            service.wait("run-1".to_string(), 0).await.unwrap(),
            Some(InvokeResult::Responded(Response {
                status_hint: Some(202),
                ..
            }))
        ));
    }

    #[test]
    fn all_five_release_races_decode_the_exact_stored_winner() {
        for (path, stored, expected) in [
            ("respond", outcome("responded", "", 201), ("responded", 201)),
            (
                "authored-fail",
                outcome("failed", "invalid-receipt", 400),
                ("failed", 400),
            ),
            (
                "node-error",
                outcome("failed", "infrastructure-failure", 500),
                ("failed", 500),
            ),
            (
                "response-deadline",
                outcome("cancelled", "response-deadline", 504),
                ("cancelled", 504),
            ),
            (
                "observed-disconnect",
                outcome("cancelled", "observed-disconnect", 499),
                ("cancelled", 499),
            ),
        ] {
            let result = to_invoke_result(stored).unwrap();
            let actual = match result {
                InvokeResult::Responded(response) => ("responded", response.status_hint.unwrap()),
                InvokeResult::Failed(failure) => ("failed", failure.status),
                InvokeResult::Cancelled(failure) => ("cancelled", failure.status),
            };
            assert_eq!(actual, expected, "{path}");
        }
    }

    #[tokio::test]
    async fn cancel_uses_the_inline_owner_generation_and_is_idempotently_acked() {
        let backend = MockBackend::default();
        backend.targets.lock().await.push_back(Some(target(true)));
        backend
            .recoveries
            .lock()
            .await
            .push_back(InvocationRecovery::Missing);
        backend
            .admissions
            .lock()
            .await
            .push_back(AdmissionResult::Admitted {
                run_id: "run-1".to_string(),
            });
        let service = InvocationService::new(backend.clone(), None, config(), driver());
        let _ = service.begin(request()).await.unwrap();

        assert_eq!(
            service.cancel("run-1".to_string()).await.unwrap(),
            CancelAck {
                run_id: "run-1".to_string()
            }
        );
        assert_eq!(
            backend.cancels.lock().await.as_slice(),
            &[("run-1".to_string(), "invoke-1".to_string(), 1)]
        );
    }
}
