//! Call-scoped execution access to an existing PostgreSQL transaction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::time::Instant;
use tracing::Instrument as _;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::wasmtime::component::{Accessor, Resource};

use super::resources::{
    PgStatementTransaction, SharedTxnState, StatementConnectionGuard, TxnState,
    admit_transaction_statement, db_span_for_project, plugin_of, record_query_ms,
    run_verified_query, take_conn,
};
use super::{PgError, RowSet, SessionClaims, SqlValue, StatementError, WamnPostgres};

#[derive(Debug, Default)]
pub(super) struct TransactionViews {
    scopes: HashMap<String, ViewInvocation>,
    // At most one outstanding view per live owner invocation.
    owners: HashMap<String, Arc<TransactionViewLease>>,
}

/// Host-selected participation carried into one native invocation.
///
/// This reference retains no transaction connection. Revocation invalidates it
/// even if native dispatch has not activated the participant yet.
#[derive(Debug, Clone)]
pub struct TransactionParticipation(Arc<TransactionViewLease>);

/// Execution-only resource held in the authorized participant's resource table.
#[derive(Debug, Clone)]
pub struct PgTransactionView {
    lease: Arc<TransactionViewLease>,
    scope: String,
}

#[derive(Debug)]
struct ViewInvocation {
    identity: ViewIdentity,
    deadline: Instant,
    participant: Option<Arc<TransactionViewLease>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ViewIdentity {
    claims: SessionClaims,
    authority: super::AuthorityClass,
    package: String,
    component_digest: String,
    operation: String,
    origin: crate::plugins::connection_http::ConnectionOrigin,
}

impl ViewIdentity {
    fn same_transaction(&self, other: &Self) -> bool {
        // The native dispatcher separately admits the exact target operation,
        // component digest and package from the same release closure.
        self.claims == other.claims
            && self.authority == other.authority
            && self.origin == other.origin
    }
}

pub(super) struct TransactionViewLease {
    owner: String,
    operation: String,
    identity: ViewIdentity,
    deadline: Instant,
    transaction: Weak<Mutex<TxnState>>,
    destroyed: Arc<std::sync::atomic::AtomicU64>,
    row_limit: u64,
    access: Mutex<ViewAccess>,
    changed: tokio::sync::Notify,
}

impl std::fmt::Debug for TransactionViewLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TransactionViewLease")
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct ViewAccess {
    active: bool,
    participant: Option<String>,
    in_flight: bool,
}

impl TransactionViewLease {
    pub(super) fn revoke(&self) {
        self.access
            .lock()
            .expect("transaction view lock poisoned")
            .active = false;
        self.changed.notify_waiters();
    }

    async fn revoked(&self) {
        loop {
            let changed = self.changed.notified();
            if !self
                .access
                .lock()
                .expect("transaction view lock poisoned")
                .active
            {
                return;
            }
            changed.await;
        }
    }
}

struct ViewUse(Arc<TransactionViewLease>);

impl Drop for ViewUse {
    fn drop(&mut self) {
        self.0
            .access
            .lock()
            .expect("transaction view lock poisoned")
            .in_flight = false;
        self.0.changed.notify_waiters();
    }
}

fn denied() -> StatementError {
    StatementError::Postgres(PgError::PermissionDenied)
}

impl WamnPostgres {
    pub(super) fn refuse_independent_participant_sql(&self, scope: &str) -> Result<(), PgError> {
        let views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        if views
            .scopes
            .get(scope)
            .is_some_and(|invocation| invocation.participant.is_some())
        {
            return Err(PgError::PermissionDenied);
        }
        Ok(())
    }

    fn view_identity(&self, scope: &str) -> Result<ViewIdentity, StatementError> {
        let invocation = self.invocation(scope).ok_or_else(denied)?;
        let release = self.release_identity_for(scope).ok_or_else(denied)?;
        let user_id = self.user_id_for(scope).ok_or_else(denied)?;
        let tenant = self.tenant_for(scope).ok_or_else(denied)?;
        if self.operation_for(scope).as_deref() != Some(invocation.operation.as_str()) {
            return Err(denied());
        }
        Ok(ViewIdentity {
            claims: SessionClaims {
                tenant,
                project: Some(self.project_for(scope)),
                schema: self.schema_for(scope),
                runner: self.runner_for(scope),
                role: self.role_for(scope),
                user_id: Some(user_id),
                operation: None,
                release: Some(release),
            },
            authority: self.workload_authority_for(scope),
            package: invocation.package_id,
            component_digest: invocation.component_digest,
            operation: invocation.operation,
            origin: invocation.origin,
        })
    }

    /// Select a pending view after the dispatcher admits the exact target.
    pub fn prepare_transaction_participation(
        &self,
        owner_scope: &str,
        operation: &str,
    ) -> anyhow::Result<Option<TransactionParticipation>> {
        let identity = self.view_identity(owner_scope);
        anyhow::ensure!(
            self.invocation(owner_scope).is_some(),
            "transaction-view-owner-unbound"
        );
        let views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        if let Ok(identity) = identity {
            anyhow::ensure!(
                views.scopes.get(owner_scope).is_some_and(
                    |bound| bound.identity == identity && Instant::now() < bound.deadline
                ),
                "transaction-view-owner-revoked"
            );
        }
        let Some(view) = views
            .owners
            .get(owner_scope)
            .filter(|view| view.operation == operation)
        else {
            return Ok(None);
        };
        let access = view.access.lock().expect("transaction view lock poisoned");
        anyhow::ensure!(
            access.active && access.participant.is_none() && Instant::now() < view.deadline,
            "transaction-view-unavailable"
        );
        Ok(Some(TransactionParticipation(Arc::clone(view))))
    }

    /// Bind transaction access only after native operation authorization succeeds.
    pub fn bind_transaction_scope(
        &self,
        scope: &str,
        deadline: Instant,
        participation: Option<&TransactionParticipation>,
    ) -> anyhow::Result<()> {
        // Non-release calls retain ordinary SQL, but cannot acquire a view.
        let identity = match self.view_identity(scope) {
            Ok(identity) => identity,
            Err(_) if participation.is_none() => return Ok(()),
            Err(_) => anyhow::bail!("transaction-view-participant-identity-unbound"),
        };
        anyhow::ensure!(
            Instant::now() < deadline,
            "transaction-view-deadline-expired"
        );
        let mut views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        anyhow::ensure!(
            !views.scopes.contains_key(scope),
            "transaction-view-scope-already-bound"
        );
        let participant = if let Some(TransactionParticipation(view)) = participation {
            let mut access = view.access.lock().expect("transaction view lock poisoned");
            anyhow::ensure!(
                access.active
                    && access.participant.is_none()
                    && view.operation == identity.operation
                    && view.identity.same_transaction(&identity)
                    && deadline <= view.deadline
                    && Instant::now() < view.deadline
                    && views
                        .owners
                        .get(&view.owner)
                        .is_some_and(|current| Arc::ptr_eq(current, view)),
                "transaction-view-participant-mismatch"
            );
            access.participant = Some(scope.to_owned());
            Some(Arc::clone(view))
        } else {
            None
        };
        views.scopes.insert(
            scope.to_owned(),
            ViewInvocation {
                identity,
                deadline,
                participant,
            },
        );
        Ok(())
    }

    /// A participant cannot delegate work beyond its execution-only view.
    pub fn permit_transaction_nested_call(&self, scope: &str) -> anyhow::Result<()> {
        self.refuse_independent_participant_sql(scope)
            .map_err(|_| anyhow::anyhow!("transaction-participant-cannot-delegate"))
    }

    /// Revoke before native authority or its transaction owner leaves the call.
    pub fn revoke_transaction_scope(&self, scope: &str) {
        let mut views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        if let Some(invocation) = views.scopes.remove(scope)
            && let Some(view) = invocation.participant
        {
            view.revoke();
            if views
                .owners
                .get(&view.owner)
                .is_some_and(|current| Arc::ptr_eq(current, &view))
            {
                views.owners.remove(&view.owner);
            }
        }
        if let Some(view) = views.owners.remove(scope) {
            view.revoke();
        }
    }

    fn select_transaction_participant(
        &self,
        scope: &str,
        transaction: &PgStatementTransaction,
        operation: String,
    ) -> Result<(), StatementError> {
        let identity = self.view_identity(scope)?;
        if transaction.owner_scope != scope {
            return Err(denied());
        }
        let mut views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        let invocation = views.scopes.get(scope).ok_or_else(denied)?;
        if invocation.participant.is_some()
            || invocation.identity != identity
            || Instant::now() >= invocation.deadline
            || operation.is_empty()
        {
            return Err(denied());
        }
        let deadline = invocation.deadline;
        if views.owners.get(scope).is_some_and(|view| {
            let access = view.access.lock().expect("transaction view lock poisoned");
            access.active
                || access.in_flight
                || access
                    .participant
                    .as_ref()
                    .is_some_and(|participant| views.scopes.contains_key(participant))
        }) {
            return Err(denied());
        }
        let mut state = transaction
            .transaction
            .state
            .lock()
            .expect("transaction lock poisoned");
        if state.finished || state.conn.is_none() {
            return Err(denied());
        }
        let view = Arc::new(TransactionViewLease {
            owner: scope.to_owned(),
            operation,
            identity,
            deadline,
            transaction: Arc::downgrade(&transaction.transaction.state),
            destroyed: Arc::clone(&transaction.transaction.destroyed),
            row_limit: transaction.transaction.row_limit,
            access: Mutex::new(ViewAccess {
                active: true,
                participant: None,
                in_flight: false,
            }),
            changed: tokio::sync::Notify::new(),
        });
        state.view = Some(Arc::downgrade(&view));
        views.owners.insert(scope.to_owned(), view);
        Ok(())
    }

    fn acquire_transaction_view(&self, scope: &str) -> Result<PgTransactionView, StatementError> {
        let identity = self.view_identity(scope)?;
        let views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        let invocation = views.scopes.get(scope).ok_or_else(denied)?;
        let view = invocation.participant.as_ref().ok_or_else(denied)?;
        let access = view.access.lock().expect("transaction view lock poisoned");
        if invocation.identity != identity
            || Instant::now() >= invocation.deadline
            || Instant::now() >= view.deadline
            || !access.active
            || access.participant.as_deref() != Some(scope)
        {
            return Err(denied());
        }
        Ok(PgTransactionView {
            lease: Arc::clone(view),
            scope: scope.to_owned(),
        })
    }

    async fn run_transaction_view(
        &self,
        scope: &str,
        resource: &PgTransactionView,
        digest: &str,
        binds: &[SqlValue],
    ) -> Result<RowSet, StatementError> {
        let identity = self.view_identity(scope)?;
        let (view, deadline) = {
            let views = self
                .transaction_views
                .lock()
                .expect("transaction views lock poisoned");
            let invocation = views.scopes.get(scope).ok_or_else(denied)?;
            if invocation.identity != identity || Instant::now() >= invocation.deadline {
                return Err(denied());
            }
            (
                invocation.participant.clone().ok_or_else(denied)?,
                invocation.deadline,
            )
        };
        let state = view.transaction.upgrade().ok_or_else(denied)?;
        let usage = {
            let mut access = view.access.lock().expect("transaction view lock poisoned");
            if !access.active
                || access.participant.as_deref() != Some(scope)
                || access.in_flight
                || resource.scope != scope
                || !Arc::ptr_eq(&resource.lease, &view)
                || !view.identity.same_transaction(&identity)
                || view.operation != identity.operation
                || Instant::now() >= view.deadline
            {
                return Err(denied());
            }
            access.in_flight = true;
            ViewUse(Arc::clone(&view))
        };
        let active = self.active_statement_set(scope);
        let statement = admit_transaction_statement(self, scope, active.as_deref(), digest, binds)?;
        let connection = StatementConnectionGuard::new(
            take_conn(&state).map_err(StatementError::Postgres)?,
            Arc::clone(&view.destroyed),
        );
        let result = tokio::select! {
            biased;
            () = view.revoked() => Err(denied()),
            () = tokio::time::sleep_until(deadline) => Err(StatementError::Postgres(PgError::StatementTimeout)),
            result = async {
                connection.connection().query_one(
                    "SELECT set_config('app.operation', $1, true)", &[&identity.operation],
                ).await.map_err(|error| StatementError::Postgres(super::types::map_pg_error(&error)))?;
                let rows = run_verified_query(connection.connection(), digest, &statement, binds, view.row_limit).await?;
                connection.connection().query_one(
                    "SELECT set_config('app.operation', $1, true)", &[&view.identity.operation],
                ).await.map_err(|error| StatementError::Postgres(super::types::map_pg_error(&error)))?;
                Ok(rows)
            } => result,
        };
        {
            let access = view.access.lock().expect("transaction view lock poisoned");
            if result.is_ok() && access.active && Instant::now() < deadline {
                connection.restore(&state);
            } else {
                state.lock().expect("transaction lock poisoned").finished = true;
                drop(connection);
                return result.and(Err(denied()));
            }
        }
        drop(usage);
        result
    }
}

pub(super) async fn finish_view(state: &SharedTxnState) {
    let view = state
        .lock()
        .expect("transaction lock poisoned")
        .view
        .take()
        .and_then(|view| view.upgrade());
    if let Some(view) = view {
        view.revoke();
        loop {
            let changed = view.changed.notified();
            if !view
                .access
                .lock()
                .expect("transaction view lock poisoned")
                .in_flight
            {
                break;
            }
            changed.await;
        }
    }
}

pub(super) fn select<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    rep: &Resource<PgStatementTransaction>,
    operation: String,
) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
    accessor.with(|mut access| {
        let ctx = access.get();
        let transaction = ctx.table.get(rep)?;
        Ok(plugin_of(&ctx)?.select_transaction_participant(
            ctx.component_id.as_ref(),
            transaction,
            operation,
        ))
    })
}

pub(super) fn acquire<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
) -> wash_runtime::wasmtime::Result<Result<Resource<PgTransactionView>, StatementError>> {
    accessor.with(|mut access| {
        let ctx = access.get();
        match plugin_of(&ctx)?.acquire_transaction_view(ctx.component_id.as_ref()) {
            Ok(view) => Ok(Ok(ctx.table.push(view)?)),
            Err(error) => Ok(Err(error)),
        }
    })
}

pub(super) async fn run<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    resource: Resource<PgTransactionView>,
    digest: String,
    binds: Vec<SqlValue>,
) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
    let (plugin, scope, trace, view) = accessor.with(|mut access| {
        let ctx = access.get();
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            crate::plugins::invocation_trace::invocation_trace(&ctx),
            ctx.table.get(&resource)?.clone(),
        ))
    })?;
    trace
        .run(async move {
            let project = plugin.project_for(&scope);
            let span = db_span_for_project(&plugin, &scope, &project, "statement.view.run");
            let started = std::time::Instant::now();
            let result = plugin
                .run_transaction_view(&scope, &view, &digest, &binds)
                .instrument(span)
                .await;
            record_query_ms("statement.view.run", &project, started.elapsed());
            Ok(result)
        })
        .await
}

#[cfg(test)]
mod tests;
