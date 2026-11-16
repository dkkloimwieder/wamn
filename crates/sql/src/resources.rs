//! Streaming SQL and handles borrowed from the host operation transaction.

use super::pool::{Object, destroy_connection};
use super::types::map_pg_error;
use super::{PgError, SqlValue, WAMN_POSTGRES_ID, WamnPostgres, client, statement_wit};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use wamn_authority::effect_span::{
    EffectIdentity, EffectRun, effect_span, record_effect_ms, record_wiring,
};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx};
use wash_runtime::wasmtime::component::{Accessor, Resource};

#[derive(Debug)]
pub(super) struct TxnState {
    /// The xid returned by the host's identity insert, never a guest transaction.
    pub(super) xid: Option<String>,
    /// Present while the transaction owns a connection. Taken out for the
    /// duration of each call (a std mutex guard cannot be held across await).
    pub(super) conn: Option<Object>,
    /// True once COMMIT or ROLLBACK ran (connection repooled).
    pub(super) finished: bool,
}

pub(super) type SharedTxnState = Arc<std::sync::Mutex<TxnState>>;

pub(super) use wamn_connections::database::ConnectionGuard as StatementConnectionGuard;

pub(super) fn restore_connection(connection: StatementConnectionGuard, state: &SharedTxnState) {
    let Ok(mut transaction) = state.lock() else {
        return;
    };
    if !transaction.finished {
        transaction.conn = Some(connection.into_connection());
    }
}

/// Connection owned by the host operation transaction.
///
/// The [`Drop`] impl is the crash-safety guarantee: if the resource dies
/// without an explicit finish — guest trap, epoch kill, store teardown — the
/// connection is destroyed (socket closed, server aborts the transaction),
/// never repooled.
#[derive(Debug)]
pub struct PgTransaction {
    pub(super) state: SharedTxnState,
    pub(super) destroyed: Arc<AtomicU64>,
    /// A guest handle of an operation transaction that the host owns. It never
    /// finishes the transaction, and dropping it leaves the connection open.
    pub(super) lent: bool,
}

impl PgTransaction {
    /// A handle of this transaction that the host lends to the guest.
    pub(super) fn lent(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            destroyed: Arc::clone(&self.destroyed),
            lent: true,
        }
    }
}

/// Guest handle of the transaction owned by the host operation.
#[derive(Debug)]
pub struct PgStatementTransaction {
    pub(super) transaction: PgTransaction,
    pub(super) owner_scope: String,
}

impl Drop for PgTransaction {
    fn drop(&mut self) {
        // The owner of an operation transaction finishes it.
        if self.lent {
            return;
        }
        let mut st = match self.state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let finished = st.finished;
        st.finished = true;
        let connection = st.conn.take();
        drop(st);
        if let Some(obj) = connection {
            if finished {
                drop(obj); // clean: back to the pool
            } else {
                tracing::warn!(
                    "wamn:postgres transaction dropped without commit/rollback; destroying connection"
                );
                destroy_connection(obj, &self.destroyed);
            }
        }
    }
}

fn txn_closed() -> PgError {
    PgError::QueryError((
        "WAMN2".to_string(),
        "transaction already finished or connection lost".to_string(),
    ))
}

pub(super) fn take_conn(state: &SharedTxnState) -> Result<Object, PgError> {
    let mut st = state.lock().map_err(|_| txn_closed())?;
    if st.finished {
        return Err(txn_closed());
    }
    st.conn.take().ok_or_else(txn_closed)
}

/// Run `op` with the transaction's connection. Fatal (connection-level)
/// errors destroy the connection and poison the transaction; statement-level
/// errors return the connection to the slot (the transaction is aborted
/// server-side until the guest rolls back, mirroring libpq semantics).
pub(super) async fn with_txn_conn<T, F, Fut>(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    op: F,
) -> Result<T, PgError>
where
    F: FnOnce(StatementConnectionGuard) -> Fut,
    Fut: std::future::Future<Output = (StatementConnectionGuard, Result<T, sqlx::Error>)>,
{
    let connection = StatementConnectionGuard::new(take_conn(state)?, Arc::clone(destroyed));
    let (connection, result) = op(connection).await;
    match result {
        Ok(v) => {
            restore_connection(connection, state);
            Ok(v)
        }
        Err(e) => {
            let mapped = map_pg_error(&e);
            if super::types::connection_lost(&e) {
                if let Ok(mut st) = state.lock() {
                    st.finished = true;
                }
                drop(connection);
            } else {
                restore_connection(connection, state);
            }
            Err(mapped)
        }
    }
}

pub(super) fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<WamnPostgres>> {
    ctx.try_get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)
}

/// [9.8] Guest DB-call latency histogram (ms), labelled by `db.operation`
/// (dynamic and operation SQL) and `wamn.project`. On the global
/// meter beside the 9.1 `wamn.postgres` span — a no-op until a provider is
/// installed (`OTEL_*`). Recorded around the awaited call at each `db_span` site.
///
/// PUBLISHED, AND FROZEN. `tests/integration/src/metricbench.rs` polls
/// `wamn_postgres_query_duration_ms_count` against the running collector and
/// blocks the in-cluster acceptance test on it, and asserts it again over pinned fixture
/// text; `docs/archive/observability/dashboards.md` slices a deployed Grafana
/// panel by `db_operation`. Renaming either the instrument or the label breaks
/// the condition loudly and the dashboards silently.
static QUERY_DURATION_MS: std::sync::LazyLock<opentelemetry::metrics::Histogram<f64>> =
    std::sync::LazyLock::new(|| {
        opentelemetry::global::meter("wamn-postgres")
            .f64_histogram("wamn.postgres.query.duration_ms")
            .with_description("wamn:postgres guest DB call latency in ms, by db.operation")
            .build()
    });

/// The operation label of [`QUERY_DURATION_MS`]. Frozen with the instrument.
const DB_OPERATION: &str = "db.operation";

/// Record one guest DB call's wall time on [`QUERY_DURATION_MS`]. `op` matches
/// the `db_span` operation; `project` is the executing component's project.
pub(super) fn record_query_ms(
    op: &'static str,
    project: &wamn_registry::ProjectId,
    elapsed: std::time::Duration,
) {
    record_effect_ms(
        &QUERY_DURATION_MS,
        DB_OPERATION,
        op,
        project.as_str(),
        elapsed,
    );
}

/// [9.1] A `wamn.postgres` span over one guest DB call, enriched host-side with
/// the executing component's authenticated project. The name and the `db.*` fields are
/// this surface's own, while the `wamn.*` identity block is [`effect_span`]'s, shared with
/// every other effect surface.
///
/// The run and the wiring position come from the two identity registries the
/// router driver binds before the pooled instance runs: `set_current_run` for
/// the run, `bind_invocation` for the node (`wamn-0h0g.7.9`). Both are
/// host-attested, so this span says which run and which node raised the call
/// without walking its `wamn.component.invoke` parent (`wamn-0h0g.24.14`).
///
/// A call outside a node walk — a platform read, or a pooled instance between
/// invocations — holds neither, and records the keys empty. `wamn.requirement`
/// is empty on every postgres span: it names the connection requirement an
/// HTTP effect was admitted under. SQL uses the transaction identity.
pub(super) fn db_span_for_project(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &wamn_registry::ProjectId,
    op: &'static str,
) -> tracing::Span {
    let project_env_key = plugin.project_env_key_for(component_id);
    let run = plugin.current_run_for(component_id);
    let span = effect_span!(
        "wamn.postgres",
        EffectIdentity {
            project_env_key: project_env_key.as_ref(),
            project: project.as_str(),
            component: component_id,
        },
        run.as_ref().map(|run| EffectRun {
            run_id: &run.run,
            requirement: "",
        }),
        db.system = "postgresql",
        db.operation = op,
    );
    let invocation = plugin.invocation(component_id);
    record_wiring(
        &span,
        invocation
            .as_ref()
            .map(wamn_connections::connection::ConnectionInvocation::effect_wiring),
    );
    span
}

pub(super) async fn prepare_statement_transaction(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &wamn_registry::ProjectId,
) -> Result<
    (
        PgTransaction,
        super::session_identity::transactions::BoundIdentity,
    ),
    PgError,
> {
    let project_env_key = plugin.require_project_env_key(component_id)?;
    let schema = plugin.schema_for(component_id);
    let runner = plugin.runner_for(component_id);
    let role = plugin.role_for(component_id);
    let user_id = plugin.user_id_for(component_id);
    let operation = plugin.operation_for(component_id);
    let application = plugin.application_for(component_id);

    let (connection, policy, authority) = plugin
        .checkout_workload(component_id, project, &project_env_key)
        .await?;
    let connection = StatementConnectionGuard::new(connection, Arc::clone(&plugin.destroyed));
    let identity = WamnPostgres::prepare_identity(
        authority,
        plugin.active_read(component_id),
        schema.as_deref(),
        runner.as_deref(),
        role.as_deref(),
        user_id.as_ref(),
        operation.as_ref(),
        application.as_ref(),
        None,
        policy.statement_timeout_ms,
    )?;
    Ok((
        PgTransaction {
            state: Arc::new(std::sync::Mutex::new(TxnState {
                conn: Some(connection.into_connection()),
                xid: None,
                finished: false,
            })),
            destroyed: Arc::clone(&plugin.destroyed),
            lent: false,
        },
        identity,
    ))
}

impl client::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> client::HostWithStore<T> for SharedCtx {
    async fn query(
        accessor: &Accessor<T, Self>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> super::query::QueryResult {
        super::query::dynamic_query(accessor, sql, params).await
    }
}

impl statement_wit::Host for ActiveCtx<'_> {}
impl<T: 'static + Send> statement_wit::HostWithStore<T> for SharedCtx {
    async fn operation_transaction(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, PgError>> {
        lend_operation_transaction(accessor).await
    }
}

impl<T: 'static + Send> statement_wit::HostTransactionWithStore<T> for SharedCtx {
    async fn query(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> super::query::QueryResult {
        super::query::transaction_query(accessor, &rep, sql, params).await
    }
    async fn query_batch(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
        sql: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        super::query::transaction_batch(accessor, &rep, sql).await
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated destructor only releases a borrowed handle"
)]
impl statement_wit::HostTransaction for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<()> {
        drop(self.table.delete(rep)?);
        Ok(())
    }
}

async fn lend_operation_transaction<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, PgError>> {
    let (transaction, component_id) = accessor.with(|mut access| {
        let ctx = access.get();
        let plugin = plugin_of(&ctx)?;
        let component_id = ctx.component_id.to_string();
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin.operation_transaction_for(&component_id),
            component_id,
        ))
    })?;
    let Some(transaction) = transaction else {
        return Ok(Err(PgError::PermissionDenied));
    };
    let transaction = match transaction.lend().await {
        Ok(transaction) => transaction,
        Err(error) => return Ok(Err(error)),
    };
    accessor.with(|mut access| {
        access
            .get()
            .table
            .push(PgStatementTransaction {
                transaction,
                owner_scope: component_id,
            })
            .map(Ok)
            .map_err(Into::into)
    })
}

pub(super) async fn finish_statement_txn(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    verb: &str,
) -> Result<(), PgError> {
    let mut connection = StatementConnectionGuard::new(take_conn(state)?, Arc::clone(destroyed));
    match super::session_identity::transactions::finish_transaction(connection.connection(), verb)
        .await
    {
        Ok(()) => {
            if let Ok(mut transaction) = state.lock() {
                transaction.finished = true;
            }
            connection.repool();
            Ok(())
        }
        Err(error) => {
            if let Ok(mut transaction) = state.lock() {
                transaction.finished = true;
            }
            Err(map_pg_error(&error))
        }
    }
}

#[cfg(feature = "wasm_component_model_implements")]
impl super::bindings::named_imports::wamn::postgres::statements::Host for ActiveCtx<'_> {}

#[cfg(feature = "wasm_component_model_implements")]
impl<T: 'static + Send> super::bindings::named_imports::wamn::postgres::statements::HostWithStore<T>
    for SharedCtx
{
    async fn operation_transaction(
        accessor: &Accessor<T, Self>,
        _id: super::NamedStatements,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, PgError>> {
        lend_operation_transaction(accessor).await
    }
}
#[cfg(feature = "wasm_component_model_implements")]
impl<T: 'static + Send>
    super::bindings::named_imports::wamn::postgres::statements::HostTransactionWithStore<T>
    for SharedCtx
{
    async fn query(
        accessor: &Accessor<T, Self>,
        _id: super::NamedStatements,
        rep: Resource<PgStatementTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> super::query::QueryResult {
        super::query::transaction_query(accessor, &rep, sql, params).await
    }
    async fn query_batch(
        accessor: &Accessor<T, Self>,
        _id: super::NamedStatements,
        rep: Resource<PgStatementTransaction>,
        sql: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        super::query::transaction_batch(accessor, &rep, sql).await
    }
}
#[cfg(feature = "wasm_component_model_implements")]
#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated destructor only releases a borrowed handle"
)]
impl super::bindings::named_imports::wamn::postgres::statements::HostTransaction for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        _id: super::NamedStatements,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<()> {
        drop(self.table.delete(rep)?);
        Ok(())
    }
}
