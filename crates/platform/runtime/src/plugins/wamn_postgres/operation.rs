//! Entry-owned operations on the Store's host transaction.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use wamn_catalog::{ComponentSqlField, ComponentSqlStatement, ComponentSqlValueType};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::release_manifest::LoadedRelease;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx};
use wash_runtime::wasmtime::component::{Accessor, Resource, ResourceTable};

use super::bindings::wamn::operation::operation::{self as wit, Begun, Refusal};
use super::write_log::OpaqueBegun;
use super::{
    CommitFailure, OperationTransaction, PreparedStatementMembers, PreparedStatementSet,
    ReleaseIdentity, SessionClaims, StatementField, StatementValueType, VerifiedStatement,
    WAMN_POSTGRES_ID, WamnPostgres,
};

/// Immutable authority installed by the host before a workload starts.
#[derive(Debug)]
pub(super) struct OperationWorkload {
    release: Arc<LoadedRelease>,
    claims: SessionClaims,
    operations: BTreeMap<String, PreparedStatementSet>,
    members: PreparedStatementMembers,
}

impl WamnPostgres {
    /// Bind an entry to one component in the process's loaded release.
    ///
    /// The host supplies the admitted component identity and its database claims.
    /// No current Receiving loader calls this entry interface.
    pub fn bind_operation_workload(
        &mut self,
        workload: String,
        release: Arc<LoadedRelease>,
        package: &str,
        component: &str,
        digest: &str,
        mut claims: SessionClaims,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            claims.tenant == release.scope().tenant_id,
            "operation tenant mismatch"
        );
        anyhow::ensure!(
            claims.release.as_ref() == Some(&ReleaseIdentity::from_loaded(&release)),
            "operation release mismatch"
        );
        anyhow::ensure!(
            !self.operation_workloads.contains_key(&workload),
            "operation workload already bound"
        );
        let served = release
            .manifest()
            .components
            .iter()
            .find(|served| {
                served.package_id == package
                    && served.descriptor.component == component
                    && served.descriptor.component_digest.as_str() == digest
            })
            .context("operation component not in loaded release")?;
        if let Some(schema) = &served.runtime_schema {
            claims.schema = Some(schema.clone());
        }
        let operations = served
            .operations
            .iter()
            .filter(|(id, fact)| fact.registered_operation.as_deref() == Some(id.as_str()))
            .map(|(id, fact)| {
                let read = release.manifest().routes.iter().any(|route| {
                    route.package_id == served.package_id
                        && route.component == served.descriptor.component
                        && &route.operation == id
                        && route.type_.is_read()
                });
                (
                    id.clone(),
                    Self::prepare_statement_set(lower_statements(&fact.statements), read),
                )
            })
            .collect();
        let members = Self::prepare_statement_members(served.statement_instances.iter().map(
            |(name, statements)| {
                (
                    name.clone(),
                    Self::prepare_statement_set(lower_statements(statements), false),
                )
            },
        ));
        self.operation_workloads.insert(
            workload,
            OperationWorkload {
                release,
                claims,
                operations,
                members,
            },
        );
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

/// Store-local lifecycle state. Only `open` marks an operation as begun.
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
}

impl Drop for OpenOperation {
    fn drop(&mut self) {
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

impl wit::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> wit::HostWithStore<T> for SharedCtx {
    async fn begin_operation(
        accessor: &Accessor<T, Self>,
        caller: Resource<AuthenticatedCaller>,
        operation: String,
        idempotency_key: String,
        request_hash: Vec<u8>,
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
            let Ok(caller) = ctx.table.get(&caller).cloned() else {
                return Ok(Err(Refusal::InvalidCaller));
            };
            let postgres = ctx.try_get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)?;
            Ok::<_, wash_runtime::wasmtime::Error>(Ok((postgres, caller)))
        })?;
        let (postgres, caller) = match input {
            Ok(input) => input,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let result = begin(
            postgres,
            &store.workload,
            &caller,
            operation,
            idempotency_key,
            request_hash,
        )
        .await;
        match result {
            Ok((open, begun)) => {
                if let Some(open) = &open {
                    accessor.with(|mut access| {
                        access.get().component_id = Arc::from(open.scope.as_str());
                    });
                }
                *current = open;
                Ok(Ok(begun))
            }
            Err(refusal) => Ok(Err(refusal)),
        }
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
        let Some(open) = current.take() else {
            return Ok(Err(Refusal::NotBegun));
        };
        let transaction = open
            .transaction
            .as_ref()
            .expect("new begin owns a transaction");
        transaction.finish_opaque(&open.operation, &open.key, result);
        let committed = transaction.commit().await.map_err(|error| match error {
            CommitFailure::RolledBack(error) => Refusal::Postgres(error),
            CommitFailure::Uncertain(_) => Refusal::Uncertain,
        });
        // Drop clears all authority on both success and refusal.
        drop(open);
        accessor.with(|mut access| {
            access.get().component_id = Arc::clone(&store.workload);
        });
        Ok(committed)
    }
}

async fn begin(
    postgres: Arc<WamnPostgres>,
    workload: &str,
    caller: &AuthenticatedCaller,
    operation: String,
    key: String,
    request: Vec<u8>,
) -> Result<(Option<OpenOperation>, Begun), Refusal> {
    let facts = postgres
        .operation_workloads
        .get(workload)
        .ok_or(Refusal::UnknownOperation)?;
    let statements = facts
        .operations
        .get(&operation)
        .ok_or(Refusal::UnknownOperation)?;
    if !caller.permits(&operation) {
        return Err(Refusal::GrantDenied);
    }
    // The handle was issued for an attachment of this loaded release.
    if facts
        .release
        .manifest()
        .attachment(caller.attachment_id())
        .is_none()
    {
        return Err(Refusal::InvalidCaller);
    }
    let delivery = uuid::Uuid::new_v4().to_string();
    let mut open = OpenOperation {
        postgres: Arc::clone(&postgres),
        scope: wamn_engine::operation::next_scope(workload).into_string(),
        operation,
        key,
        transaction: None,
    };
    let claims = SessionClaims {
        user_id: Some(caller.principal_id().to_owned()),
        operation: Some(open.operation.clone()),
        ..facts.claims.clone()
    };
    postgres
        .bind_session_claims(&open.scope, &claims)
        .await
        .map_err(|error| binding_error(OperationErrorKind::Claims, &open.scope, error))?;
    postgres.set_current_run(
        &open.scope,
        Some(wamn_event_wire::Causation {
            run: delivery.clone(),
            root: delivery,
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
    let transaction = postgres
        .begin_operation_transaction(&open.scope)
        .await
        .map_err(Refusal::Postgres)?;
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
    postgres
        .bind_operation_transaction(&open.scope, &transaction)
        .map_err(|error| binding_error(OperationErrorKind::Transaction, &open.scope, error))?;
    open.transaction = Some(transaction);
    Ok((Some(open), Begun::New))
}

#[cfg(test)]
mod tests;
