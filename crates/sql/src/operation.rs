//! Entry-owned operations on the Store's host transaction.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::invocation::{AuthorizedInvocation, InvocationRequest};
use anyhow::Context as _;
use wamn_catalog::canonical_name::CanonicalName;
use wamn_engine::entry_operation::operation::{self as wit, Begun, Refusal};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::native_routes::DeliveryClass;
use wash_runtime::engine::ctx::ActiveCtx;
use wash_runtime::wasmtime::component::{Accessor, HasData, Resource, ResourceTable};

use super::session_identity::{SessionIdentityError, SessionIdentityErrorType};
use super::write_log::{Claim, WriteLogStore};
use super::{
    OperationTransaction, PgCommitFailure, SessionIdentity, WAMN_POSTGRES_ID,
    WamnPostgres,
};

/// Delivery counters shared by all operations of this PostgreSQL plugin.
#[derive(Debug)]
pub(super) struct OperationMetrics {
    attempts: opentelemetry::metrics::Counter<u64>,
    errors: opentelemetry::metrics::Counter<u64>,
}

impl Default for OperationMetrics {
    fn default() -> Self {
        let meter = opentelemetry::global::meter(wamn_engine::native_routes::OPERATION_METRICS_ID);
        Self {
            attempts: meter.u64_counter("wamn.router.delivery.attempts").build(),
            errors: meter.u64_counter("wamn.router.delivery.errors").build(),
        }
    }
}

impl OperationMetrics {
    fn record(&self, source: &CanonicalName, class: DeliveryClass) {
        let mut attributes = vec![
            opentelemetry::KeyValue::new("wamn.source.type", "attachment"),
            opentelemetry::KeyValue::new("wamn.source.id", source.to_string()),
        ];
        self.attempts.add(1, &attributes);
        if let Some(error) = class.error() {
            attributes.push(opentelemetry::KeyValue::new("wamn.delivery.error", error));
            self.errors.add(1, &attributes);
        }
    }
}

/// Immutable authority installed by the host before a workload starts.
#[derive(Debug)]
pub(super) struct OperationWorkload {
    pub(super) workload: Box<str>,
    identity: SessionIdentity,
    operations: BTreeMap<CanonicalName, OperationAuthority>,
}

#[derive(Debug)]
struct OperationAuthority {
    read: bool,
    delete: bool,
    grant: Option<wamn_catalog::registered_operation::RegisteredOperation>,
    schema: Option<String>,
    transaction: bool,
}

impl WamnPostgres {
    pub(super) fn bind_entry_instance(
        &self,
        workload: &str,
        component: &str,
        config: wamn_catalog::application_config::ApplicationConfig,
    ) -> anyhow::Result<()> {
        use wamn_catalog::application_config::ApplicationOperationType;
        let identity = SessionIdentity {
            project_env_key: Some(wamn_registry::name::project_env_key(
                &config.org,
                &config.project,
                &config.environment,
            )),
            project: Some(config.project),
            application: Some(config.application),
            ..SessionIdentity::default()
        };
        let operations = config
            .components
            .into_iter()
            .flat_map(|component| {
                component
                    .operations
                    .into_iter()
                    .map(move |(name, operation)| {
                        let grant = operation
                            .public
                            .then(|| name.as_str().parse())
                            .transpose()?;
                        Ok((
                            name,
                            OperationAuthority {
                                read: operation.type_ == Some(ApplicationOperationType::Read),
                                delete: operation.type_ == Some(ApplicationOperationType::Delete),
                                grant,
                                schema: component.schema.clone(),
                                transaction: operation.host_transaction,
                            },
                        ))
                    })
            })
            .collect::<anyhow::Result<_>>()?;
        self.operation_workloads
            .write()
            .expect("operation workloads lock poisoned")
            .insert(
                component.to_owned(),
                Arc::new(OperationWorkload {
                    workload: workload.into(),
                    identity,
                    operations,
                }),
            );
        Ok(())
    }
}

/// Store-local authority for a begun write or an authorized read.
#[derive(Debug, Clone)]
struct StoreOperation {
    workload: Arc<str>,
    open: Arc<tokio::sync::Mutex<Option<OpenOperation>>>,
}

#[derive(Debug)]
struct OpenOperation {
    postgres: Arc<WamnPostgres>,
    authority: Arc<AuthorizedInvocation>,
    operation: CanonicalName,
    key: String,
    write: bool,
    source: CanonicalName,
    settled: bool,
}

impl OpenOperation {
    fn scope(&self) -> &str {
        self.authority.id()
    }
    fn transaction(&self) -> Option<OperationTransaction> {
        self.postgres.operation_transaction_for(self.scope())
    }
    fn end_execution(&self) {
        self.postgres
            .scopes
            .update_existing::<crate::invocation::Authority, _>(self.scope(), |record| {
                record.identity = SessionIdentity::default();
                record.causation = None;
                record.transaction = None;
                record.read = false;
            });
    }

    fn store(&self) -> WriteLogStore {
        let identity = self
            .postgres
            .session_identity(self.scope())
            .expect("active invocation identity");
        WriteLogStore::new(
            Arc::clone(&self.postgres),
            identity
                .project
                .unwrap_or_else(|| super::DEFAULT_PROJECT.to_owned()),
            identity
                .project_env_key
                .expect("active invocation has a project environment"),
        )
    }
}

impl Drop for OpenOperation {
    fn drop(&mut self) {
        if !self.settled {
            self.postgres
                .operation_metrics
                .record(&self.source, DeliveryClass::ExecutionFailed);
        }
    }
}

fn state(table: &mut ResourceTable) -> Option<&mut StoreOperation> {
    table
        .iter_mut()
        .find_map(|entry| entry.downcast_mut::<StoreOperation>())
}

/// Context for failures below the WIT boundary.
#[derive(Debug)]
struct OperationError {
    type_: OperationErrorType,
    scope: String,
    source: SessionIdentityError,
}

#[derive(Debug)]
enum OperationErrorType {
    Identity,
    Transaction,
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "bind {:?} for {}: {}",
            self.type_, self.scope, self.source
        )
    }
}

impl std::error::Error for OperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn binding_error(type_: OperationErrorType, scope: &str, source: anyhow::Error) -> Refusal {
    // Principal reads classify their failures. The other binding steps only
    // validate identity and install local authority.
    let source = source
        .downcast::<SessionIdentityError>()
        .unwrap_or_else(|source| SessionIdentityError {
            type_: SessionIdentityErrorType::Authorization,
            context: "bind operation authority",
            source,
        });
    let error = OperationError {
        type_,
        scope: scope.to_owned(),
        source,
    };
    tracing::warn!(category = ?error.source.type_, error = %error, "operation binding refused");
    Refusal::Postgres(match error.source.type_ {
        SessionIdentityErrorType::Authorization => super::PgError::PermissionDenied,
        SessionIdentityErrorType::Infrastructure => super::PgError::ConnectionUnavailable,
    })
}

/// The `wamn:operation` host over PostgreSQL. It reaches the Store's state
/// through [`ActiveCtx`], as the other `wamn:postgres` imports do.
pub(super) struct PostgresOperation;

impl HasData for PostgresOperation {
    type Data<'a> = ActiveCtx<'a>;
}

impl<T: 'static + Send> wit::HostWithStore<T> for PostgresOperation {
    async fn invoke(
        accessor: &Accessor<T, Self>,
        operation: String,
        context: wamn_engine::operation::node_types::NodeContext,
        input: String,
    ) -> wash_runtime::wasmtime::Result<
        Result<
            wamn_engine::operation::node_types::Emission,
            wamn_engine::operation::node_types::NodeError,
        >,
    > {
        let operation: CanonicalName = operation
            .try_into()
            .map_err(wash_runtime::wasmtime::Error::new)?;
        let store = accessor
            .with(|mut access| state(access.get().table).cloned())
            .ok_or_else(|| wash_runtime::wasmtime::Error::msg("operation is not authorized"))?;
        let authority = {
            let current = store.open.lock().await;
            let open = current
                .as_ref()
                .filter(|open| !open.settled && open.operation == operation)
                .ok_or_else(|| wash_runtime::wasmtime::Error::msg("operation authority differs"))?;
            Arc::clone(&open.authority)
        };
        let (invoker, postgres) = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>((
                ctx.try_get_plugin::<wamn_engine::operation::WamnOperation>("wamn-operation")?,
                ctx.try_get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)?,
            ))
        })?;
        invoker
            .invoke(
                &store.workload,
                &postgres.scopes,
                Arc::from(authority.id()),
                operation,
                context,
                input,
            )
            .await
            .map_err(wash_runtime::wasmtime::Error::from_anyhow)
    }

    async fn sign_result(
        accessor: &Accessor<T, Self>,
        _payload: String,
    ) -> wash_runtime::wasmtime::Result<Result<String, wamn_engine::operation::node_types::NodeError>>
    {
        let store = accessor
            .with(|mut access| state(access.get().table).cloned())
            .ok_or_else(|| wash_runtime::wasmtime::Error::msg("operation is not authorized"))?;
        let current = store.open.lock().await;
        current
            .as_ref()
            .ok_or_else(|| wash_runtime::wasmtime::Error::msg("operation is not authorized"))?;
        Ok(Err(wamn_engine::operation::node_types::NodeError::Terminal(
            wamn_engine::operation::node_types::ErrorDetail {
                code: Some("result_signing_unavailable".into()),
                message: "result signing is not available until connections return".into(),
            },
        )))
    }

    async fn finish_read(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let Some(store) = accessor.with(|mut access| state(access.get().table).cloned()) else {
            return Ok(Err(Refusal::NotBegun));
        };
        let mut current = store.open.lock().await;
        if current
            .as_ref()
            .is_none_or(|open| open.write || open.settled)
        {
            return Ok(Err(Refusal::NotBegun));
        }
        let mut open = current.take().expect("authorized read exists");
        if let Some(transaction) = open.transaction()
            && let Err(PgCommitFailure::RolledBack(error) | PgCommitFailure::Uncertain(error)) =
                transaction.commit().await
        {
            return Ok(Err(Refusal::Postgres(error)));
        }
        open.postgres
            .operation_metrics
            .record(&open.source, DeliveryClass::Delivered);
        open.settled = true;
        open.end_execution();
        *current = Some(open);
        Ok(Ok(()))
    }

    async fn wal_position(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<String, Refusal>> {
        let Some(store) = accessor.with(|mut access| state(access.get().table).cloned()) else {
            return Ok(Err(Refusal::NotBegun));
        };
        let current = store.open.lock().await;
        let Some(open) = current.as_ref() else {
            return Ok(Err(Refusal::NotBegun));
        };
        if open.settled {
            return Ok(Err(Refusal::NotBegun));
        }
        let Some(transaction) = open.transaction() else {
            return Ok(Err(Refusal::NotBegun));
        };
        Ok(transaction.wal_position().await.map_err(Refusal::Postgres))
    }

    async fn rollback(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let Some(store) = accessor.with(|mut access| state(access.get().table).cloned()) else {
            return Ok(Err(Refusal::NotBegun));
        };
        let mut current = store.open.lock().await;
        if current
            .as_ref()
            .is_some_and(|open| !open.write || open.settled)
        {
            return Ok(Err(Refusal::NotBegun));
        }
        let Some(mut open) = current.take() else {
            return Ok(Err(Refusal::NotBegun));
        };
        let rolled_back = match open.transaction() {
            Some(transaction) => transaction.rollback().await.map_err(Refusal::Postgres),
            None => open
                .store()
                .release_claim(&open.operation, &open.key)
                .await
                .map_err(Refusal::Postgres),
        };
        open.postgres.operation_metrics.record(
            &open.source,
            if rolled_back.is_ok() {
                DeliveryClass::Delivered
            } else {
                DeliveryClass::ExecutionFailed
            },
        );
        open.settled = true;
        open.end_execution();
        *current = Some(open);
        Ok(rolled_back)
    }
    async fn authorize(
        accessor: &Accessor<T, Self>,
        caller: Option<Resource<AuthenticatedCaller>>,
        operation: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        Ok(
            enter(accessor, caller, operation, String::new(), Vec::new(), true)
                .await?
                .map(drop),
        )
    }

    async fn begin_operation(
        accessor: &Accessor<T, Self>,
        caller: Option<Resource<AuthenticatedCaller>>,
        operation: String,
        idempotency_key: String,
        request_hash: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<Begun, Refusal>> {
        enter(
            accessor,
            caller,
            operation,
            idempotency_key,
            request_hash,
            false,
        )
        .await
    }

    async fn commit(
        accessor: &Accessor<T, Self>,
        result: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let store = accessor.with(|mut access| state(access.get().table).cloned());
        let Some(store) = store else {
            return Ok(Err(Refusal::NotBegun));
        };
        let mut current = store.open.lock().await;
        if current
            .as_ref()
            .is_some_and(|open| !open.write || open.settled)
        {
            return Ok(Err(Refusal::NotBegun));
        }
        let Some(mut open) = current.take() else {
            return Ok(Err(Refusal::NotBegun));
        };
        let committed = complete_operation(&open, result).await;
        open.postgres.operation_metrics.record(
            &open.source,
            if committed.is_ok() {
                DeliveryClass::Delivered
            } else {
                DeliveryClass::ExecutionFailed
            },
        );
        open.settled = true;
        open.end_execution();
        *current = Some(open);
        Ok(committed)
    }
}

async fn enter<T: 'static + Send>(
    accessor: &Accessor<T, PostgresOperation>,
    caller: Option<Resource<AuthenticatedCaller>>,
    operation: String,
    idempotency_key: String,
    request_hash: Vec<u8>,
    authorize_only: bool,
) -> wash_runtime::wasmtime::Result<Result<Begun, Refusal>> {
    let Ok(operation) = CanonicalName::try_from(operation) else {
        return Ok(Err(Refusal::UnknownOperation));
    };
    let store = accessor.with(|mut access| {
        let ctx = access.get();
        if state(ctx.table).is_none() {
            ctx.table.push(StoreOperation {
                workload: Arc::clone(&ctx.component_id),
                open: Arc::default(),
            })?;
        }
        Ok::<_, wash_runtime::wasmtime::Error>(
            state(ctx.table).expect("Store state exists").clone(),
        )
    })?;
    // Serialize lifecycle calls without marking an unfinished begin as open.
    let mut current = store.open.lock().await;
    // The open operation and its scope stay as they are.
    if current.as_ref().is_some_and(|open| !open.settled) {
        return Ok(Err(Refusal::AlreadyBegun));
    }
    current.take();
    accessor.with(|mut access| {
        access.get().component_id = Arc::clone(&store.workload);
    });
    let input = accessor.with(|mut access| {
        let ctx = access.get();
        let Ok(caller) = caller
            .map(|caller| ctx.table.get(&caller).cloned())
            .transpose()
        else {
            return Ok(Err(Refusal::InvalidCaller));
        };
        let postgres = ctx.try_get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)?;
        Ok::<_, wash_runtime::wasmtime::Error>(Ok((postgres, caller)))
    })?;
    let (postgres, caller) = match input {
        Ok(input) => input,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let source = caller.as_ref().map_or_else(
        || operation.clone(),
        |caller| caller.attachment_id().to_owned(),
    );
    let trace = accessor
        .with(|mut access| wamn_engine::invocation_trace::invocation_trace(&mut access.get()));
    let result = trace
        .clone()
        .run(begin(
            Arc::clone(&postgres),
            &store.workload,
            caller.as_ref(),
            operation,
            idempotency_key,
            request_hash,
            authorize_only,
        ))
        .await;
    if !matches!(&result, Ok((Some(open), _)) if !open.settled) {
        let class = match &result {
            Ok(_) | Err(Refusal::Conflict | Refusal::InProgress | Refusal::OutcomeUnknown) => {
                DeliveryClass::Delivered
            }
            Err(Refusal::GrantDenied | Refusal::InvalidCaller) => DeliveryClass::PermissionDenied,
            Err(_) => DeliveryClass::ExecutionFailed,
        };
        postgres.operation_metrics.record(&source, class);
    }
    match result {
        Ok((mut open, begun)) => {
            if let Some(open) = &mut open {
                accessor.with(|mut access| {
                    let mut ctx = access.get();
                    ctx.component_id = Arc::from(open.scope());
                });
            }
            *current = open;
            Ok(Ok(begun))
        }
        Err(refusal) => Ok(Err(refusal)),
    }
}

async fn begin(
    postgres: Arc<WamnPostgres>,
    workload: &str,
    caller: Option<&AuthenticatedCaller>,
    operation: CanonicalName,
    key: String,
    request: Vec<u8>,
    authorize_only: bool,
) -> Result<(Option<OpenOperation>, Begun), Refusal> {
    let facts = postgres
        .operation_workloads
        .read()
        .expect("operation workloads lock poisoned")
        .get(workload)
        .cloned()
        .ok_or(Refusal::UnknownOperation)?;
    let admitted = facts
        .operations
        .get(&operation)
        .ok_or(Refusal::UnknownOperation)?;
    if admitted.read && !authorize_only {
        return Err(Refusal::ReadsClaimNothing);
    }
    // A caller handle belongs to a configured route of this workload.
    if let Some(caller) = caller
        && postgres
            .scopes
            .routes(workload)
            .is_none_or(|routes| routes.route(caller.attachment_id()).is_none())
    {
        return Err(Refusal::InvalidCaller);
    }
    let delivery = uuid::Uuid::new_v4().to_string();
    let source = caller.map_or_else(
        || operation.clone(),
        |caller| caller.attachment_id().to_owned(),
    );
    let identity = SessionIdentity {
        schema: admitted
            .schema
            .clone()
            .or_else(|| facts.identity.schema.clone()),
        user_id: caller.map(|caller| caller.principal_id().to_owned()),
        operation: Some(operation.clone()),
        ..facts.identity.clone()
    };
    let authority = AuthorizedInvocation::enter(
        &postgres,
        InvocationRequest {
            context: None,
            scope: wamn_engine::operation::next_scope(workload).into_string(),
            identity,
            caller: caller.cloned(),
            registered_operation: admitted.grant.as_ref(),
            causation: Some(wamn_event_wire::Causation {
                run: delivery.clone(),
                root: delivery.clone(),
                depth: 0,
            }),
            invocation: None,
            read: admitted.read,
        },
    )
    .map_err(|error| {
        if error
            .downcast_ref::<wamn_engine::native_routes::OperationRefusal>()
            .is_some()
        {
            Refusal::GrantDenied
        } else {
            binding_error(OperationErrorType::Identity, workload, error)
        }
    })?;
    let mut open = OpenOperation {
        postgres: Arc::clone(&postgres),
        authority,
        operation,
        key,
        source,
        write: !authorize_only,
        settled: true,
    };
    if authorize_only {
        let transaction = open
            .postgres
            .begin_operation_transaction(open.scope())
            .await
            .map_err(Refusal::Postgres)?;
        open.postgres
            .bind_operation_transaction(open.scope(), &transaction)
            .map_err(|error| binding_error(OperationErrorType::Transaction, open.scope(), error))?;
        open.settled = false;
        return Ok((Some(open), Begun::New(delivery)));
    }
    let claim = claim_operation(&open, admitted.transaction, admitted.delete, request).await?;
    match claim {
        Claim::New => {
            open.settled = false;
            Ok((Some(open), Begun::New(delivery)))
        }
        Claim::Finished(result) => {
            open.end_execution();
            Ok((Some(open), Begun::Finished(result)))
        }
        Claim::InProgress => Err(Refusal::InProgress),
        Claim::OutcomeUnknown => Err(Refusal::OutcomeUnknown),
        Claim::Conflict => Err(Refusal::Conflict),
    }
}

async fn claim_operation(
    open: &OpenOperation,
    transacted: bool,
    delete: bool,
    request: Vec<u8>,
) -> Result<Claim, Refusal> {
    Ok(if transacted {
        let mut transaction = open
            .postgres
            .begin_operation_transaction(open.scope())
            .await
            .map_err(Refusal::Postgres)?;
        if delete {
            transaction =
                transaction.with_known_result(super::write_log::DELETED_RESULT.to_owned());
        }
        let claim = transaction
            .begin_opaque(&open.operation, &open.key, request)
            .await
            .map_err(Refusal::Postgres)?;
        if matches!(claim, Claim::New) {
            open.postgres
                .bind_operation_transaction(open.scope(), &transaction)
                .map_err(|error| {
                    binding_error(OperationErrorType::Transaction, open.scope(), error)
                })?;
        } else {
            transaction.rollback().await.map_err(Refusal::Postgres)?;
        }
        claim
    } else {
        open.store()
            .claim(&open.operation, &open.key, request)
            .await
            .map_err(Refusal::Postgres)?
    })
}

async fn complete_operation(open: &OpenOperation, result: String) -> Result<(), Refusal> {
    if let Some(transaction) = open.transaction() {
        transaction.finish_opaque(&open.operation, &open.key, result);
        transaction.commit().await.map_err(|error| match error {
            PgCommitFailure::RolledBack(error) => Refusal::Postgres(error),
            PgCommitFailure::Uncertain(_) => Refusal::OutcomeUnknown,
        })
    } else {
        open.store()
            .complete(&open.operation, &open.key, result)
            .await
            .map_err(Refusal::Postgres)
    }
}

/// Workflow nodes borrow the same concrete claim and transaction owner as HTTP.
impl WamnPostgres {
    pub async fn invoke_node<F, Fut>(
        self: &Arc<Self>,
        authority: &Arc<AuthorizedInvocation>,
        operation: &CanonicalName,
        route: &wamn_catalog::ServingRoute,
        transacted: bool,
        input: &serde_json::Value,
        mut invoke: F,
    ) -> anyhow::Result<
        Result<
            wamn_engine::operation::node_types::Emission,
            wamn_engine::operation::node_types::NodeError,
        >,
    >
    where
        F: FnMut(String) -> Fut + Send,
        Fut: Future<Output = anyhow::Result<wamn_engine::operation::native_call::NativeOutcome>>
            + Send,
    {
        use serde_json::{Value, json};
        use wamn_engine::operation::node_types;
        let parsed = (|| {
            let elements = input
                .as_array()
                .context("operation input is not a request element list")?;
            let mut keys = std::collections::HashSet::new();
            let mut requests = Vec::with_capacity(elements.len());
            for element in elements {
                let id = element["request_id"]
                    .as_str()
                    .context("request element has no request_id")?;
                let key = route
                    .idempotency
                    .as_deref()
                    .map_or(Some(id), |field| {
                        field
                            .split('.')
                            .try_fold(element, |value, field| value.get(field))
                            .and_then(Value::as_str)
                    })
                    .context("request element has no caller key")?;
                anyhow::ensure!(keys.insert(key), "caller key repeats within one operation");
                let mut body = element.clone();
                body.as_object_mut()
                    .context("request element is not an object")?
                    .remove("request_id");
                if let Some(field) = &route.idempotency {
                    let segments = field.split('.').collect::<Vec<_>>();
                    if let Some((last, parents)) = segments.split_last()
                        && let Some(Value::Object(parent)) = parents
                            .iter()
                            .try_fold(&mut body, |value, name| value.get_mut(*name))
                    {
                        parent.remove(*last);
                    }
                }
                requests.push((element, id, key, wamn_digest::canonical_json_sha256(&body)));
            }
            Ok::<_, anyhow::Error>((elements, requests))
        })();
        let (elements, requests) = match parsed {
            Ok(requests) => requests,
            Err(error) => {
                return Ok(Err(node_types::NodeError::InvalidInput(
                    node_types::ErrorDetail {
                        code: Some("invalid_input".into()),
                        message: error.to_string(),
                    },
                )));
            }
        };
        let mut answers = Vec::with_capacity(elements.len());
        let mut port = None;
        for (element, id, key, hash) in requests {
            self.revoke_operation_transaction(authority.id());
            let open = OpenOperation {
                postgres: Arc::clone(self),
                authority: Arc::clone(authority),
                operation: operation.clone(),
                key: key.into(),
                write: true,
                source: operation.clone(),
                settled: true,
            };
            let claim = claim_operation(
                &open,
                transacted,
                route.type_ == wamn_catalog::OperationType::Delete,
                hash.to_string().into_bytes(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("operation claim refused: {error:?}"))?;
            let refusal = |code: &str| json!({"error":{"code":code}});
            let body = match claim {
                Claim::Finished(result) => serde_json::from_str(&result)?,
                Claim::InProgress => refusal("in-progress"),
                Claim::OutcomeUnknown => refusal("outcome-unknown"),
                Claim::Conflict => refusal("idempotency_conflict"),
                Claim::New => {
                    let outcome = invoke(serde_json::to_string(&json!([element]))?)
                        .await?
                        .into_json()?;
                    match outcome {
                        Err(node_types::NodeError::Cancelled) => {
                            if let Some(transaction) = open.transaction() {
                                transaction.rollback().await?;
                            }
                            self.revoke_operation_transaction(authority.id());
                            return Ok(Err(node_types::NodeError::Cancelled));
                        }
                        Err(error) => {
                            if let Some(transaction) = open.transaction() {
                                transaction.rollback().await?;
                            } else {
                                open.store().release_claim(operation, key).await?;
                            }
                            if elements.len() == 1 {
                                return Ok(Err(error));
                            }
                            let detail = match &error {
                                node_types::NodeError::Retryable(detail)
                                | node_types::NodeError::Terminal(detail)
                                | node_types::NodeError::InvalidInput(detail) => detail,
                                node_types::NodeError::RateLimited(detail) => &detail.detail,
                                node_types::NodeError::Cancelled => return Ok(Err(error)),
                            };
                            json!({"error":{"code":detail.code,"message":detail.message}})
                        }
                        Ok(emission) => {
                            port = emission.port.or(port);
                            let results: Vec<Value> = serde_json::from_str(&emission.payload)?;
                            let mut result = results
                                .into_iter()
                                .find(|result| result["request_id"].as_str() == Some(id))
                                .context("operation returned no correlated result")?;
                            result
                                .as_object_mut()
                                .context("operation result is not an object")?
                                .remove("request_id");
                            if result.get("error").is_some() {
                                if let Some(transaction) = open.transaction() {
                                    transaction.rollback().await?;
                                } else {
                                    open.store().release_claim(operation, key).await?;
                                }
                                result
                            } else {
                                match complete_operation(&open, serde_json::to_string(&result)?)
                                    .await
                                {
                                    Ok(()) => result,
                                    Err(Refusal::OutcomeUnknown) => refusal("outcome-unknown"),
                                    Err(error) => {
                                        return Err(anyhow::anyhow!(
                                            "operation completion refused: {error:?}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            };
            self.revoke_operation_transaction(authority.id());
            let mut answer = body;
            answer
                .as_object_mut()
                .context("stored operation result is not an object")?
                .insert("request_id".into(), Value::from(id));
            answers.push(answer);
        }
        Ok(Ok(node_types::Emission {
            payload: serde_json::to_string(&answers)?,
            port,
        }))
    }
}
