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
    Admitted, BeginResult, Failure, FlowError, InvocationError, InvokeRequest, InvokeResult,
    Rejection, Response,
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

/// Aborts its task when explicitly cancelled or when its sole owner is dropped.
pub(crate) struct AbortOnDrop<T>(pub(crate) tokio::task::JoinHandle<T>);

impl<T> AbortOnDrop<T> {
    pub(crate) fn abort(&self) {
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

/// A host-side invocation failure and the context that produced it.
///
/// [`InvocationFailure::kind`] is the WIT-shaped arm the plugin boundary answers
/// with; `operation`, `field`, `run_id` and `source` stay host-side for the log
/// and never reach the caller, so a failure cannot leak run-store internals
/// (wamn-0h0g.15.40). This is not a second wire taxonomy: `kind` is the frozen
/// contract's own [`InvocationError`], translated exactly once at the plugin.
#[derive(Debug)]
pub struct InvocationFailure {
    kind: InvocationError,
    operation: &'static str,
    field: Option<&'static str>,
    run_id: Option<String>,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl InvocationFailure {
    /// The contract arm this failure is answered with.
    pub fn kind(&self) -> InvocationError {
        self.kind
    }

    fn new(kind: InvocationError, operation: &'static str) -> Self {
        Self {
            kind,
            operation,
            field: None,
            run_id: None,
            source: None,
        }
    }

    fn with_field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }

    fn with_run(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
}

impl std::fmt::Display for InvocationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "flow invocation {} failed: {:?}",
            self.operation, self.kind
        )?;
        if let Some(field) = self.field {
            write!(formatter, " (field {field})")?;
        }
        if let Some(run_id) = &self.run_id {
            write!(formatter, " (run {run_id})")?;
        }
        Ok(())
    }
}

impl std::error::Error for InvocationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// The run store is unreachable or refused the transaction; retryable.
fn store_unavailable(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> InvocationFailure {
    InvocationFailure::new(InvocationError::StoreUnavailable, operation).with_source(source)
}

/// A stored row cannot be decoded into the versioned contract; terminal.
fn store_corrupt(operation: &'static str) -> InvocationFailure {
    InvocationFailure::new(InvocationError::StoreCorrupt, operation)
}

/// The arguments as presented are refused rather than truncated; terminal.
fn invalid_request(operation: &'static str) -> InvocationFailure {
    InvocationFailure::new(InvocationError::InvalidRequest, operation)
}

#[async_trait]
pub trait InvocationBackend: Clone + Send + Sync + 'static {
    async fn resolve_target(
        &self,
        tenant: &str,
        catalog: &str,
        environment: &str,
        attachment: &str,
    ) -> Result<Option<InvocationTarget>, InvocationFailure>;

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
    ) -> Result<InvocationRecovery, InvocationFailure>;

    async fn admit(
        &self,
        config: &InvocationServiceConfig,
        admission: &HttpAdmission,
    ) -> Result<AdmissionResult, InvocationFailure>;

    async fn poll(&self, tenant: &str, run_id: &str) -> Result<InvocationPoll, InvocationFailure>;
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

    pub async fn begin(&self, request: InvokeRequest) -> Result<BeginResult, InvocationFailure> {
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
            .map_err(|_| invalid_request("narrow expected catalog version"))?;
        if target.catalog_version != expected_catalog_version {
            // A promotion carrying an unchanged attachment is safe to retry:
            // the definition hash includes the resolved auth source.
            if target.definition_hash != request.expected_definition_hash {
                return Ok(rejected(409, "admission-retry"));
            }
        }

        let input: Value = serde_json::from_str(&request.payload)
            .map_err(|source| invalid_request("parse mapped payload").with_source(source))?;
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
    ) -> Result<Option<InvokeResult>, InvocationFailure> {
        // Subscribe before the first poll so a completion between observation
        // and waiting cannot be lost. Hints only shorten the bounded wait.
        let mut notifications = self
            .outcome_listener
            .as_ref()
            .map(SharedOutcomeListener::subscribe);
        if let Some(outcome) = released(
            self.backend.poll(&self.config.tenant_id, &run_id).await?,
            &run_id,
        )? {
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
        released(
            self.backend.poll(&self.config.tenant_id, &run_id).await?,
            &run_id,
        )?
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

    async fn transaction(&self) -> Result<deadpool_postgres::Object, InvocationFailure> {
        self.pool
            .get()
            .await
            .map_err(|source| store_unavailable("checkout invocation database", source))
    }
}

async fn set_tenant(transaction: &Transaction<'_>, tenant: &str) -> Result<(), InvocationFailure> {
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .map_err(|source| store_unavailable("set tenant scope", source))?;
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
    ) -> Result<Option<InvocationTarget>, InvocationFailure> {
        let mut client = self.transaction().await?;
        let transaction = client
            .transaction()
            .await
            .map_err(|source| store_unavailable("open resolve transaction", source))?;
        set_tenant(&transaction, tenant).await?;
        let row = transaction
            .query_opt(
                resolve_invocation_target_sql(),
                &[&catalog, &environment, &attachment],
            )
            .await
            .map_err(|source| store_unavailable("resolve invocation target", source))?;
        transaction
            .commit()
            .await
            .map_err(|source| store_unavailable("commit resolve transaction", source))?;
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
    ) -> Result<InvocationRecovery, InvocationFailure> {
        let mut client = self.transaction().await?;
        let transaction = client
            .transaction()
            .await
            .map_err(|source| store_unavailable("open recovery transaction", source))?;
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
            .await
            .map_err(|source| store_unavailable("look up invocation recovery", source))?;
        transaction
            .commit()
            .await
            .map_err(|source| store_unavailable("commit recovery transaction", source))?;
        decode_recovery(&row)
    }

    async fn admit(
        &self,
        config: &InvocationServiceConfig,
        admission: &HttpAdmission,
    ) -> Result<AdmissionResult, InvocationFailure> {
        let mut client = self.transaction().await?;
        let transaction = client
            .transaction()
            .await
            .map_err(|source| store_unavailable("open admission transaction", source))?;
        set_tenant(&transaction, &config.tenant_id).await?;
        let run_id: String = transaction
            .query_one("SELECT gen_random_uuid()::text", &[])
            .await
            .map_err(|source| store_unavailable("mint run id", source))?
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
            .await
            .map_err(|source| store_unavailable("lock catalog head", source))?;

        let producer = AdmissionProducer::Http.as_sql();
        let catalog_version = admission.target.catalog_version;
        let flow_version = admission.target.flow_version;
        let input = serde_json::to_string(&admission.input)
            .map_err(|source| invalid_request("serialize admission input").with_source(source))?;
        let context = serde_json::to_string(&admission.invocation_context)
            .map_err(|source| invalid_request("serialize admission context").with_source(source))?;
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
            .await
            .map_err(|source| store_unavailable("admit callable run", source))?;
        let result = AdmissionResult::from_parts(row.get(0), row.get(1))
            .ok_or_else(|| store_corrupt("read admission result").with_field("result-code"))?;
        transaction
            .commit()
            .await
            .map_err(|source| store_unavailable("commit admission transaction", source))?;
        Ok(result)
    }

    async fn poll(&self, tenant: &str, run_id: &str) -> Result<InvocationPoll, InvocationFailure> {
        let mut client = self.transaction().await?;
        let transaction = client
            .transaction()
            .await
            .map_err(|source| store_unavailable("open poll transaction", source))?;
        set_tenant(&transaction, tenant).await?;
        let row = transaction
            .query_one(poll_invocation_outcome_sql(), &[&run_id])
            .await
            .map_err(|source| store_unavailable("poll invocation outcome", source))?;
        transaction
            .commit()
            .await
            .map_err(|source| store_unavailable("commit poll transaction", source))?;
        decode_poll(&row)
    }
}

fn decode_target(row: Row) -> Result<InvocationTarget, InvocationFailure> {
    let execution_bundle_hash: String = row.get(7);
    let exact_bytes: Option<Vec<u8>> = row.get(8);
    Ok(InvocationTarget {
        catalog_version: row.get(0),
        definition_hash: row.get(1),
        flow_id: row.get(2),
        flow_version: row.get(3),
        definition: serde_json::from_str(row.get(4)).map_err(|source| {
            store_corrupt("decode resolved target")
                .with_field("definition-json")
                .with_source(source)
        })?,
        auth_policy: serde_json::from_str(row.get(5)).map_err(|source| {
            store_corrupt("decode resolved target")
                .with_field("auth-policy-json")
                .with_source(source)
        })?,
        enabled: row.get(6),
        idempotency_required: exact_bytes.as_deref().is_none_or(|exact_bytes| {
            invocation_idempotency_required(&execution_bundle_hash, exact_bytes)
        }),
    })
}

fn decode_outcome(row: &Row) -> Result<InvocationOutcome, InvocationFailure> {
    let missing = |field: &'static str| store_corrupt("decode released outcome").with_field(field);
    let body: String = row
        .get::<_, Option<String>>(3)
        .ok_or_else(|| missing("body"))?;
    let status = row
        .get::<_, Option<i32>>(4)
        .map(u16::try_from)
        .transpose()
        .map_err(|_| store_corrupt("narrow released outcome").with_field("caller-http-status"))?;
    Ok(InvocationOutcome {
        run_id: row
            .get::<_, Option<String>>(1)
            .ok_or_else(|| missing("run-id"))?,
        kind: row
            .get::<_, Option<String>>(2)
            .ok_or_else(|| missing("kind"))?,
        body: serde_json::from_str(&body).map_err(|source| missing("body").with_source(source))?,
        http_status: status,
        hash: row
            .get::<_, Option<String>>(5)
            .ok_or_else(|| missing("hash"))?,
        flow_id: row
            .get::<_, Option<String>>(6)
            .ok_or_else(|| missing("flow-id"))?,
        flow_version: u32::try_from(
            row.get::<_, Option<i32>>(7)
                .ok_or_else(|| missing("flow-version"))?,
        )
        .map_err(|_| store_corrupt("narrow released outcome").with_field("flow-version"))?,
    })
}

fn decode_recovery(row: &Row) -> Result<InvocationRecovery, InvocationFailure> {
    Ok(match row.get::<_, &str>(0) {
        "missing" => InvocationRecovery::Missing,
        "in-flight" => InvocationRecovery::InFlight {
            run_id: row
                .get::<_, Option<String>>(1)
                .ok_or_else(|| store_corrupt("decode in-flight admission").with_field("run-id"))?,
        },
        "released" => InvocationRecovery::Released(decode_outcome(row)?),
        "idempotency-key-reused" => InvocationRecovery::IdempotencyKeyReused,
        "idempotency-scope-changed" => InvocationRecovery::IdempotencyScopeChanged,
        // The offending literal is logged rather than returned: the contract's
        // failure arms deliberately carry no detail (wamn-0h0g.15.40).
        code => {
            tracing::warn!(code = %code, "invalid invocation recovery result");
            return Err(store_corrupt("decode invocation recovery").with_field("result-code"));
        }
    })
}

fn decode_poll(row: &Row) -> Result<InvocationPoll, InvocationFailure> {
    Ok(match row.get::<_, &str>(0) {
        "running" => InvocationPoll::Running,
        "released" => InvocationPoll::Released(decode_outcome(row)?),
        "not-found" => InvocationPoll::NotFound,
        code => {
            tracing::warn!(code = %code, "invalid invocation poll result");
            return Err(store_corrupt("decode invocation poll").with_field("result-code"));
        }
    })
}

/// A poll that names no run is the caller's `unknown-run`, not a host defect:
/// the run id came from the caller, and inventing a still-unreleased `none` for
/// it would make the adapter wait out its whole budget for nothing.
fn released(
    poll: InvocationPoll,
    run_id: &str,
) -> Result<Option<InvocationOutcome>, InvocationFailure> {
    match poll {
        InvocationPoll::Running => Ok(None),
        InvocationPoll::Released(outcome) => Ok(Some(outcome)),
        InvocationPoll::NotFound => Err(InvocationFailure::new(
            InvocationError::UnknownRun,
            "poll invocation outcome",
        )
        .with_run(run_id)),
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
) -> Result<(Option<DateTime<Utc>>, DateTime<Utc>), InvocationFailure> {
    let run_ms = target
        .definition
        .get("run-deadline-ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            store_corrupt("read stored attachment definition").with_field("run-deadline-ms")
        })?;
    let run_ms = request
        .deadline_override
        .map_or(run_ms, |limit| limit.min(run_ms));
    let response_ms = target
        .definition
        .get("response-deadline-ms")
        .and_then(Value::as_u64)
        .map(|value| value.min(run_ms));
    let now = Utc::now();
    let run = now
        + TimeDelta::milliseconds(i64::try_from(run_ms).map_err(|_| {
            store_corrupt("narrow stored attachment definition").with_field("run-deadline-ms")
        })?);
    let response = response_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| {
            store_corrupt("narrow stored attachment definition").with_field("response-deadline-ms")
        })?
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

fn to_invoke_result(outcome: InvocationOutcome) -> Result<InvokeResult, InvocationFailure> {
    match outcome.kind.as_str() {
        "responded" => Ok(InvokeResult::Responded(Response {
            run_id: outcome.run_id,
            body: serde_json::to_string(&outcome.body).map_err(|source| {
                store_corrupt("re-encode stored response")
                    .with_field("caller-outcome-json")
                    .with_source(source)
            })?,
            status_hint: outcome.http_status,
        })),
        "failed" => {
            if outcome.body.get("code").and_then(Value::as_str) == Some("effect-uncertain") {
                let stored = serde_json::from_value::<EffectUncertainFailure>(outcome.body)
                    .map_err(|source| {
                        store_corrupt("decode stored effect uncertainty").with_source(source)
                    })?;
                if stored.run_id() != outcome.run_id {
                    return Err(
                        store_corrupt("decode stored effect uncertainty").with_field("run-id")
                    );
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
                .ok_or_else(|| stored_failure_missing("error"))?;
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
                    .ok_or_else(|| stored_failure_missing("flow-version"))?
                    .try_into()
                    .map_err(|_| {
                        store_corrupt("narrow stored failure").with_field("flow-version")
                    })?,
            };
            Ok(InvokeResult::Failed(Failure {
                status: outcome
                    .http_status
                    .ok_or_else(|| stored_failure_missing("caller-http-status"))?,
                error: flow_error,
            }))
        }
        // The offending literal is logged rather than returned: the contract's
        // failure arms deliberately carry no detail (wamn-0h0g.15.40).
        kind => {
            tracing::warn!(kind = %kind, "unknown stored caller outcome kind");
            Err(store_corrupt("decode stored caller outcome").with_field("caller-outcome-kind"))
        }
    }
}

fn stored_failure_missing(field: &'static str) -> InvocationFailure {
    store_corrupt("read stored failure").with_field(field)
}

fn required_string(value: &Value, key: &'static str) -> Result<String, InvocationFailure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| stored_failure_missing(key))
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

    struct AbortWitness(Arc<AtomicBool>);

    impl Drop for AbortWitness {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn pending_abort_owner(started: Arc<AtomicBool>, aborted: Arc<AtomicBool>) -> AbortOnDrop<()> {
        AbortOnDrop(tokio::spawn(async move {
            let _witness = AbortWitness(aborted);
            started.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }))
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

    #[test]
    fn abort_on_drop_has_one_crate_owner() {
        let flow_invocation = include_str!("flow_invocation.rs");
        let wiring_doorbell = include_str!("wiring_doorbell.rs");
        let definition = ["struct ", "AbortOnDrop", "<T>"].concat();

        assert_eq!(flow_invocation.matches(&definition).count(), 1);
        assert_eq!(wiring_doorbell.matches(&definition).count(), 0);
    }

    #[tokio::test]
    async fn dropping_abort_on_drop_aborts_the_task() {
        let started = Arc::new(AtomicBool::new(false));
        let aborted = Arc::new(AtomicBool::new(false));
        let owner = pending_abort_owner(Arc::clone(&started), Arc::clone(&aborted));
        eventually(|| started.load(Ordering::SeqCst)).await;

        drop(owner);

        eventually(|| aborted.load(Ordering::SeqCst)).await;
    }

    #[tokio::test]
    async fn explicit_abort_aborts_the_task() {
        let started = Arc::new(AtomicBool::new(false));
        let aborted = Arc::new(AtomicBool::new(false));
        let owner = pending_abort_owner(Arc::clone(&started), Arc::clone(&aborted));
        eventually(|| started.load(Ordering::SeqCst)).await;

        owner.abort();

        eventually(|| aborted.load(Ordering::SeqCst)).await;
    }

    #[async_trait]
    impl InvocationBackend for MockBackend {
        async fn resolve_target(
            &self,
            _tenant: &str,
            _catalog: &str,
            _environment: &str,
            _attachment: &str,
        ) -> Result<Option<InvocationTarget>, InvocationFailure> {
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
        ) -> Result<InvocationRecovery, InvocationFailure> {
            Ok(self
                .recoveries
                .lock()
                .await
                .pop_front()
                .expect("missing recovery fixture"))
        }

        async fn admit(
            &self,
            _config: &InvocationServiceConfig,
            admission: &HttpAdmission,
        ) -> Result<AdmissionResult, InvocationFailure> {
            self.admitted_versions
                .lock()
                .await
                .push(admission.target.catalog_version);
            self.admitted_client_keys
                .lock()
                .await
                .push(admission.client_key_digest.clone());
            Ok(self
                .admissions
                .lock()
                .await
                .pop_front()
                .expect("missing admission fixture"))
        }

        async fn poll(
            &self,
            _tenant: &str,
            _run_id: &str,
        ) -> Result<InvocationPoll, InvocationFailure> {
            Ok(self
                .polls
                .lock()
                .await
                .pop_front()
                .expect("missing poll fixture"))
        }
    }

    /// A backend that cannot answer at all — the case that used to trap the
    /// guest, because the contract had nowhere to report it (wamn-0h0g.15.40).
    #[derive(Clone)]
    struct FailingBackend(InvocationError);

    #[async_trait]
    impl InvocationBackend for FailingBackend {
        async fn resolve_target(
            &self,
            _tenant: &str,
            _catalog: &str,
            _environment: &str,
            _attachment: &str,
        ) -> Result<Option<InvocationTarget>, InvocationFailure> {
            Err(InvocationFailure::new(self.0, "test resolve target"))
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
        ) -> Result<InvocationRecovery, InvocationFailure> {
            Err(InvocationFailure::new(self.0, "test recover"))
        }

        async fn admit(
            &self,
            _config: &InvocationServiceConfig,
            _admission: &HttpAdmission,
        ) -> Result<AdmissionResult, InvocationFailure> {
            Err(InvocationFailure::new(self.0, "test admit"))
        }

        async fn poll(
            &self,
            _tenant: &str,
            _run_id: &str,
        ) -> Result<InvocationPoll, InvocationFailure> {
            Err(InvocationFailure::new(self.0, "test poll"))
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
        // wamn-0h0g.12.114: the host builds this pool and the wamn:postgres pool
        // from two independent chains, 42 lines apart in one function. They agreed
        // only because the variable they ordered differently — the generic
        // `DATABASE_URL`, which base images, operators and sidecars inject
        // unbidden — happened to be set nowhere; nothing enforced the agreement.
        // deploy/ wires WAMN_PG_URL only, so this assertion IS the invariant:
        // neither pool may reacquire an ambient credential name. The count above
        // is the other half — it fails if the deliberate WAMN_RUN_STORE_PG_URL
        // override is what gets deleted instead.
        assert!(!plugin.contains("DATABASE_URL"));
        let pool = include_str!("plugins/wamn_postgres/pool.rs");
        assert!(!pool.contains("DATABASE_URL"));
        assert_eq!(pool.matches("WAMN_PG_URL").count(), 1);
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

    // The gap wamn-0h0g.15.40 closes: a store outage had nowhere to go, so both
    // operations converted it into a wasm trap that poisoned the flow-http
    // instance. It must now answer, and answer as a retryable outage — never as
    // a `rejection`, which would hand the caller a decision nobody made.
    #[tokio::test]
    async fn a_store_outage_answers_both_operations_instead_of_trapping() {
        let service =
            InvocationService::new(FailingBackend(InvocationError::StoreUnavailable), config());

        let begin = service
            .begin(request())
            .await
            .expect_err("a store outage must not read as a decided outcome");
        assert_eq!(begin.kind(), InvocationError::StoreUnavailable);
        let wait = service
            .wait("run-1".to_string(), 0)
            .await
            .expect_err("a store outage must not read as a decided outcome");
        assert_eq!(wait.kind(), InvocationError::StoreUnavailable);
    }

    // A run the store does not hold is the caller's `unknown-run`. Reporting it
    // as a still-unreleased `none` would spend the adapter's whole wait budget
    // and end in a 504 inviting a retry under the same idempotency key.
    #[tokio::test]
    async fn a_poll_that_names_no_run_is_unknown_run_not_still_waiting() {
        let backend = MockBackend::default();
        backend
            .polls
            .lock()
            .await
            .push_back(InvocationPoll::NotFound);
        let service = InvocationService::new(backend, config());

        let failure = service
            .wait("run-missing".to_string(), 0)
            .await
            .expect_err("an absent run must not read as unreleased");
        assert_eq!(failure.kind(), InvocationError::UnknownRun);
    }

    // The adapter serializes the mapped payload itself, so a payload that is not
    // JSON means a defective or hostile guest. That is terminal: classifying it
    // as an outage would advertise a retry that can only fail again.
    #[tokio::test]
    async fn a_payload_that_is_not_json_is_refused_terminally_before_admission() {
        let backend = MockBackend::default();
        backend.targets.lock().await.push_back(Some(target(true)));
        backend
            .recoveries
            .lock()
            .await
            .push_back(InvocationRecovery::Missing);
        let service = InvocationService::new(backend.clone(), config());
        let mut malformed = request();
        malformed.payload = "not json".to_string();

        let failure = service
            .begin(malformed)
            .await
            .expect_err("a non-JSON payload must be refused");
        assert_eq!(failure.kind(), InvocationError::InvalidRequest);
        assert!(backend.admissions.lock().await.is_empty());
    }

    // A stored outcome kind this contract cannot name is corruption, not an
    // outage: the same row will decode the same way on every retry.
    #[test]
    fn an_unnameable_stored_outcome_kind_is_terminal_corruption() {
        let failure = to_invoke_result(outcome("cancelled", "", 200))
            .expect_err("an unknown stored kind must not decode");
        assert_eq!(failure.kind(), InvocationError::StoreCorrupt);
    }
}
