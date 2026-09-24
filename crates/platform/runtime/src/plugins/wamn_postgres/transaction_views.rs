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
    // At most one outstanding view per live invocation.
    owners: HashMap<String, Arc<TransactionViewLease>>,
}

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
    selected: Option<super::statement_wit::Participation>,
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

/// The view that one invocation's transaction lends to its published
/// participant. An application composes at build, so the participant runs in
/// the same invocation scope as the base that selected it.
pub(super) struct TransactionViewLease {
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
    acquired: bool,
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

    /// Bind transaction access only after native operation authorization succeeds.
    pub fn bind_transaction_scope(&self, scope: &str, deadline: Instant) -> anyhow::Result<()> {
        // Non-release calls retain ordinary SQL, but cannot acquire a view.
        let Ok(identity) = self.view_identity(scope) else {
            return Ok(());
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
        views.scopes.insert(
            scope.to_owned(),
            ViewInvocation {
                identity,
                deadline,
                selected: None,
            },
        );
        Ok(())
    }

    /// Bind the participant that the release publishes for this entry.
    pub fn bind_selected_participant(
        &self,
        scope: &str,
        operation: String,
        intent: String,
    ) -> anyhow::Result<()> {
        let identity = self
            .view_identity(scope)
            .map_err(|_| anyhow::anyhow!("participant-owner-unbound"))?;
        let mut views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        let invocation = views
            .scopes
            .get_mut(scope)
            .ok_or_else(|| anyhow::anyhow!("participant-owner-revoked"))?;
        anyhow::ensure!(
            invocation.identity == identity
                && Instant::now() < invocation.deadline
                && invocation.selected.is_none(),
            "participant-owner-mismatch"
        );
        invocation.selected = Some(super::statement_wit::Participation { operation, intent });
        Ok(())
    }

    fn selected_participation(
        &self,
        scope: &str,
    ) -> Result<Option<super::statement_wit::Participation>, StatementError> {
        let identity = self.view_identity(scope)?;
        let views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        let invocation = views.scopes.get(scope).ok_or_else(denied)?;
        if invocation.identity != identity || Instant::now() >= invocation.deadline {
            return Err(denied());
        }
        Ok(invocation.selected.clone())
    }

    /// Revoke before native authority or its transaction owner leaves the call.
    pub fn revoke_transaction_scope(&self, scope: &str) {
        let mut views = self
            .transaction_views
            .lock()
            .expect("transaction views lock poisoned");
        views.scopes.remove(scope);
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
        // Only the participant that the release publishes for this entry.
        if invocation.identity != identity
            || Instant::now() >= invocation.deadline
            || invocation
                .selected
                .as_ref()
                .is_none_or(|selected| selected.operation != operation)
        {
            return Err(denied());
        }
        let deadline = invocation.deadline;
        if views.owners.get(scope).is_some_and(|view| {
            let access = view.access.lock().expect("transaction view lock poisoned");
            access.active || access.in_flight
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
            operation,
            identity,
            deadline,
            transaction: Arc::downgrade(&transaction.transaction.state),
            destroyed: Arc::clone(&transaction.transaction.destroyed),
            row_limit: transaction.transaction.row_limit,
            access: Mutex::new(ViewAccess {
                active: true,
                acquired: false,
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
        let view = views.owners.get(scope).ok_or_else(denied)?;
        let mut access = view.access.lock().expect("transaction view lock poisoned");
        if invocation.identity != identity
            || view.identity != identity
            || Instant::now() >= invocation.deadline
            || Instant::now() >= view.deadline
            || !access.active
            || access.acquired
        {
            return Err(denied());
        }
        access.acquired = true;
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
                Arc::clone(views.owners.get(scope).ok_or_else(denied)?),
                invocation.deadline,
            )
        };
        let state = view.transaction.upgrade().ok_or_else(denied)?;
        let usage = {
            let mut access = view.access.lock().expect("transaction view lock poisoned");
            if !access.active
                || !access.acquired
                || access.in_flight
                || resource.scope != scope
                || !Arc::ptr_eq(&resource.lease, &view)
                || view.identity != identity
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
                // The participant's statement records its own operation, and
                // the entry's operation returns for the base that resumes.
                connection.connection().query_one(
                    "SELECT set_config('app.operation', $1, true)", &[&view.operation],
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

pub(super) fn selected<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
) -> wash_runtime::wasmtime::Result<
    Result<Option<super::statement_wit::Participation>, StatementError>,
> {
    accessor.with(|mut access| {
        let ctx = access.get();
        Ok(plugin_of(&ctx)?.selected_participation(ctx.component_id.as_ref()))
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
            wamn_engine::invocation_trace::invocation_trace(&ctx),
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
