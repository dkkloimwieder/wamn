//! Entry-owned operations on the Store's host transaction.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use wamn_catalog::{ComponentSqlField, ComponentSqlStatement, ComponentSqlValueType};
use wamn_engine::entry_operation::operation::{self as wit, Begun, Refusal};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::invocation_trace::{InvocationTrace, RequestTrace};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::DeliveryClass;
use wash_runtime::engine::ctx::ActiveCtx;
use wash_runtime::wasmtime::component::{Accessor, HasData, Resource, ResourceTable};

use super::write_log::OpaqueBegun;
use super::{
    CommitFailure, OperationTransaction, PreparedStatementMembers, PreparedStatementSet,
    ReleaseIdentity, SessionClaims, StatementField, StatementValueType, VerifiedStatement,
    WAMN_POSTGRES_ID, WamnPostgres,
};

/// Delivery counters shared by all operations of this PostgreSQL plugin.
#[derive(Debug)]
pub(super) struct OperationMetrics {
    attempts: opentelemetry::metrics::Counter<u64>,
    errors: opentelemetry::metrics::Counter<u64>,
}

impl Default for OperationMetrics {
    fn default() -> Self {
        let meter = opentelemetry::global::meter(wamn_engine::router_delivery::ROUTER_DELIVERY_ID);
        Self {
            attempts: meter.u64_counter("wamn.router.delivery.attempts").build(),
            errors: meter.u64_counter("wamn.router.delivery.errors").build(),
        }
    }
}

impl OperationMetrics {
    fn record(&self, source: &str, class: DeliveryClass) {
        let mut attributes = vec![
            opentelemetry::KeyValue::new("wamn.source.kind", "attachment"),
            opentelemetry::KeyValue::new("wamn.source.id", source.to_owned()),
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
    start: (String, String),
    release: Arc<LoadedRelease>,
    claims: SessionClaims,
    operations: BTreeMap<String, PreparedStatementSet>,
    grants: BTreeMap<String, Option<String>>,
    schemas: BTreeMap<String, String>,
    /// The digest of the member component that owns each operation.
    digests: BTreeMap<String, String>,
    /// Operation to its package's application name in the environment map.
    applications: BTreeMap<String, String>,
    members: PreparedStatementMembers,
}

impl WamnPostgres {
    /// Record the host-selected entry reference and released SQL ownership.
    pub fn bind_operation_workload(
        &self,
        workload: &str,
        release: Arc<LoadedRelease>,
        reference: &str,
        component_name: &str,
        claims: SessionClaims,
    ) -> anyhow::Result<()> {
        let entry = release
            .manifest()
            .composed_entry
            .as_ref()
            .context("release has no composed entry")?;
        anyhow::ensure!(
            wamn_catalog::composed_entry_starts_as(&entry.reference, reference),
            "composed entry reference differs from the release"
        );
        anyhow::ensure!(
            claims.tenant == release.scope().tenant_id
                && claims.release.as_ref() == Some(&ReleaseIdentity::from_loaded(&release)),
            "operation release mismatch"
        );
        let components = entry
            .members
            .iter()
            .map(|member| {
                release
                    .manifest()
                    .components
                    .iter()
                    .find(|component| {
                        component.package_id == member.package_id
                            && component.descriptor.component == member.component
                            && component.descriptor.component_digest == member.digest
                    })
                    .expect("manifest validates entry members")
            })
            .collect::<Vec<_>>();
        let mut operations = BTreeMap::new();
        let mut grants = BTreeMap::new();
        let mut schemas = BTreeMap::new();
        let mut digests = BTreeMap::new();
        let mut applications = BTreeMap::new();
        for component in &components {
            let application = wamn_catalog::application_for_package(
                &release.scope().applications,
                &component.package_id,
            )
            .with_context(|| {
                format!(
                    "package {} must belong to exactly one declared application",
                    component.package_id
                )
            })?;
            for (id, fact) in &component.operations {
                applications.insert(id.clone(), application.to_owned());
                let read = release
                    .manifest()
                    .routes
                    .iter()
                    .any(|route| &route.operation == id && route.type_.is_read());
                operations.insert(
                    id.clone(),
                    Self::prepare_statement_set(lower_statements(&fact.statements), read),
                );
                grants.insert(id.clone(), fact.registered_operation.clone());
                digests.insert(id.clone(), component.descriptor.component_digest.clone());
                if let Some(schema) = &component.runtime_schema {
                    schemas.insert(id.clone(), schema.clone());
                }
            }
        }
        // Publication preserves the application's named SQL imports in the entry.
        // The composed application's descriptor already binds every nested member.
        let application = release
            .manifest()
            .composing_member()
            .context("entry has no application members")?;
        let mut members = BTreeMap::new();
        if application.statement_instances.is_empty() {
            let statements = application
                .operations
                .values()
                .flat_map(|fact| fact.statements.clone())
                .collect();
            members.insert(
                "sql-0".to_owned(),
                Self::prepare_statement_set(lower_statements(&statements), false),
            );
        } else {
            for (name, statements) in &application.statement_instances {
                members.insert(
                    name.clone(),
                    Self::prepare_statement_set(lower_statements(statements), false),
                );
            }
        }
        let mut workloads = self
            .operation_workloads
            .write()
            .expect("operation workloads lock poisoned");
        anyhow::ensure!(
            !workloads.contains_key(workload),
            "operation workload already bound"
        );
        workloads.insert(
            workload.to_owned(),
            Arc::new(OperationWorkload {
                start: (component_name.to_owned(), reference.to_owned()),
                release,
                claims,
                operations,
                grants,
                schemas,
                digests,
                applications,
                members: Self::prepare_statement_members(members),
            }),
        );
        Ok(())
    }

    /// Remove a completed or cancelled start's pending reference.
    pub fn forget_operation_start(&self, workload: &str) {
        self.operation_workloads
            .write()
            .expect("operation workloads lock poisoned")
            .remove(workload);
    }

    pub(super) fn bind_entry_instance(
        &self,
        workload: &str,
        component: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        let mut bound = self
            .operation_workloads
            .write()
            .expect("operation workloads lock poisoned");
        let entry = bound
            .get(workload)
            .cloned()
            .context("operation entry workload is not bound")?;
        {
            let (started_name, reference) = &entry.start;
            anyhow::ensure!(started_name == name, "operation authority is entry-only");
            anyhow::ensure!(
                entry
                    .release
                    .manifest()
                    .composed_entry
                    .as_ref()
                    .is_some_and(|released| {
                        wamn_catalog::composed_entry_starts_as(&released.reference, reference)
                    }),
                "started entry reference differs from the release"
            );
        }
        bound.insert(component.to_owned(), entry);
        Ok(())
    }
}

fn lower_statements(
    statements: &BTreeMap<String, ComponentSqlStatement>,
) -> super::VerifiedStatementSet {
    let field = |field: &ComponentSqlField| StatementField {
        value_type: match field.value_type {
            ComponentSqlValueType::Boolean => StatementValueType::Boolean,
            ComponentSqlValueType::Int32 => StatementValueType::Int32,
            ComponentSqlValueType::Int64 => StatementValueType::Int64,
            ComponentSqlValueType::Float64 => StatementValueType::Float64,
            ComponentSqlValueType::Text => StatementValueType::Text,
            ComponentSqlValueType::Bytes => StatementValueType::Bytes,
            ComponentSqlValueType::Numeric => StatementValueType::Numeric,
            ComponentSqlValueType::Timestamptz => StatementValueType::Timestamptz,
            ComponentSqlValueType::Json => StatementValueType::Json,
            ComponentSqlValueType::Uuid => StatementValueType::Uuid,
        },
        nullable: field.nullable,
    };
    statements
        .iter()
        .map(|(path, statement)| {
            (
                path.clone(),
                VerifiedStatement {
                    exact_sql: statement.sql.clone().into_boxed_str(),
                    binds: statement.binds.iter().map(field).collect(),
                    columns: statement.columns.iter().map(field).collect(),
                    transactional: statement.transactional,
                },
            )
        })
        .collect()
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
    scope: String,
    operation: String,
    key: String,
    transaction: Option<OperationTransaction>,
    source: String,
    settled: bool,
    /// The request trace that `wamn.component.invoke` replaces until settle.
    request_trace: Option<InvocationTrace>,
}

impl Drop for OpenOperation {
    fn drop(&mut self) {
        if !self.settled {
            self.postgres
                .operation_metrics
                .record(&self.source, DeliveryClass::ExecutionFailed);
        }
        self.postgres.revoke_operation_transaction(&self.scope);
        self.postgres.revoke_session_claims(&self.scope);
        self.postgres.clear_statement_scope(&self.scope);
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
    kind: OperationErrorKind,
    scope: String,
    source: anyhow::Error,
}

#[derive(Debug)]
enum OperationErrorKind {
    Claims,
    Statements,
    Transaction,
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "bind {:?} for {}: {}",
            self.kind, self.scope, self.source
        )
    }
}

impl std::error::Error for OperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn binding_error(kind: OperationErrorKind, scope: &str, source: anyhow::Error) -> Refusal {
    let error = OperationError {
        kind,
        scope: scope.to_owned(),
        source,
    };
    tracing::warn!(error = %error, "operation binding refused");
    Refusal::Postgres(super::PgError::PermissionDenied)
}

/// The `wamn:operation` host over PostgreSQL. It reaches the Store's state
/// through [`ActiveCtx`], as the other `wamn:postgres` imports do.
pub(super) struct PostgresOperation;

impl HasData for PostgresOperation {
    type Data<'a> = ActiveCtx<'a>;
}

impl<T: 'static + Send> wit::HostWithStore<T> for PostgresOperation {
    async fn finish_read(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<(), Refusal>> {
        let Some(store) = accessor.with(|mut access| state(access.get().table).cloned()) else {
            return Ok(Err(Refusal::NotBegun));
        };
        let mut current = store.open.lock().await;
        if current
            .as_ref()
            .is_none_or(|open| open.transaction.is_some())
        {
            return Ok(Err(Refusal::NotBegun));
        }
        let mut open = current.take().expect("authorized read exists");
        open.postgres
            .operation_metrics
            .record(&open.source, DeliveryClass::Delivered);
        open.settled = true;
        let request_trace = open.request_trace.take();
        drop(open);
        accessor.with(|mut access| {
            let mut ctx = access.get();
            ctx.component_id = Arc::clone(&store.workload);
            swap_request_trace(ctx.table, request_trace);
        });
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
        let Some(transaction) = open.transaction.as_ref() else {
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
            .is_some_and(|open| open.transaction.is_none())
        {
            return Ok(Err(Refusal::NotBegun));
        }
        let Some(mut open) = current.take() else {
            return Ok(Err(Refusal::NotBegun));
        };
        let rolled_back = open
            .transaction
            .as_ref()
            .expect("new begin owns a transaction")
            .rollback()
            .await
            .map_err(Refusal::Postgres);
        open.postgres.operation_metrics.record(
            &open.source,
            if rolled_back.is_ok() {
                DeliveryClass::Delivered
            } else {
                DeliveryClass::ExecutionFailed
            },
        );
        open.settled = true;
        let request_trace = open.request_trace.take();
        drop(open);
        accessor.with(|mut access| {
            let mut ctx = access.get();
            ctx.component_id = Arc::clone(&store.workload);
            swap_request_trace(ctx.table, request_trace);
        });
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
            .is_some_and(|open| open.transaction.is_none())
        {
            return Ok(Err(Refusal::NotBegun));
        }
        let Some(mut open) = current.take() else {
            return Ok(Err(Refusal::NotBegun));
        };
        let transaction = open
            .transaction
            .as_ref()
            .expect("new begin owns a transaction");
        if !open.postgres.active_read(&open.scope) {
            transaction.finish_opaque(&open.operation, &open.key, result);
        }
        let committed = transaction.commit().await.map_err(|error| match error {
            CommitFailure::RolledBack(error) => Refusal::Postgres(error),
            CommitFailure::Uncertain(_) => Refusal::Uncertain,
        });
        open.postgres.operation_metrics.record(
            &open.source,
            if committed.is_ok() {
                DeliveryClass::Delivered
            } else {
                DeliveryClass::ExecutionFailed
            },
        );
        open.settled = true;
        let request_trace = open.request_trace.take();
        // Drop clears all authority on both success and refusal.
        drop(open);
        accessor.with(|mut access| {
            let mut ctx = access.get();
            ctx.component_id = Arc::clone(&store.workload);
            swap_request_trace(ctx.table, request_trace);
        });
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
    if current.is_some() {
        return Ok(Err(Refusal::AlreadyBegun));
    }
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
    let trace = trace.in_scope(|| {
        entry_invocation_span(&postgres, &store.workload, caller.as_ref(), &operation)
            .map_or_else(InvocationTrace::capture, |span| {
                span.in_scope(InvocationTrace::capture)
            })
    });
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
    if !matches!(&result, Ok((Some(_), _))) {
        let class = match &result {
            Ok(_) | Err(Refusal::Conflict | Refusal::Uncertain | Refusal::Resolved(_)) => {
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
                    ctx.component_id = Arc::from(open.scope.as_str());
                    // The operation's effects parent to `wamn.component.invoke`.
                    open.request_trace = swap_request_trace(ctx.table, Some(trace));
                });
            }
            *current = open;
            Ok(Ok(begun))
        }
        Err(refusal) => Ok(Err(refusal)),
    }
}

/// Open the entry operation's `wamn.component.invoke` span in the current span.
///
/// An unknown operation opens none; [`begin`] refuses it.
fn entry_invocation_span(
    postgres: &WamnPostgres,
    workload: &str,
    caller: Option<&AuthenticatedCaller>,
    operation: &str,
) -> Option<tracing::Span> {
    let facts = postgres
        .operation_workloads
        .read()
        .expect("operation workloads lock poisoned")
        .get(workload)
        .cloned()?;
    let release = facts.release.scope();
    let span = wamn_engine::invocation_trace::invocation_span(
        &wamn_engine::invocation_trace::InvocationSite {
            tenant_id: &release.tenant_id,
            project: facts.claims.project.as_deref().unwrap_or_default(),
            environment: &release.environment,
            wiring_id: "",
            wiring_version: 0,
            node_id: "",
            input_port: None,
            operation,
            component_digest: facts.digests.get(operation)?,
            caller,
        },
        None,
    );
    if let Some(caller) = caller {
        span.record("wamn.attachment_id", caller.attachment_id());
    }
    Some(span)
}

/// Replace the trace of the request's Store, and return the one it held.
fn swap_request_trace(
    table: &mut ResourceTable,
    trace: Option<InvocationTrace>,
) -> Option<InvocationTrace> {
    let trace = trace?;
    let request = table
        .iter_mut()
        .find_map(|entry| entry.downcast_mut::<RequestTrace>())?;
    Some(std::mem::replace(&mut request.0, trace))
}

async fn begin(
    postgres: Arc<WamnPostgres>,
    workload: &str,
    caller: Option<&AuthenticatedCaller>,
    operation: String,
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
    let statements = facts
        .operations
        .get(&operation)
        .ok_or(Refusal::UnknownOperation)?;
    if statements.is_read() && !authorize_only {
        return Err(Refusal::ReadsClaimNothing);
    }
    wamn_engine::router_delivery::authorize_registered_operation(
        caller,
        facts.grants[&operation].as_deref(),
    )
    .map_err(|_| Refusal::GrantDenied)?;
    // A present handle was issued for an attachment of this loaded release.
    if let Some(caller) = caller
        && facts
            .release
            .manifest()
            .attachment(caller.attachment_id())
            .is_none()
    {
        return Err(Refusal::InvalidCaller);
    }
    let delivery = uuid::Uuid::new_v4().to_string();
    let source = caller.map_or_else(
        || operation.clone(),
        |caller| caller.attachment_id().to_owned(),
    );
    let mut open = OpenOperation {
        postgres: Arc::clone(&postgres),
        scope: wamn_engine::operation::next_scope(workload).into_string(),
        operation,
        key,
        transaction: None,
        source,
        settled: true,
        request_trace: None,
    };
    let claims = SessionClaims {
        schema: facts
            .schemas
            .get(&open.operation)
            .cloned()
            .or_else(|| facts.claims.schema.clone()),
        user_id: caller.map(|caller| caller.principal_id().to_owned()),
        operation: Some(open.operation.clone()),
        application: Some(facts.applications[&open.operation].clone()),
        ..facts.claims.clone()
    };
    if authorize_only {
        // The host-issued caller already passed authentication and grant checks.
        postgres
            .bind_item_session_claims(&open.scope, &claims)
            .map_err(|error| binding_error(OperationErrorKind::Claims, &open.scope, error))?;
    } else {
        postgres
            .bind_session_claims(&open.scope, &claims)
            .await
            .map_err(|error| binding_error(OperationErrorKind::Claims, &open.scope, error))?;
    }
    postgres.set_current_run(
        &open.scope,
        Some(wamn_event_wire::Causation {
            run: delivery.clone(),
            root: delivery.clone(),
            depth: 0,
        }),
    );
    postgres
        .bind_prepared_statement_members(&open.scope, &facts.members)
        .and_then(|()| {
            postgres.bind_prepared_statement_operation(&open.scope, &open.operation, statements)
        })
        .and_then(|()| postgres.activate_statement_operation(&open.scope, &open.operation))
        .map_err(|error| binding_error(OperationErrorKind::Statements, &open.scope, error))?;
    if authorize_only {
        open.settled = false;
        return Ok((Some(open), Begun::New(delivery)));
    }
    let transaction = postgres
        .begin_operation_transaction(&open.scope)
        .await
        .map_err(Refusal::Postgres)?;
    if !postgres.active_read(&open.scope) {
        match transaction
            .begin_opaque(&open.operation, &open.key, request)
            .await
            .map_err(Refusal::Postgres)?
        {
            OpaqueBegun::New => {}
            OpaqueBegun::Finished(result) => return Ok((None, Begun::Finished(result))),
            OpaqueBegun::Uncertain => return Err(Refusal::Uncertain),
            OpaqueBegun::Conflict => return Err(Refusal::Conflict),
        }
    }
    postgres
        .bind_operation_transaction(&open.scope, &transaction)
        .map_err(|error| binding_error(OperationErrorKind::Transaction, &open.scope, error))?;
    open.transaction = Some(transaction);
    open.settled = false;
    Ok((Some(open), Begun::New(delivery)))
}

#[cfg(test)]
mod tests;
