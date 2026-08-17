//! Production provider for `wamn:flow-invocation@0.1.0`.
//!
//! The provider owns final admission and bounded waiter reconciliation. HTTP
//! adaptation, authentication, mapping, and flow graph execution remain outside
//! this module.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use deadpool_postgres::Pool;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio_postgres::{AsyncMessage, NoTls, Row, Transaction};
use wamn_flow_invocation::{
    Admitted, BeginResult, Failure, FlowError, InvokeRequest, InvokeResult, Rejection, Response,
};
use wamn_run_state::EffectUncertainFailure;
use wamn_run_state::admission::{
    AdmissionProducer, AdmissionResult, AdmissionTransition, RunStateSchema, admission_transaction,
};
use wamn_run_state::invocation::{
    InvocationOutcome, InvocationPoll, InvocationRecovery, InvocationTarget,
    invocation_idempotency_required, lookup_invocation_recovery_sql, poll_invocation_outcome_sql,
    resolve_invocation_target_sql,
};

const EFFECT_UNCERTAIN_HTTP_STATUS: u16 = 502;
const OUTCOME_HINT_CAPACITY: usize = 256;
const OUTCOME_LISTENER_RECONNECT_DELAY: Duration = Duration::from_millis(250);

#[async_trait]
trait OutcomeConnector: Send + Sync {
    async fn listen(
        &self,
        hints: tokio::sync::broadcast::Sender<String>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()>;
}

struct PostgresOutcomeConnector {
    database_url: String,
}

#[async_trait]
impl OutcomeConnector for PostgresOutcomeConnector {
    async fn listen(
        &self,
        hints: tokio::sync::broadcast::Sender<String>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let (client, mut connection) = tokio_postgres::connect(&self.database_url, NoTls)
            .await
            .context("connect invocation outcome listener")?;
        let mut driver = AbortOnDrop(tokio::spawn(async move {
            while let Some(message) =
                futures_util::future::poll_fn(|context| connection.poll_message(context)).await
            {
                match message {
                    Ok(AsyncMessage::Notification(notification)) => {
                        let _ = hints.send(notification.payload().to_string());
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(anyhow!("flow-invocation outcome LISTEN connection closed"))
        }));
        if let Err(error) = client.batch_execute("LISTEN wamn_run_outcome").await {
            driver.abort();
            return Err(error).context("LISTEN wamn_run_outcome");
        }

        let result = tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| anyhow!("outcome listener shutdown owner dropped"))?;
                Ok(())
            }
            result = &mut driver.0 => {
                result.context("join invocation outcome listener connection")?
            }
        };
        driver.abort();
        drop(client);
        result
    }
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> AbortOnDrop<T> {
    fn abort(&self) {
        self.0.abort();
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Clone)]
pub(crate) struct SharedOutcomeListener {
    inner: Arc<SharedOutcomeListenerInner>,
}

struct SharedOutcomeListenerInner {
    hints: tokio::sync::broadcast::Sender<String>,
    shutdown: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SharedOutcomeListenerInner {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

impl SharedOutcomeListener {
    pub(crate) fn postgres(database_url: String) -> Self {
        Self::with_connector(
            Arc::new(PostgresOutcomeConnector { database_url }),
            OUTCOME_LISTENER_RECONNECT_DELAY,
        )
    }

    fn with_connector(connector: Arc<dyn OutcomeConnector>, reconnect_delay: Duration) -> Self {
        let (hints, _) = tokio::sync::broadcast::channel(OUTCOME_HINT_CAPACITY);
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let task_hints = hints.clone();
        let task = tokio::spawn(run_outcome_listener(
            connector,
            task_hints,
            shutdown_receiver,
            reconnect_delay,
        ));
        Self {
            inner: Arc::new(SharedOutcomeListenerInner {
                hints,
                shutdown,
                task,
            }),
        }
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.hints.subscribe()
    }

    #[cfg(test)]
    fn receiver_count(&self) -> usize {
        self.inner.hints.receiver_count()
    }
}

async fn run_outcome_listener(
    connector: Arc<dyn OutcomeConnector>,
    hints: tokio::sync::broadcast::Sender<String>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    reconnect_delay: Duration,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if let Err(error) = connector.listen(hints.clone(), shutdown.clone()).await {
            tracing::warn!(
                error = %error,
                "flow-invocation outcome LISTEN connection failed; reconnecting"
            );
        }
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(reconnect_delay) => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct InvocationServiceConfig {
    pub tenant_id: String,
    pub catalog_id: String,
    pub environment: String,
    pub platform_revision: String,
}

#[derive(Debug, Clone)]
pub struct HttpAdmission {
    pub target: InvocationTarget,
    pub request: InvokeRequest,
    pub principal_digest: String,
    pub client_key_digest: Option<String>,
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
}

#[derive(Clone)]
pub struct InvocationService<B> {
    backend: B,
    outcome_listener: Option<SharedOutcomeListener>,
    config: InvocationServiceConfig,
}

impl<B: InvocationBackend> InvocationService<B> {
    pub fn new(backend: B, config: InvocationServiceConfig) -> Self {
        Self {
            backend,
            outcome_listener: None,
            config,
        }
    }

    pub(crate) fn with_listener(
        backend: B,
        outcome_listener: SharedOutcomeListener,
        config: InvocationServiceConfig,
    ) -> Self {
        Self {
            backend,
            outcome_listener: Some(outcome_listener),
            config,
        }
    }

    pub async fn begin(&self, request: InvokeRequest) -> anyhow::Result<BeginResult> {
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
        if target.idempotency_required && request.idempotency_key.is_none() {
            return Ok(rejected(400, "idempotency-key-required"));
        }

        let principal_digest = digest(request.principal.as_bytes());
        let client_key_digest = request
            .idempotency_key
            .as_deref()
            .map(|client_key| digest(client_key.as_bytes()));
        if let Some(client_key_digest) = client_key_digest.as_deref() {
            let recovery = self
                .backend
                .recover(
                    &self.config.tenant_id,
                    &self.config.catalog_id,
                    &self.config.environment,
                    &request.attachment_id,
                    &principal_digest,
                    client_key_digest,
                    &target.definition_hash,
                    &request.client_request_fingerprint,
                )
                .await?;
            if let Some(result) = recovered_begin(recovery) {
                return Ok(result);
            }
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
                    return Ok(BeginResult::Admitted(Admitted { run_id }));
                }
                AdmissionResult::Duplicate {
                    run_id: Some(run_id),
                } => {
                    return Ok(BeginResult::Admitted(Admitted { run_id }));
                }
                AdmissionResult::Duplicate { run_id: None } => {
                    let Some(client_key_digest) = admission.client_key_digest.as_deref() else {
                        return Ok(rejected(409, "admission-retry"));
                    };
                    let recovery = self
                        .backend
                        .recover(
                            &self.config.tenant_id,
                            &self.config.catalog_id,
                            &self.config.environment,
                            &admission.request.attachment_id,
                            &admission.principal_digest,
                            client_key_digest,
                            &admission.target.definition_hash,
                            &admission.request.client_request_fingerprint,
                        )
                        .await?;
                    return Ok(recovered_begin(recovery)
                        .unwrap_or_else(|| rejected(409, "admission-retry")));
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
                            if next.idempotency_required
                                && admission.request.idempotency_key.is_none()
                            {
                                return Ok(rejected(400, "idempotency-key-required"));
                            }
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
        // Subscribe before the first poll so a completion between observation
        // and waiting cannot be lost. Hints only shorten the bounded wait.
        let mut notifications = self
            .outcome_listener
            .as_ref()
            .map(SharedOutcomeListener::subscribe);
        if let Some(outcome) = released(self.backend.poll(&self.config.tenant_id, &run_id).await?)?
        {
            return Ok(Some(to_invoke_result(outcome)?));
        }

        if let Some(notifications) = notifications.as_mut() {
            let expected = format!("{}:{run_id}", self.config.tenant_id);
            let deadline = tokio::time::sleep(Duration::from_millis(u64::from(timeout_ms)));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    message = notifications.recv() => {
                        match message {
                            Ok(payload) if payload == expected => break,
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
                            | Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
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
        let run_schema = RunStateSchema::default();
        let recipe = admission_transaction(AdmissionTransition::CallableFlow {
            schema: &run_schema,
        });
        transaction
            .query_one(
                recipe.lock_head(),
                &[&config.catalog_id, &config.environment],
            )
            .await?;

        let producer = AdmissionProducer::Http.as_sql();
        let catalog_version = admission.target.catalog_version;
        let flow_version = admission.target.flow_version;
        let input = serde_json::to_string(&admission.input)?;
        let context = serde_json::to_string(&admission.invocation_context)?;
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
                    &none_text,
                    &none_i64,
                    &none_text,
                    &none_text,
                    &none_text,
                    &none_text,
                    &none_i32,
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
}

fn decode_target(row: Row) -> anyhow::Result<InvocationTarget> {
    let execution_bundle_hash: String = row.get(7);
    let exact_bytes: Option<Vec<u8>> = row.get(8);
    Ok(InvocationTarget {
        catalog_version: row.get(0),
        definition_hash: row.get(1),
        flow_id: row.get(2),
        flow_version: row.get(3),
        definition: serde_json::from_str(row.get(4))?,
        auth_policy: serde_json::from_str(row.get(5))?,
        enabled: row.get(6),
        idempotency_required: exact_bytes.as_deref().is_none_or(|exact_bytes| {
            invocation_idempotency_required(&execution_bundle_hash, exact_bytes)
        }),
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

fn recovered_begin(recovery: InvocationRecovery) -> Option<BeginResult> {
    match recovery {
        InvocationRecovery::Released(outcome) => Some(BeginResult::Admitted(Admitted {
            run_id: outcome.run_id,
        })),
        InvocationRecovery::InFlight { run_id } => Some(BeginResult::Admitted(Admitted { run_id })),
        InvocationRecovery::IdempotencyKeyReused => Some(rejected(409, "idempotency-key-reused")),
        InvocationRecovery::IdempotencyScopeChanged => {
            Some(rejected(409, "idempotency-scope-changed"))
        }
        InvocationRecovery::Missing => None,
    }
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
        AdmissionResult::MissingRootPlan => (409, "missing-root-plan"),
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
        "failed" => {
            if outcome.body.get("code").and_then(Value::as_str) == Some("effect-uncertain") {
                let stored = serde_json::from_value::<EffectUncertainFailure>(outcome.body)?;
                if stored.run_id() != outcome.run_id {
                    return Err(anyhow!(
                        "stored effect-uncertain run id does not match outcome"
                    ));
                }
                return Ok(InvokeResult::Failed(Failure {
                    status: EFFECT_UNCERTAIN_HTTP_STATUS,
                    error: FlowError {
                        code: stored.code().to_string(),
                        message: None,
                        run_id: stored.run_id().to_string(),
                        flow_id: outcome.flow_id,
                        flow_version: outcome.flow_version,
                    },
                }));
            }
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
            Ok(InvokeResult::Failed(Failure {
                status: outcome
                    .http_status
                    .ok_or_else(|| anyhow!("stored failure missing HTTP status"))?,
                error: flow_error,
            }))
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use tokio::sync::Mutex;
    use wamn_flow_invocation::TraceContext;

    #[derive(Clone, Default)]
    struct MockBackend {
        targets: Arc<Mutex<VecDeque<Option<InvocationTarget>>>>,
        recoveries: Arc<Mutex<VecDeque<InvocationRecovery>>>,
        admissions: Arc<Mutex<VecDeque<AdmissionResult>>>,
        admitted_versions: Arc<Mutex<Vec<i32>>>,
        admitted_client_keys: Arc<Mutex<Vec<Option<String>>>>,
        polls: Arc<Mutex<VecDeque<InvocationPoll>>>,
    }

    struct CountingConnector {
        starts: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        fail_first: AtomicBool,
        payloads: Vec<String>,
    }

    impl CountingConnector {
        fn new(payloads: Vec<String>, fail_first: bool) -> Self {
            Self {
                starts: Arc::new(AtomicUsize::new(0)),
                active: Arc::new(AtomicUsize::new(0)),
                max_active: Arc::new(AtomicUsize::new(0)),
                fail_first: AtomicBool::new(fail_first),
                payloads,
            }
        }
    }

    struct ActiveConnection(Arc<AtomicUsize>);

    impl Drop for ActiveConnection {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl OutcomeConnector for CountingConnector {
        async fn listen(
            &self,
            hints: tokio::sync::broadcast::Sender<String>,
            mut shutdown: tokio::sync::watch::Receiver<bool>,
        ) -> anyhow::Result<()> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            let _active = ActiveConnection(Arc::clone(&self.active));
            if self.fail_first.swap(false, Ordering::SeqCst) {
                return Err(anyhow!("injected listener disconnect"));
            }
            while hints.receiver_count() < self.payloads.len() {
                tokio::task::yield_now().await;
            }
            for payload in &self.payloads {
                let _ = hints.send(payload.clone());
            }
            shutdown
                .changed()
                .await
                .map_err(|_| anyhow!("test listener shutdown owner dropped"))?;
            Ok(())
        }
    }

    async fn eventually(predicate: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("condition did not become true");
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
            self.admitted_client_keys
                .lock()
                .await
                .push(admission.client_key_digest.clone());
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
    }

    fn config() -> InvocationServiceConfig {
        InvocationServiceConfig {
            tenant_id: "tenant-a".to_string(),
            catalog_id: "catalog-a".to_string(),
            environment: "prod".to_string(),
            platform_revision: "rev-test".to_string(),
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
            idempotency_required: true,
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

    fn effect_uncertain_outcome() -> InvocationOutcome {
        InvocationOutcome {
            run_id: "run-1".to_string(),
            kind: "failed".to_string(),
            body: json!({"code": "effect-uncertain", "run_id": "run-1"}),
            http_status: Some(500),
            hash: "sha256:uncertain".to_string(),
            flow_id: "flow-a".to_string(),
            flow_version: 3,
        }
    }

    #[test]
    fn missing_root_plan_maps_to_its_frozen_http_refusal() {
        assert_eq!(
            admission_refusal(AdmissionResult::MissingRootPlan),
            rejected(409, "missing-root-plan")
        );
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
        let service = InvocationService::new(backend.clone(), config());

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
        let service = InvocationService::new(backend.clone(), config());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            rejected(404, "attachment-disabled")
        );
        assert!(backend.admissions.lock().await.is_empty());
    }

    // Enablement is decided inside the admission transaction, so a resolution
    // that says nothing about activation still refuses with the frozen
    // `attachment-disabled` literal. This is the leg that has to survive route
    // bytes moving to a mounted manifest: the pre-admission check above may go
    // with `InvocationTarget::enabled`, the transaction's refusal may not.
    #[tokio::test]
    async fn admission_transaction_refusal_still_reads_attachment_disabled() {
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
            .push_back(AdmissionResult::InactiveDefinition);
        let service = InvocationService::new(backend.clone(), config());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            rejected(404, "attachment-disabled")
        );
        assert_eq!(backend.admitted_versions.lock().await.as_slice(), &[8]);
    }

    // FLOW-SPEC rev18 §6.2's order, stated without reference to the resolved
    // target's activation: a released outcome is returned before the admission
    // transaction runs, so the transaction's enablement refusal can never take
    // a released run away from its caller.
    #[tokio::test]
    async fn released_outcome_precedes_the_admission_transaction_activation_check() {
        let backend = MockBackend::default();
        backend.targets.lock().await.push_back(Some(target(true)));
        backend
            .recoveries
            .lock()
            .await
            .push_back(InvocationRecovery::Released(outcome("responded", "", 200)));
        backend
            .admissions
            .lock()
            .await
            .push_back(AdmissionResult::InactiveDefinition);
        let service = InvocationService::new(backend.clone(), config());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            BeginResult::Admitted(Admitted {
                run_id: "run-1".to_string()
            })
        );
        assert!(backend.admitted_versions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn unchanged_promotion_retries_admission() {
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
        let service = InvocationService::new(backend.clone(), config());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            BeginResult::Admitted(Admitted {
                run_id: "run-promoted".to_string()
            })
        );
        assert_eq!(backend.admitted_versions.lock().await.as_slice(), &[8, 9]);
    }

    #[tokio::test]
    async fn promotion_cannot_add_a_key_requirement_after_a_keyless_preflight() {
        let backend = MockBackend::default();
        backend.targets.lock().await.extend([
            Some(InvocationTarget {
                idempotency_required: false,
                ..target(true)
            }),
            Some(InvocationTarget {
                catalog_version: 9,
                idempotency_required: true,
                ..target(true)
            }),
        ]);
        backend
            .admissions
            .lock()
            .await
            .push_back(AdmissionResult::HeadDrift);
        let service = InvocationService::new(backend.clone(), config());
        let mut without_key = request();
        without_key.idempotency_key = None;

        assert_eq!(
            service.begin(without_key).await.unwrap(),
            rejected(400, "idempotency-key-required")
        );
        assert_eq!(backend.admitted_versions.lock().await.as_slice(), &[8]);
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
        ] {
            let backend = MockBackend::default();
            backend.targets.lock().await.push_back(Some(target(true)));
            backend.recoveries.lock().await.push_back(recovery);
            let service = InvocationService::new(backend.clone(), config());
            let BeginResult::Rejected(rejection) = service.begin(request()).await.unwrap() else {
                panic!("conflict must reject before admission");
            };
            assert_eq!(rejection.code, code);
            assert!(backend.admissions.lock().await.is_empty());
        }
    }

    #[tokio::test]
    async fn in_flight_recovery_returns_the_same_run_without_admission() {
        let backend = MockBackend::default();
        backend.targets.lock().await.push_back(Some(target(true)));
        backend
            .recoveries
            .lock()
            .await
            .push_back(InvocationRecovery::InFlight {
                run_id: "run-live".to_string(),
            });
        let service = InvocationService::new(backend.clone(), config());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            BeginResult::Admitted(Admitted {
                run_id: "run-live".to_string()
            })
        );
        assert!(backend.admissions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn concurrent_duplicate_recovers_the_winning_run_after_insert_visibility() {
        let backend = MockBackend::default();
        backend.targets.lock().await.push_back(Some(target(true)));
        backend.recoveries.lock().await.extend([
            InvocationRecovery::Missing,
            InvocationRecovery::InFlight {
                run_id: "run-winner".to_string(),
            },
        ]);
        backend
            .admissions
            .lock()
            .await
            .push_back(AdmissionResult::Duplicate { run_id: None });
        let service = InvocationService::new(backend.clone(), config());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            BeginResult::Admitted(Admitted {
                run_id: "run-winner".to_string()
            })
        );
        assert_eq!(backend.admitted_versions.lock().await.as_slice(), &[8]);
    }

    #[tokio::test]
    async fn admission_visible_duplicate_returns_the_winning_run() {
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
            .push_back(AdmissionResult::Duplicate {
                run_id: Some("run-winner".to_string()),
            });
        let service = InvocationService::new(backend.clone(), config());

        assert_eq!(
            service.begin(request()).await.unwrap(),
            BeginResult::Admitted(Admitted {
                run_id: "run-winner".to_string()
            })
        );
        assert_eq!(backend.admitted_versions.lock().await.as_slice(), &[8]);
    }

    #[tokio::test]
    async fn own_plan_requirement_allows_only_pure_call_free_requests_without_a_key() {
        let required_backend = MockBackend::default();
        required_backend
            .targets
            .lock()
            .await
            .push_back(Some(target(true)));
        let required_service = InvocationService::new(required_backend.clone(), config());
        let mut without_key = request();
        without_key.idempotency_key = None;

        assert_eq!(
            required_service.begin(without_key.clone()).await.unwrap(),
            rejected(400, "idempotency-key-required")
        );
        assert!(required_backend.admissions.lock().await.is_empty());

        let pure_backend = MockBackend::default();
        pure_backend
            .targets
            .lock()
            .await
            .push_back(Some(InvocationTarget {
                idempotency_required: false,
                ..target(true)
            }));
        pure_backend
            .admissions
            .lock()
            .await
            .push_back(AdmissionResult::Admitted {
                run_id: "run-pure".to_string(),
            });
        let pure_service = InvocationService::new(pure_backend.clone(), config());

        assert_eq!(
            pure_service.begin(without_key).await.unwrap(),
            BeginResult::Admitted(Admitted {
                run_id: "run-pure".to_string()
            })
        );
        assert_eq!(
            pure_backend.admitted_client_keys.lock().await.as_slice(),
            &[None]
        );
    }

    #[tokio::test]
    async fn lost_notification_falls_back_to_the_final_bounded_poll() {
        let backend = MockBackend::default();
        backend.polls.lock().await.extend([
            InvocationPoll::Running,
            InvocationPoll::Released(outcome("responded", "", 202)),
        ]);
        let service = InvocationService::new(backend, config());

        assert!(matches!(
            service.wait("run-1".to_string(), 0).await.unwrap(),
            Some(InvokeResult::Responded(Response {
                status_hint: Some(202),
                ..
            }))
        ));
    }

    #[tokio::test]
    async fn concurrent_waiters_share_one_listener_connection_and_receive_broadcast_hints() {
        const WAITER_COUNT: usize = 8;
        let connector = Arc::new(CountingConnector::new(
            (0..WAITER_COUNT)
                .map(|index| format!("tenant-a:run-{index}"))
                .collect(),
            false,
        ));
        let listener =
            SharedOutcomeListener::with_connector(connector.clone(), Duration::from_millis(1));
        let mut waits = Vec::new();
        for index in 0..WAITER_COUNT {
            let backend = MockBackend::default();
            backend.polls.lock().await.extend([
                InvocationPoll::Running,
                InvocationPoll::Released(outcome("responded", "", 200)),
            ]);
            let service = InvocationService::with_listener(backend, listener.clone(), config());
            waits.push(tokio::spawn(async move {
                service.wait(format!("run-{index}"), 5_000).await
            }));
        }

        for wait in waits {
            assert!(wait.await.unwrap().unwrap().is_some());
        }
        assert_eq!(connector.starts.load(Ordering::SeqCst), 1);
        assert_eq!(connector.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(connector.active.load(Ordering::SeqCst), 1);
        drop(listener);
        eventually(|| connector.active.load(Ordering::SeqCst) == 0).await;
    }

    #[tokio::test]
    async fn cancelling_a_wait_drops_only_its_subscription_and_plugin_drop_stops_listener() {
        let connector = Arc::new(CountingConnector::new(Vec::new(), false));
        let listener =
            SharedOutcomeListener::with_connector(connector.clone(), Duration::from_millis(1));
        let backend = MockBackend::default();
        backend
            .polls
            .lock()
            .await
            .push_back(InvocationPoll::Running);
        let service = InvocationService::with_listener(backend, listener.clone(), config());
        let wait = tokio::spawn(async move { service.wait("run-1".to_string(), 60_000).await });

        eventually(|| listener.receiver_count() == 1).await;
        eventually(|| connector.active.load(Ordering::SeqCst) == 1).await;
        wait.abort();
        assert!(wait.await.unwrap_err().is_cancelled());
        eventually(|| listener.receiver_count() == 0).await;
        assert_eq!(connector.active.load(Ordering::SeqCst), 1);

        drop(listener);
        eventually(|| connector.active.load(Ordering::SeqCst) == 0).await;
    }

    #[tokio::test]
    async fn listener_disconnect_drops_the_old_connection_before_reconnect() {
        let connector = Arc::new(CountingConnector::new(Vec::new(), true));
        let listener =
            SharedOutcomeListener::with_connector(connector.clone(), Duration::from_millis(1));

        eventually(|| connector.starts.load(Ordering::SeqCst) >= 2).await;
        assert_eq!(connector.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(connector.active.load(Ordering::SeqCst), 1);
        drop(listener);
        eventually(|| connector.active.load(Ordering::SeqCst) == 0).await;
    }

    #[test]
    fn plugin_configures_exactly_pool_sixteen_plus_one_shared_listener_from_one_url() {
        let plugin = include_str!("plugins/wamn_flow_invocation.rs");
        assert_eq!(plugin.matches(".max_size(16)").count(), 1);
        assert_eq!(plugin.matches("SharedOutcomeListener::postgres").count(), 1);
        assert_eq!(plugin.matches("WAMN_RUN_STORE_PG_URL").count(), 1);
        assert!(!plugin.contains("WAMN_RUN_STORE_LISTENER_PG_URL"));
        assert!(plugin.contains(
            "let backend = database_url\n            .as_deref()\n            .map(build_pool)"
        ));
        assert!(
            plugin.contains(
                "let outcome_listener = database_url.map(SharedOutcomeListener::postgres);"
            )
        );
    }

    #[test]
    fn all_release_paths_decode_the_exact_stored_winner() {
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
                "depth-budget",
                outcome("failed", "depth-budget", 500),
                ("failed", 500),
            ),
            (
                "dispatch-budget",
                outcome("failed", "dispatch-budget", 500),
                ("failed", 500),
            ),
            (
                "effect-uncertain",
                effect_uncertain_outcome(),
                ("failed", 502),
            ),
        ] {
            let result = to_invoke_result(stored).unwrap();
            let actual = match result {
                InvokeResult::Responded(response) => ("responded", response.status_hint.unwrap()),
                InvokeResult::Failed(failure) => ("failed", failure.status),
            };
            assert_eq!(actual, expected, "{path}");
        }
    }

    #[test]
    fn effect_uncertain_decodes_only_the_non_committal_stored_identity() {
        let InvokeResult::Failed(failure) = to_invoke_result(effect_uncertain_outcome()).unwrap()
        else {
            panic!("effect uncertainty must remain a failed caller outcome");
        };
        assert_eq!(failure.status, 502);
        assert_eq!(failure.error.code, "effect-uncertain");
        assert_eq!(failure.error.run_id, "run-1");
        assert_eq!(failure.error.message, None);
    }
}
