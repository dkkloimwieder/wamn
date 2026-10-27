//! Transaction / cursor resources and the WIT host implementations for
//! `wamn:postgres` (SR4 split, wamn-cjv.18): the crash-safe `PgTransaction` /
//! `PgCursor` handles, the connection-lifecycle helpers, the statement drivers
//! (`run_query` / `run_execute`), and the `client` host traits backed by the
//! `WamnPostgres` plugin resolved from the invoking context.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use deadpool_postgres::Object;
use futures_util::TryStreamExt as _;
use tracing::Instrument as _;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx};
use wash_runtime::wasmtime::component::{Accessor, Resource};

use crate::plugins::effect_span::{
    EffectIdentity, EffectRun, effect_span, record_effect_ms, record_wiring,
};

use super::claims::{OneShotResult, refuse_unattributed_statement, reject_claim_mutation};
use super::numbering::{NumberCall, statement_number};
use super::pool::destroy_connection;
use super::statements::{
    BoundStatementSet, VerifiedStatement, resolve_statement, validate_prepared_statement,
    validate_statement_result,
};
use super::types::{PgParam, columns_of, decode_row, map_pg_error};
use super::{
    PgError, RowSet, SqlValue, StatementError, WAMN_POSTGRES_ID, WamnPostgres, client,
    statement_wit,
};

#[cfg(feature = "wasm_component_model_implements")]
use super::bindings;

#[derive(Debug)]
pub(super) struct TxnState {
    /// Present while the transaction owns a connection. Taken out for the
    /// duration of each call (a std mutex guard cannot be held across await).
    pub(super) conn: Option<Object>,
    /// True once COMMIT or ROLLBACK ran (connection repooled).
    pub(super) finished: bool,
}

pub(super) type SharedTxnState = Arc<std::sync::Mutex<TxnState>>;

/// Owns a checked-out connection across an await.
///
/// Dropping an armed guard destroys the connection, so cancellation cannot
/// fast-repool a session whose claim transaction may still be open.
pub(super) struct StatementConnectionGuard {
    connection: Option<Object>,
    destroyed: Arc<AtomicU64>,
}

impl StatementConnectionGuard {
    pub(super) fn new(connection: Object, destroyed: Arc<AtomicU64>) -> Self {
        Self {
            connection: Some(connection),
            destroyed,
        }
    }

    pub(super) fn connection(&self) -> &Object {
        self.connection
            .as_ref()
            .expect("armed statement connection guard has a connection")
    }

    pub(super) fn into_connection(mut self) -> Object {
        self.connection
            .take()
            .expect("armed statement connection guard has a connection")
    }

    pub(super) fn repool(mut self) {
        drop(self.connection.take());
    }

    pub(super) fn restore(mut self, state: &SharedTxnState) {
        let Ok(mut transaction) = state.lock() else {
            return;
        };
        if !transaction.finished {
            transaction.conn = self.connection.take();
        }
    }
}

impl Drop for StatementConnectionGuard {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            destroy_connection(connection, &self.destroyed);
        }
    }
}

/// Host side of a `wamn:postgres/client.transaction`.
///
/// The [`Drop`] impl is the crash-safety guarantee: if the resource dies
/// without an explicit finish — guest trap, epoch kill, store teardown — the
/// connection is destroyed (socket closed, server aborts the transaction),
/// never repooled.
#[derive(Debug)]
pub struct PgTransaction {
    pub(super) state: SharedTxnState,
    pub(super) destroyed: Arc<AtomicU64>,
    cursor_seq: u32,
    /// Row limit of the project this transaction's connection belongs to.
    pub(super) row_limit: u64,
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
            cursor_seq: 0,
            row_limit: self.row_limit,
            lent: true,
        }
    }
}

/// Host side of a `wamn:postgres/statements.transaction`.
///
/// The operation's statement set is snapshotted at `begin`; changing or
/// revoking the invocation scope cannot widen an already-open transaction.
pub struct PgStatementTransaction {
    pub(super) transaction: PgTransaction,
    pub(super) owner_scope: String,
    pub(super) statements: Option<Arc<BoundStatementSet>>,
}

impl std::fmt::Debug for PgStatementTransaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PgStatementTransaction")
            .finish_non_exhaustive()
    }
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

/// Host side of a `wamn:postgres/client.cursor`. Shares the transaction's
/// connection slot; server-side cursors die with the transaction.
#[derive(Debug)]
pub struct PgCursor {
    state: SharedTxnState,
    destroyed: Arc<AtomicU64>,
    name: String,
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
    Fut: std::future::Future<Output = (StatementConnectionGuard, Result<T, tokio_postgres::Error>)>,
{
    let connection = StatementConnectionGuard::new(take_conn(state)?, Arc::clone(destroyed));
    let (connection, result) = op(connection).await;
    match result {
        Ok(v) => {
            connection.restore(state);
            Ok(v)
        }
        Err(e) => {
            let mapped = map_pg_error(&e);
            if e.is_closed() {
                if let Ok(mut st) = state.lock() {
                    st.finished = true;
                }
                drop(connection);
            } else {
                connection.restore(state);
            }
            Err(mapped)
        }
    }
}

// ---------------------------------------------------------------------------
// Statement execution helpers
// ---------------------------------------------------------------------------

pub(super) async fn run_query(
    conn: &Object,
    sql: &str,
    params: &[SqlValue],
    row_limit: u64,
) -> Result<RowSet, PgError> {
    async {
        reject_claim_mutation(sql)?;
        let stmt = conn
            .prepare_cached(sql)
            .await
            .map_err(|e| map_pg_error(&e))?;
        let columns = columns_of(&stmt);
        let stream = conn
            .query_raw(&stmt, params.iter().map(PgParam))
            .await
            .map_err(|e| map_pg_error(&e))?;
        futures_util::pin_mut!(stream);
        // Row decode was inside wamn.postgres.statement with no span of its own,
        // so wire time and decode time were indistinguishable.
        let rows = async {
            let mut rows = Vec::new();
            while let Some(row) = stream.try_next().await.map_err(|e| map_pg_error(&e))? {
                if rows.len() as u64 >= row_limit {
                    return Err(PgError::RowLimitExceeded(row_limit));
                }
                rows.push(decode_row(&row)?);
            }
            Ok::<_, PgError>(rows)
        }
        .instrument(tracing::info_span!("wamn.postgres.decode_rows"))
        .await?;
        Ok(RowSet {
            columns,
            rows,
            wal_position: None,
        })
    }
    .instrument(tracing::info_span!(
        "wamn.postgres.statement",
        db.system = "postgresql",
        db.operation = "query",
    ))
    .await
}

pub(super) async fn run_execute(
    conn: &Object,
    sql: &str,
    params: &[SqlValue],
) -> Result<u64, PgError> {
    async {
        reject_claim_mutation(sql)?;
        let stmt = conn
            .prepare_cached(sql)
            .await
            .map_err(|e| map_pg_error(&e))?;
        conn.execute_raw(&stmt, params.iter().map(PgParam))
            .await
            .map_err(|e| map_pg_error(&e))
    }
    .instrument(tracing::info_span!(
        "wamn.postgres.statement",
        db.system = "postgresql",
        db.operation = "execute",
    ))
    .await
}

pub(super) async fn run_verified_query(
    conn: &Object,
    statement_path: &str,
    statement: &VerifiedStatement,
    binds: &[SqlValue],
    row_limit: u64,
) -> Result<RowSet, StatementError> {
    async {
        // Admission parses declared SQL and refuses these constructs on the
        // parse tree (`wamn_control::component_admission`, `declared_sql`).
        // This text guard stays as a second layer: a release admitted before
        // that check, or a forged descriptor, was never parsed.
        reject_claim_mutation(&statement.exact_sql).map_err(StatementError::Postgres)?;
        let prepared = conn
            .prepare_cached(&statement.exact_sql)
            .await
            .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?;
        validate_prepared_statement(statement_path, statement, &prepared)?;
        let columns = columns_of(&prepared);
        let stream = conn
            .query_raw(&prepared, binds.iter().map(PgParam))
            .await
            .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?;
        futures_util::pin_mut!(stream);
        // This is the path a released route takes; run_query's decode loop is a
        // different function and spanning only that one measured nothing here.
        let rows = async {
            let mut rows = Vec::new();
            while let Some(row) = stream
                .try_next()
                .await
                .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?
            {
                if rows.len() as u64 >= row_limit {
                    return Err(StatementError::Postgres(PgError::RowLimitExceeded(
                        row_limit,
                    )));
                }
                rows.push(decode_row(&row).map_err(StatementError::Postgres)?);
            }
            Ok::<_, StatementError>(rows)
        }
        .instrument(tracing::info_span!("wamn.postgres.decode_rows"))
        .await?;
        validate_statement_result(statement_path, statement, &columns, &rows)?;
        Ok(RowSet {
            columns,
            rows,
            wal_position: None,
        })
    }
    .instrument(tracing::info_span!(
        "wamn.postgres.statement",
        db.system = "postgresql",
        db.operation = "verified-query",
        statement.path = statement_path,
    ))
    .await
}

pub(super) fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<WamnPostgres>> {
    ctx.try_get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)
}

/// [9.8] Guest DB-call latency histogram (ms), labelled by `db.operation`
/// (query / execute / txn.query / txn.execute) and `wamn.project`. On the global
/// meter beside the 9.1 `wamn.postgres` span — a no-op until a provider is
/// installed (`OTEL_*`). Recorded around the awaited call at each `db_span` site.
///
/// PUBLISHED, AND FROZEN. `tests/integration/src/metricbench.rs` polls
/// `wamn_postgres_query_duration_ms_count` against the running collector and
/// blocks the in-cluster gate on it, and asserts it again over pinned fixture
/// text; `docs/archive/observability/dashboards.md` slices a deployed Grafana
/// panel by `db_operation`. Renaming either the instrument or the label breaks
/// the gate loudly and the dashboards silently.
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
pub(super) fn record_query_ms(op: &'static str, project: &str, elapsed: std::time::Duration) {
    record_effect_ms(&QUERY_DURATION_MS, DB_OPERATION, op, project, elapsed);
}

/// [9.1] A `wamn.postgres` span over one guest DB call, enriched host-side with
/// the executing component's tenant/project (the same claim maps that inject
/// `app.env_key`; the guest cannot spoof them). The name and the `db.*` fields are
/// this surface's own, while the `wamn.*` identity block is [`effect_span`]'s, shared with
/// every other effect surface.
///
/// The run and the wiring position come from the two claim registries the
/// router driver binds before the pooled instance runs: `set_current_run` for
/// the run, `bind_invocation` for the node (`wamn-0h0g.7.9`). Both are
/// host-attested, so this span says which run and which node raised the call
/// without walking its `wamn.component.invoke` parent (`wamn-0h0g.24.14`).
///
/// A call outside a node walk — a platform read, or a pooled instance between
/// invocations — holds neither, and records the keys empty. `wamn.requirement`
/// is empty on every postgres span: it names the connection requirement an
/// HTTP effect was admitted under, and a DB call is admitted by its statement
/// set instead, so this surface holds no such claim.
fn db_span(plugin: &WamnPostgres, component_id: &str, op: &'static str) -> tracing::Span {
    let project = plugin.project_for(component_id);
    db_span_for_project(plugin, component_id, &project, op)
}

pub(super) fn db_span_for_project(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &str,
    op: &'static str,
) -> tracing::Span {
    let env_key = plugin.env_key_for(component_id).unwrap_or_default();
    let run = plugin.current_run_for(component_id);
    let span = effect_span!(
        "wamn.postgres",
        EffectIdentity {
            env_key: &env_key,
            project,
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
            .map(crate::plugins::connection_http::ConnectionInvocation::effect_wiring),
    );
    span
}

async fn begin_transaction(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &str,
) -> Result<PgTransaction, PgError> {
    let env_key = plugin.require_env_key(component_id)?;
    let schema = plugin.schema_for(component_id);
    let runner = plugin.runner_for(component_id);
    let role = plugin.role_for(component_id);
    let user_id = plugin.user_id_for(component_id);
    let operation = plugin.operation_for(component_id);
    let application = plugin.application_for(component_id);
    let run = plugin.current_run_for(component_id);
    let (connection, pp, authority) = plugin
        .checkout_workload(component_id, project, &env_key)
        .await?;
    let connection = StatementConnectionGuard::new(connection, Arc::clone(&plugin.destroyed));
    plugin
        .begin_with_claims(
            connection.connection(),
            authority,
            plugin.active_read(component_id),
            &env_key,
            schema.as_deref(),
            runner.as_deref(),
            role.as_deref(),
            user_id.as_deref(),
            operation.as_deref(),
            application.as_deref(),
            run.as_ref(),
            pp.statement_timeout_ms,
        )
        .await?;
    Ok(PgTransaction {
        state: Arc::new(std::sync::Mutex::new(TxnState {
            conn: Some(connection.into_connection()),
            finished: false,
        })),
        destroyed: plugin.destroyed.clone(),
        cursor_seq: 0,
        row_limit: pp.row_limit,
        lent: false,
    })
}

pub(super) async fn begin_statement_transaction(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &str,
) -> Result<PgTransaction, PgError> {
    let (transaction, mut claims) =
        prepare_statement_transaction(plugin, component_id, project).await?;
    let connection = StatementConnectionGuard::new(
        take_conn(&transaction.state)?,
        Arc::clone(&transaction.destroyed),
    );
    if let Some(run) = plugin.current_run_for(component_id) {
        claims.causation = Some(
            super::claims::transactions::prepare_causation(connection.connection(), &run)
                .await
                .map_err(|error| map_pg_error(&error))?,
        );
    }
    claims.bind(connection.connection()).await?;
    connection.restore(&transaction.state);
    Ok(transaction)
}

pub(super) async fn prepare_statement_transaction(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &str,
) -> Result<(PgTransaction, super::claims::transactions::BoundClaims), PgError> {
    let env_key = plugin.require_env_key(component_id)?;
    let schema = plugin.schema_for(component_id);
    let runner = plugin.runner_for(component_id);
    let role = plugin.role_for(component_id);
    let user_id = plugin.user_id_for(component_id);
    let operation = plugin.operation_for(component_id);
    let application = plugin.application_for(component_id);

    let (connection, policy, authority) = plugin
        .checkout_workload(component_id, project, &env_key)
        .await?;
    let connection = StatementConnectionGuard::new(connection, Arc::clone(&plugin.destroyed));
    let claims = plugin
        .prepare_claims(
            connection.connection(),
            authority,
            plugin.active_read(component_id),
            &env_key,
            schema.as_deref(),
            runner.as_deref(),
            role.as_deref(),
            user_id.as_deref(),
            operation.as_deref(),
            application.as_deref(),
            None,
            policy.statement_timeout_ms,
        )
        .await?;
    Ok((
        PgTransaction {
            state: Arc::new(std::sync::Mutex::new(TxnState {
                conn: Some(connection.into_connection()),
                finished: false,
            })),
            destroyed: Arc::clone(&plugin.destroyed),
            cursor_seq: 0,
            row_limit: policy.row_limit,
            lent: false,
        },
        claims,
    ))
}

impl client::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> client::HostWithStore<T> for SharedCtx {
    async fn query(
        accessor: &Accessor<T, Self>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let (plugin, component_id, trace) = accessor.with(|mut access| {
            let mut ctx = access.get();
            let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                trace,
            ))
        })?;
        trace
            .run(async move {
                let span = db_span(&plugin, &component_id, "query");
                let project = plugin.project_for(&component_id);
                let t0 = std::time::Instant::now();
                let result = plugin
                    .one_shot(&component_id, &sql, &params, true)
                    .instrument(span)
                    .await;
                record_query_ms("query", &project, t0.elapsed());
                Ok(match result {
                    Ok(OneShotResult::Rows(rs)) => Ok(rs),
                    Ok(OneShotResult::Count(_)) => unreachable!("one_shot(want_rows) returns rows"),
                    Err(e) => Err(e),
                })
            })
            .await
    }

    async fn execute(
        accessor: &Accessor<T, Self>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let (plugin, component_id, trace) = accessor.with(|mut access| {
            let mut ctx = access.get();
            let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                trace,
            ))
        })?;
        trace
            .run(async move {
                let span = db_span(&plugin, &component_id, "execute");
                let project = plugin.project_for(&component_id);
                let t0 = std::time::Instant::now();
                let result = plugin
                    .one_shot(&component_id, &sql, &params, false)
                    .instrument(span)
                    .await;
                record_query_ms("execute", &project, t0.elapsed());
                Ok(match result {
                    Ok(OneShotResult::Count(n)) => Ok(n),
                    Ok(OneShotResult::Rows(_)) => {
                        unreachable!("one_shot(!want_rows) returns count")
                    }
                    Err(e) => Err(e),
                })
            })
            .await
    }

    async fn begin(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgTransaction>, PgError>> {
        let (plugin, component_id, trace) = accessor.with(|mut access| {
            let mut ctx = access.get();
            let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                trace,
            ))
        })?;
        trace
            .run(async move {
                let project = plugin.project_for(&component_id);
                let span = db_span(&plugin, &component_id, "begin");
                let t0 = std::time::Instant::now();

                // Both round trips — the pool checkout and the claim-stamping BEGIN —
                // are the one effect, so one span covers both.
                let opened = begin_transaction(&plugin, &component_id, &project)
                    .instrument(span)
                    .await;
                record_query_ms("begin", &project, t0.elapsed());
                let txn = match opened {
                    Ok(opened) => opened,
                    Err(e) => return Ok(Err(e)),
                };
                accessor.with(|mut access| access.get().table.push(txn).map(Ok).map_err(Into::into))
            })
            .await
    }
}

#[cfg(feature = "wasm_component_model_implements")]
impl bindings::named_imports::wamn::postgres::client::Host for ActiveCtx<'_> {}

#[cfg(feature = "wasm_component_model_implements")]
impl<T: 'static + Send> bindings::named_imports::wamn::postgres::client::HostWithStore<T>
    for SharedCtx
{
    async fn query(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let (plugin, component_id, trace) = accessor.with(|mut access| {
            let mut ctx = access.get();
            let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                trace,
            ))
        })?;
        trace
            .run(async move {
                let project = id.project().to_string();
                let span = db_span_for_project(&plugin, &component_id, &project, "query");
                let t0 = std::time::Instant::now();
                let result = plugin
                    .one_shot_for_project(&component_id, &project, &sql, &params, true)
                    .instrument(span)
                    .await;
                record_query_ms("query", &project, t0.elapsed());
                Ok(match result {
                    Ok(OneShotResult::Rows(rs)) => Ok(rs),
                    Ok(OneShotResult::Count(_)) => unreachable!("one_shot(want_rows) returns rows"),
                    Err(e) => Err(e),
                })
            })
            .await
    }

    async fn execute(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let (plugin, component_id, trace) = accessor.with(|mut access| {
            let mut ctx = access.get();
            let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                trace,
            ))
        })?;
        trace
            .run(async move {
                let project = id.project().to_string();
                let span = db_span_for_project(&plugin, &component_id, &project, "execute");
                let t0 = std::time::Instant::now();
                let result = plugin
                    .one_shot_for_project(&component_id, &project, &sql, &params, false)
                    .instrument(span)
                    .await;
                record_query_ms("execute", &project, t0.elapsed());
                Ok(match result {
                    Ok(OneShotResult::Count(n)) => Ok(n),
                    Ok(OneShotResult::Rows(_)) => {
                        unreachable!("one_shot(!want_rows) returns count")
                    }
                    Err(e) => Err(e),
                })
            })
            .await
    }

    async fn begin(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgTransaction>, PgError>> {
        let (plugin, component_id, trace) = accessor.with(|mut access| {
            let mut ctx = access.get();
            let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                trace,
            ))
        })?;
        trace
            .run(async move {
                let project = id.project().to_string();
                let span = db_span_for_project(&plugin, &component_id, &project, "begin");
                let t0 = std::time::Instant::now();
                let opened = begin_transaction(&plugin, &component_id, &project)
                    .instrument(span)
                    .await;
                record_query_ms("begin", &project, t0.elapsed());
                match opened {
                    Ok(txn) => accessor.with(|mut access| {
                        access.get().table.push(txn).map(Ok).map_err(Into::into)
                    }),
                    Err(e) => Ok(Err(e)),
                }
            })
            .await
    }
}

async fn txn_query<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    project: &str,
    rep: Resource<PgTransaction>,
    sql: String,
    params: Vec<SqlValue>,
) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
    let (plugin, component_id, trace, state, destroyed, row_limit) =
        accessor.with(|mut access| {
            let mut ctx = access.get();
            let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
            let txn = ctx.table.get(&rep)?;
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                trace,
                Arc::clone(&txn.state),
                Arc::clone(&txn.destroyed),
                txn.row_limit,
            ))
        })?;
    trace
        .run(async move {
            let span = db_span_for_project(&plugin, &component_id, project, "txn.query");
            let t0 = std::time::Instant::now();
            let out = with_txn_conn(&state, &destroyed, |connection| async move {
                // `run_query` maps errors already, so nothing reaching
                // `with_txn_conn` is a raw error it would judge fatal.
                let queried = run_query(connection.connection(), &sql, &params, row_limit).await;
                (connection, Ok(queried))
            })
            .instrument(span)
            .await
            .and_then(|r| r);
            record_query_ms("txn.query", project, t0.elapsed());
            Ok(out)
        })
        .await
}

async fn txn_execute<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    project: &str,
    rep: Resource<PgTransaction>,
    sql: String,
    params: Vec<SqlValue>,
) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
    let (plugin, component_id, trace, state, destroyed) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        let txn = ctx.table.get(&rep)?;
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
            Arc::clone(&txn.state),
            Arc::clone(&txn.destroyed),
        ))
    })?;
    trace
        .run(async move {
            let span = db_span_for_project(&plugin, &component_id, project, "txn.execute");
            let t0 = std::time::Instant::now();
            let out = with_txn_conn(&state, &destroyed, |connection| async move {
                // `run_execute` maps errors already, so nothing reaching
                // `with_txn_conn` is a raw error it would judge fatal.
                let executed = run_execute(connection.connection(), &sql, &params).await;
                (connection, Ok(executed))
            })
            .instrument(span)
            .await
            .and_then(|r| r);
            record_query_ms("txn.execute", project, t0.elapsed());
            Ok(out)
        })
        .await
}

async fn txn_open_cursor<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    project: &str,
    rep: Resource<PgTransaction>,
    sql: String,
    params: Vec<SqlValue>,
) -> wash_runtime::wasmtime::Result<Result<Resource<PgCursor>, PgError>> {
    let (plugin, component_id, trace, state, destroyed, name) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        let plugin = plugin_of(&ctx)?;
        let component_id = ctx.component_id.to_string();

        let txn = ctx.table.get_mut(&rep)?;
        txn.cursor_seq += 1;
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin,
            component_id,
            trace,
            Arc::clone(&txn.state),
            Arc::clone(&txn.destroyed),
            format!("wamn_c{}", txn.cursor_seq),
        ))
    })?;
    trace
        .run(async move {
            // A cursor over `SELECT set_config('app.env_key', …)` would execute the
            // override on fetch; guard the same surface as query/execute (wamn-cjv.2).
            if let Err(e) = reject_claim_mutation(&sql) {
                return Ok(Err(e));
            }
            let span = db_span_for_project(&plugin, &component_id, project, "txn.open_cursor");
            let declare = format!("DECLARE {name} CURSOR FOR {sql}");
            let t0 = std::time::Instant::now();
            let result = with_txn_conn(&state, &destroyed, |connection| async move {
                let r = async {
                    let stmt = connection.connection().prepare(&declare).await?;
                    connection
                        .connection()
                        .execute_raw(&stmt, params.iter().map(PgParam))
                        .await
                }
                .await;
                (connection, r)
            })
            .instrument(span)
            .await;
            record_query_ms("txn.open_cursor", project, t0.elapsed());
            Ok(match result {
                Ok(_) => accessor.with(|mut access| {
                    access
                        .get()
                        .table
                        .push(PgCursor {
                            state,
                            destroyed,
                            name,
                        })
                        .map(Ok)
                        .map_err(wash_runtime::wasmtime::Error::from)
                })?,
                Err(e) => Err(e),
            })
        })
        .await
}

async fn txn_finish<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    project: &str,
    rep: Resource<PgTransaction>,
    verb: &'static str,
) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
    let (plugin, component_id, trace, state, destroyed) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        let txn = ctx.table.get(&rep)?;
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
            Arc::clone(&txn.state),
            Arc::clone(&txn.destroyed),
        ))
    })?;
    trace
        .run(async move {
            let op = match verb {
                "COMMIT" => "txn.commit",
                "ROLLBACK" => "txn.rollback",
                _ => unreachable!("transaction finish verb is fixed"),
            };
            let span = db_span_for_project(&plugin, &component_id, project, op);
            let t0 = std::time::Instant::now();
            let result = finish_txn(&state, &destroyed, verb).instrument(span).await;
            record_query_ms(op, project, t0.elapsed());
            Ok(result)
        })
        .await
}

fn txn_drop(
    ctx: &mut ActiveCtx<'_>,
    rep: Resource<PgTransaction>,
) -> wash_runtime::wasmtime::Result<()> {
    // Resource destruction cannot suspend. Drop destroys an unfinished
    // connection, so PostgreSQL rolls back instead of returning it to the pool.
    drop(ctx.table.delete(rep)?);
    Ok(())
}

async fn cursor_fetch<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    project: &str,
    rep: Resource<PgCursor>,
    max_rows: u32,
) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
    let (plugin, component_id, trace, state, destroyed, name) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        let cursor = ctx.table.get(&rep)?;
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
            Arc::clone(&cursor.state),
            Arc::clone(&cursor.destroyed),
            cursor.name.clone(),
        ))
    })?;
    trace
        .run(async move {
            let span = db_span_for_project(&plugin, &component_id, project, "cursor.fetch");
            let t0 = std::time::Instant::now();
            let fetched = with_txn_conn(&state, &destroyed, |connection| async move {
                let r = async {
                    let sql = format!("FETCH FORWARD {max_rows} FROM {name}");
                    let stmt = connection.connection().prepare(&sql).await?;
                    let columns = columns_of(&stmt);
                    let rows = connection.connection().query(&stmt, &[]).await?;
                    Ok::<_, tokio_postgres::Error>((columns, rows))
                }
                .await;
                (connection, r)
            })
            .instrument(span)
            .await;
            record_query_ms("cursor.fetch", project, t0.elapsed());
            Ok(fetched.and_then(|(columns, rows)| {
                let rows = rows.iter().map(decode_row).collect::<Result<Vec<_>, _>>()?;
                Ok(RowSet {
                    columns,
                    rows,
                    wal_position: None,
                })
            }))
        })
        .await
}

fn cursor_drop(
    ctx: &mut ActiveCtx<'_>,
    rep: Resource<PgCursor>,
) -> wash_runtime::wasmtime::Result<()> {
    // Server-side cursors die with their transaction; nothing to release.
    ctx.table.delete(rep)?;
    Ok(())
}

#[cfg(feature = "wasm_component_model_implements")]
impl<T: 'static + Send> bindings::named_imports::wamn::postgres::client::HostTransactionWithStore<T>
    for SharedCtx
{
    async fn query(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        txn_query(accessor, id.project(), rep, sql, params).await
    }

    async fn execute(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        txn_execute(accessor, id.project(), rep, sql, params).await
    }

    async fn open_cursor(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgCursor>, PgError>> {
        txn_open_cursor(accessor, id.project(), rep, sql, params).await
    }

    async fn commit(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        txn_finish(accessor, id.project(), rep, "COMMIT").await
    }

    async fn rollback(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        txn_finish(accessor, id.project(), rep, "ROLLBACK").await
    }
}

#[cfg(feature = "wasm_component_model_implements")]
impl<T: 'static + Send> bindings::named_imports::wamn::postgres::client::HostCursorWithStore<T>
    for SharedCtx
{
    async fn fetch(
        accessor: &Accessor<T, Self>,
        id: super::NamedProject,
        rep: Resource<PgCursor>,
        max_rows: u32,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        cursor_fetch(accessor, id.project(), rep, max_rows).await
    }
}

impl<T: 'static + Send> client::HostTransactionWithStore<T> for SharedCtx {
    async fn query(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let project = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>(
                plugin_of(&ctx)?.project_for(ctx.component_id.as_ref()),
            )
        })?;
        txn_query(accessor, &project, rep, sql, params).await
    }

    async fn execute(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let project = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>(
                plugin_of(&ctx)?.project_for(ctx.component_id.as_ref()),
            )
        })?;
        txn_execute(accessor, &project, rep, sql, params).await
    }

    async fn open_cursor(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgCursor>, PgError>> {
        let project = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>(
                plugin_of(&ctx)?.project_for(ctx.component_id.as_ref()),
            )
        })?;
        txn_open_cursor(accessor, &project, rep, sql, params).await
    }

    async fn commit(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        let project = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>(
                plugin_of(&ctx)?.project_for(ctx.component_id.as_ref()),
            )
        })?;
        txn_finish(accessor, &project, rep, "COMMIT").await
    }

    async fn rollback(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        let project = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>(
                plugin_of(&ctx)?.project_for(ctx.component_id.as_ref()),
            )
        })?;
        txn_finish(accessor, &project, rep, "ROLLBACK").await
    }
}

/// COMMIT or ROLLBACK, then repool the connection and mark the transaction
/// finished. On failure the connection is destroyed.
async fn finish_txn(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    verb: &str,
) -> Result<(), PgError> {
    let connection = StatementConnectionGuard::new(take_conn(state)?, Arc::clone(destroyed));
    match super::claims::transactions::finish_transaction(connection.connection(), verb).await {
        Ok(()) => {
            if let Ok(mut st) = state.lock() {
                st.finished = true;
            }
            connection.repool();
            Ok(())
        }
        Err(e) => {
            if let Ok(mut st) = state.lock() {
                st.finished = true;
            }
            drop(connection);
            Err(map_pg_error(&e))
        }
    }
}

impl<T: 'static + Send> client::HostCursorWithStore<T> for SharedCtx {
    async fn fetch(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgCursor>,
        max_rows: u32,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let project = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>(
                plugin_of(&ctx)?.project_for(ctx.component_id.as_ref()),
            )
        })?;
        cursor_fetch(accessor, &project, rep, max_rows).await
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated transaction destructor is async but only releases local state"
)]
impl client::HostTransaction for ActiveCtx<'_> {
    async fn drop(&mut self, rep: Resource<PgTransaction>) -> wash_runtime::wasmtime::Result<()> {
        txn_drop(self, rep)
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated cursor destructor is async but only releases local state"
)]
impl client::HostCursor for ActiveCtx<'_> {
    async fn drop(&mut self, rep: Resource<PgCursor>) -> wash_runtime::wasmtime::Result<()> {
        cursor_drop(self, rep)
    }
}

#[cfg(feature = "wasm_component_model_implements")]
#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated transaction destructor is async but only releases local state"
)]
impl bindings::named_imports::wamn::postgres::client::HostTransaction for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        _id: super::NamedProject,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<()> {
        txn_drop(self, rep)
    }
}

#[cfg(feature = "wasm_component_model_implements")]
#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated cursor destructor is async but only releases local state"
)]
impl bindings::named_imports::wamn::postgres::client::HostCursor for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        _id: super::NamedProject,
        rep: Resource<PgCursor>,
    ) -> wash_runtime::wasmtime::Result<()> {
        cursor_drop(self, rep)
    }
}

impl statement_wit::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> statement_wit::HostWithStore<T> for SharedCtx {
    async fn run(
        accessor: &Accessor<T, Self>,
        statement_path: String,
        binds: Vec<SqlValue>,
        wal: Option<statement_wit::WantWalPosition>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
        statement_run(accessor, None, statement_path, binds, wal).await
    }

    async fn begin(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, StatementError>>
    {
        statement_begin(accessor, None).await
    }

    async fn run_stream(
        accessor: &Accessor<T, Self>,
        statement_path: String,
        binds: Vec<SqlValue>,
        wal: Option<statement_wit::WantWalPosition>,
    ) -> wash_runtime::wasmtime::Result<
        Result<
            (
                Vec<super::Column>,
                Option<String>,
                wash_runtime::wasmtime::component::StreamReader<Vec<SqlValue>>,
                wash_runtime::wasmtime::component::FutureReader<Result<(), StatementError>>,
            ),
            StatementError,
        >,
    > {
        super::run_stream::run_stream(accessor, None, statement_path, binds, wal).await
    }

    async fn operation_transaction(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, StatementError>>
    {
        lend_operation_transaction(accessor, None).await
    }

    async fn next_number(
        accessor: &Accessor<T, Self>,
        name: String,
        keys: Vec<(String, Option<String>)>,
    ) -> wash_runtime::wasmtime::Result<Result<String, StatementError>> {
        statement_number(accessor, name, keys, NumberCall::Next).await
    }

    async fn peek_number(
        accessor: &Accessor<T, Self>,
        name: String,
        keys: Vec<(String, Option<String>)>,
    ) -> wash_runtime::wasmtime::Result<Result<String, StatementError>> {
        statement_number(accessor, name, keys, NumberCall::Peek).await
    }
}

impl<T: 'static + Send> statement_wit::HostTransactionWithStore<T> for SharedCtx {
    async fn run(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
        statement_path: String,
        binds: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
        statement_txn_run(accessor, rep, statement_path, binds).await
    }

    async fn commit(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
        statement_txn_finish(accessor, rep, "COMMIT").await
    }

    async fn rollback(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
        statement_txn_finish(accessor, rep, "ROLLBACK").await
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated transaction destructor is async but only releases local state"
)]
impl statement_wit::HostTransaction for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<()> {
        // An unfinished transaction destroys its connection without suspending.
        drop(self.table.delete(rep)?);
        Ok(())
    }
}

/// Run one admitted statement on its own connection, resolved in the set of
/// the `statements` import named `instance`.
async fn statement_run<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    instance: Option<&str>,
    statement_path: String,
    binds: Vec<SqlValue>,
    wal: Option<statement_wit::WantWalPosition>,
) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
    let (plugin, component_id, trace) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
        ))
    })?;
    trace
        .run(async move {
            if let Err(refused) = plugin.refuse_in_host_transaction(&component_id, "run") {
                return Ok(Err(refused));
            }
            let active = plugin.active_statement_set(&component_id, instance);
            let read = plugin.active_read(&component_id);
            let statement =
                match resolve_statement(active.as_deref(), read, &statement_path, &binds) {
                    Ok(statement) => statement,
                    Err(error) => return Ok(Err(error)),
                };
            let project = plugin.project_for(&component_id);
            let span = db_span_for_project(&plugin, &component_id, &project, "statement.run");
            let started = std::time::Instant::now();
            let result = plugin
                .one_shot_statement(
                    &component_id,
                    &statement_path,
                    &statement,
                    &binds,
                    wal.is_some(),
                )
                .instrument(span)
                .await;
            record_query_ms("statement.run", &project, started.elapsed());
            Ok(result)
        })
        .await
}

/// Begin a guest transaction that runs the set of the `statements` import named `instance`.
async fn statement_begin<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    instance: Option<&str>,
) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, StatementError>> {
    let (plugin, component_id, trace) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
        ))
    })?;
    trace
        .run(async move {
            if let Err(refused) = plugin.refuse_in_host_transaction(&component_id, "begin") {
                return Ok(Err(refused));
            }
            let project = plugin.project_for(&component_id);
            let statements = plugin.active_statement_set(&component_id, instance);
            let span = db_span_for_project(&plugin, &component_id, &project, "statement.begin");
            let started = std::time::Instant::now();
            let opened = begin_statement_transaction(&plugin, &component_id, &project)
                .instrument(span)
                .await;
            record_query_ms("statement.begin", &project, started.elapsed());
            match opened {
                Ok(transaction) => accessor.with(|mut access| {
                    access
                        .get()
                        .table
                        .push(PgStatementTransaction {
                            transaction,
                            owner_scope: component_id.clone(),
                            statements,
                        })
                        .map(Ok)
                        .map_err(Into::into)
                }),
                Err(error) => Ok(Err(StatementError::Postgres(error))),
            }
        })
        .await
}

/// Lend the operation's host transaction to the `statements` import named
/// `instance`, with that import's statement set.
async fn lend_operation_transaction<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    instance: Option<&str>,
) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, StatementError>> {
    let (transaction, statements, component_id) = accessor.with(|mut access| {
        let ctx = access.get();
        let plugin = plugin_of(&ctx)?;
        let component_id = ctx.component_id.to_string();
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin.operation_transaction_for(&component_id),
            plugin.active_statement_set(&component_id, instance),
            component_id,
        ))
    })?;
    let Some(transaction) = transaction else {
        return Ok(Err(StatementError::Postgres(PgError::PermissionDenied)));
    };
    let transaction = match transaction.lend().await {
        Ok(transaction) => transaction,
        Err(error) => return Ok(Err(StatementError::Postgres(error))),
    };
    accessor.with(|mut access| {
        access
            .get()
            .table
            .push(PgStatementTransaction {
                transaction,
                owner_scope: component_id,
                statements,
            })
            .map(Ok)
            .map_err(Into::into)
    })
}

/// Run one admitted statement on a statement transaction, resolved in the
/// set the transaction holds.
async fn statement_txn_run<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    rep: Resource<PgStatementTransaction>,
    statement_path: String,
    binds: Vec<SqlValue>,
) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
    let (plugin, component_id, trace, state, destroyed, row_limit, statements) =
        accessor.with(|mut access| {
            let mut ctx = access.get();
            let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
            let transaction = ctx.table.get(&rep)?;
            wash_runtime::wasmtime::ensure!(
                transaction.owner_scope == ctx.component_id.as_ref(),
                "statement transaction belongs to a different invocation"
            );
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                trace,
                Arc::clone(&transaction.transaction.state),
                Arc::clone(&transaction.transaction.destroyed),
                transaction.transaction.row_limit,
                transaction.statements.clone(),
            ))
        })?;
    trace
        .run(async move {
            let project = plugin.project_for(&component_id);
            let statement = match admit_transaction_statement(
                &plugin,
                &component_id,
                statements.as_deref(),
                &statement_path,
                &binds,
            ) {
                Ok(statement) => statement,
                Err(error) => return Ok(Err(error)),
            };
            let connection = match take_conn(&state) {
                Ok(connection) => StatementConnectionGuard::new(connection, Arc::clone(&destroyed)),
                Err(error) => return Ok(Err(StatementError::Postgres(error))),
            };
            let span = db_span_for_project(&plugin, &component_id, &project, "statement.txn.run");
            let started = std::time::Instant::now();
            let result = run_verified_query(
                connection.connection(),
                &statement_path,
                &statement,
                &binds,
                row_limit,
            )
            .instrument(span)
            .await;
            record_query_ms("statement.txn.run", &project, started.elapsed());

            if statement_run_disposition(&result) == StatementRunDisposition::Destroy {
                if let Ok(mut transaction) = state.lock() {
                    transaction.finished = true;
                }
                drop(connection);
            } else {
                connection.restore(&state);
            }
            Ok(result)
        })
        .await
}

/// Resolve a statement for an explicit statement transaction.
///
/// A transactional statement with no executing principal is refused here,
/// before the transaction's connection runs it.
pub(super) fn admit_transaction_statement(
    plugin: &WamnPostgres,
    component_id: &str,
    statements: Option<&BoundStatementSet>,
    statement_path: &str,
    binds: &[SqlValue],
) -> Result<Arc<VerifiedStatement>, StatementError> {
    let statement = resolve_statement(
        statements,
        plugin.active_read(component_id),
        statement_path,
        binds,
    )?;
    refuse_unattributed_statement(&statement, plugin.user_id_for(component_id).as_deref())
        .map_err(StatementError::Postgres)?;
    Ok(statement)
}

async fn statement_txn_finish<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    rep: Resource<PgStatementTransaction>,
    verb: &'static str,
) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
    let (plugin, component_id, trace, state, destroyed, lent) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        let transaction = ctx.table.get(&rep)?;
        wash_runtime::wasmtime::ensure!(
            transaction.owner_scope == ctx.component_id.as_ref(),
            "statement transaction belongs to a different invocation"
        );
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
            Arc::clone(&transaction.transaction.state),
            Arc::clone(&transaction.transaction.destroyed),
            transaction.transaction.lent,
        ))
    })?;
    // The host commits or rolls back the transaction it owns.
    if lent {
        return Ok(Err(StatementError::Postgres(PgError::QueryError((
            super::operation_transaction::FINISH_REFUSED.to_owned(),
            "the host finishes the operation transaction".to_owned(),
        )))));
    }
    trace
        .run(async move {
            let project = plugin.project_for(&component_id);
            let operation = match verb {
                "COMMIT" => "statement.txn.commit",
                "ROLLBACK" => "statement.txn.rollback",
                _ => unreachable!("transaction finish verb is fixed"),
            };
            let span = db_span_for_project(&plugin, &component_id, &project, operation);
            let started = std::time::Instant::now();
            let result = finish_statement_txn(&state, &destroyed, verb)
                .instrument(span)
                .await
                .map_err(StatementError::Postgres);
            record_query_ms(operation, &project, started.elapsed());
            Ok(result)
        })
        .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatementRunDisposition {
    Restore,
    Destroy,
}

fn statement_run_disposition<T>(result: &Result<T, StatementError>) -> StatementRunDisposition {
    if result.is_ok() {
        StatementRunDisposition::Restore
    } else {
        StatementRunDisposition::Destroy
    }
}

pub(super) async fn finish_statement_txn(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    verb: &str,
) -> Result<(), PgError> {
    let connection = StatementConnectionGuard::new(take_conn(state)?, Arc::clone(destroyed));
    match super::claims::transactions::finish_transaction(connection.connection(), verb).await {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Open the real transaction resource for cross-crate teardown tests.
/// This test-only entry bypasses guest WIT lowering, not transaction ownership.
#[cfg(feature = "test-util")]
pub async fn retained_transaction_for_test(
    plugin: &WamnPostgres,
    scope: &str,
    project: &str,
    sql: &str,
) -> anyhow::Result<(PgTransaction, i32)> {
    let transaction = begin_transaction(plugin, scope, project)
        .await
        .map_err(|error| anyhow::anyhow!("open test transaction: {error}"))?;
    let connection = take_conn(&transaction.state)
        .map_err(|error| anyhow::anyhow!("take test transaction: {error}"))?;
    let connection = StatementConnectionGuard::new(connection, Arc::clone(&transaction.destroyed));
    connection.connection().batch_execute(sql).await?;
    let pid = connection
        .connection()
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    connection.restore(&transaction.state);
    Ok((transaction, pid))
}

#[cfg(feature = "wasm_component_model_implements")]
impl bindings::named_imports::wamn::postgres::statements::Host for ActiveCtx<'_> {}

/// Each named import resolves statements in its own member's set. Methods on
/// a transaction need no name: the transaction holds the set it was lent with.
#[cfg(feature = "wasm_component_model_implements")]
impl<T: 'static + Send> bindings::named_imports::wamn::postgres::statements::HostWithStore<T>
    for SharedCtx
{
    async fn run(
        accessor: &Accessor<T, Self>,
        id: super::NamedStatements,
        statement_path: String,
        binds: Vec<SqlValue>,
        wal: Option<statement_wit::WantWalPosition>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
        statement_run(accessor, Some(id.instance()), statement_path, binds, wal).await
    }

    async fn run_stream(
        accessor: &Accessor<T, Self>,
        id: super::NamedStatements,
        statement_path: String,
        binds: Vec<SqlValue>,
        wal: Option<statement_wit::WantWalPosition>,
    ) -> wash_runtime::wasmtime::Result<
        Result<
            (
                Vec<super::Column>,
                Option<String>,
                wash_runtime::wasmtime::component::StreamReader<Vec<SqlValue>>,
                wash_runtime::wasmtime::component::FutureReader<Result<(), StatementError>>,
            ),
            StatementError,
        >,
    > {
        super::run_stream::run_stream(accessor, Some(id.instance()), statement_path, binds, wal)
            .await
    }

    async fn begin(
        accessor: &Accessor<T, Self>,
        id: super::NamedStatements,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, StatementError>>
    {
        statement_begin(accessor, Some(id.instance())).await
    }

    async fn operation_transaction(
        accessor: &Accessor<T, Self>,
        id: super::NamedStatements,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, StatementError>>
    {
        lend_operation_transaction(accessor, Some(id.instance())).await
    }

    async fn next_number(
        accessor: &Accessor<T, Self>,
        _id: super::NamedStatements,
        name: String,
        keys: Vec<(String, Option<String>)>,
    ) -> wash_runtime::wasmtime::Result<Result<String, StatementError>> {
        statement_number(accessor, name, keys, NumberCall::Next).await
    }

    async fn peek_number(
        accessor: &Accessor<T, Self>,
        _id: super::NamedStatements,
        name: String,
        keys: Vec<(String, Option<String>)>,
    ) -> wash_runtime::wasmtime::Result<Result<String, StatementError>> {
        statement_number(accessor, name, keys, NumberCall::Peek).await
    }
}

#[cfg(feature = "wasm_component_model_implements")]
impl<T: 'static + Send>
    bindings::named_imports::wamn::postgres::statements::HostTransactionWithStore<T> for SharedCtx
{
    async fn run(
        accessor: &Accessor<T, Self>,
        _id: super::NamedStatements,
        rep: Resource<PgStatementTransaction>,
        statement_path: String,
        binds: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
        statement_txn_run(accessor, rep, statement_path, binds).await
    }

    async fn commit(
        accessor: &Accessor<T, Self>,
        _id: super::NamedStatements,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
        statement_txn_finish(accessor, rep, "COMMIT").await
    }

    async fn rollback(
        accessor: &Accessor<T, Self>,
        _id: super::NamedStatements,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
        statement_txn_finish(accessor, rep, "ROLLBACK").await
    }
}

#[cfg(feature = "wasm_component_model_implements")]
#[expect(
    clippy::unused_async_trait_impl,
    reason = "the generated transaction destructor is async but only releases local state"
)]
impl bindings::named_imports::wamn::postgres::statements::HostTransaction for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        _id: super::NamedStatements,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<()> {
        drop(self.table.delete(rep)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::connection_http::{
        ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin,
    };
    use crate::plugins::effect_span::span_tests::{SpanHarness, expected_attributes};
    use crate::plugins::wamn_postgres::{
        ContractMismatch, ContractPart, ValueShape, WamnPostgresConfig,
    };
    use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
    use tokio_postgres::NoTls;
    use wamn_event_wire::Causation;

    const COMPONENT_ID: &str = "component-store-7";
    const COMPONENT_DIGEST: &str = "sha256:aaaa";

    /// A plugin that opens no connection, so a span test needs no database.
    fn offline_plugin() -> WamnPostgres {
        let plugin = WamnPostgres::new(WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1,
            statement_timeout_ms: 1,
            row_limit: 1,
        })
        .expect("an offline postgres plugin opens no connection");
        plugin
            .set_env_key(COMPONENT_ID, "tenant-a")
            .expect("the tenant claim is valid");
        plugin
    }

    #[tokio::test]
    async fn cancelled_client_transaction_destroys_its_connection_and_completion_repools() {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_test_postgres::database();
        let config: tokio_postgres::Config = database.url().parse().expect("test database URL");
        let manager = Manager::from_config(
            config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(1)
            .runtime(deadpool_postgres::Runtime::Tokio1)
            .build()
            .expect("test pool");
        let connection = pool.get().await.expect("initial pooled connection");
        connection
            .batch_execute("BEGIN")
            .await
            .expect("begin cancellation transaction");
        let state = Arc::new(std::sync::Mutex::new(TxnState {
            conn: Some(connection),
            finished: false,
        }));
        let destroyed = Arc::new(AtomicU64::new(0));
        let operation_started = Arc::new(tokio::sync::Notify::new());
        let task_state = Arc::clone(&state);
        let task_destroyed = Arc::clone(&destroyed);
        let task_started = Arc::clone(&operation_started);
        let task = tokio::spawn(async move {
            with_txn_conn(&task_state, &task_destroyed, |connection| async move {
                task_started.notify_one();
                let result = connection
                    .connection()
                    .batch_execute("SELECT pg_sleep(30) /* wamn-cancelled-client-transaction */")
                    .await;
                (connection, result)
            })
            .await
        });
        operation_started.notified().await;
        task.abort();
        assert!(
            task.await
                .expect_err("cancelled transaction task")
                .is_cancelled()
        );
        assert_eq!(destroyed.load(std::sync::atomic::Ordering::Relaxed), 1);

        let connection = tokio::time::timeout(std::time::Duration::from_secs(2), pool.get())
            .await
            .expect("destroyed connection releases pool capacity")
            .expect("pool creates a clean replacement");
        connection
            .batch_execute("BEGIN")
            .await
            .expect("begin completion control transaction");
        let state = Arc::new(std::sync::Mutex::new(TxnState {
            conn: Some(connection),
            finished: false,
        }));
        let value = with_txn_conn(&state, &destroyed, |connection| async move {
            let result = connection
                .connection()
                .query_one("SELECT 1::integer", &[])
                .await
                .map(|row| row.get::<_, i32>(0));
            (connection, result)
        })
        .await
        .expect("normal transaction operation");
        assert_eq!(value, 1);
        finish_txn(&state, &destroyed, "COMMIT")
            .await
            .expect("normal transaction completion");
        assert_eq!(destroyed.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(pool.status().available, 1);
        drop(pool.get().await.expect("completed connection was repooled"));
    }

    /// The invocation the router driver binds before the pooled instance runs.
    fn invocation() -> ConnectionInvocation {
        ConnectionInvocation {
            origin: ConnectionOrigin {
                package_id: "package_a".to_string(),
                component_digest: COMPONENT_DIGEST.to_string(),
                component: "orders".to_string(),
                interface_version: "1.0.0".to_string(),
                operation: "orders:notify/dispatch@1.0.0".to_string(),
            },
            entry: crate::plugins::connection_http::InvocationEntry::Wiring(
                crate::plugins::connection_http::WiringPosition {
                    package_id: "package_a".to_string(),
                    wiring_hash: format!("sha256:{}", "a".repeat(64)),
                    wiring_id: "orders".to_string(),
                    wiring_version: 3,
                    node_id: "record".to_string(),
                    occurrence: 2,
                },
            ),
            package_id: "package_a".to_string(),
            component_digest: COMPONENT_DIGEST.to_string(),
            component: "recorder".to_string(),
            operation: "orders:notify/dispatch@1.0.0".to_string(),
            closure: ConnectionExecutionClosure::Released,
            effects: None,
        }
    }

    /// A DB call inside a node walk names its run and its node on the span
    /// itself. A mutant that drops either registry read fails here.
    #[test]
    fn a_db_call_inside_a_node_walk_carries_its_run_and_node_identity() {
        let plugin = offline_plugin();
        plugin.set_current_run(
            COMPONENT_ID,
            Some(Causation {
                run: "run-42".to_string(),
                root: "root-1".to_string(),
                depth: 1,
            }),
        );
        plugin
            .bind_invocation(COMPONENT_ID, invocation())
            .expect("the fresh registry accepts its first invocation");

        let harness = SpanHarness::install("postgres-node-walk-span-test");
        drop(db_span_for_project(
            &plugin,
            COMPONENT_ID,
            "project-a",
            "query",
        ));

        assert_eq!(
            harness.attributes("wamn.postgres"),
            expected_attributes(&[
                ("db.system", "postgresql"),
                ("db.operation", "query"),
                ("wamn.env_key", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", COMPONENT_ID),
                ("wamn.package_id", "package_a"),
                ("wamn.wiring_id", "orders"),
                ("wamn.wiring_version", "3"),
                ("wamn.node_id", "record"),
                ("wamn.occurrence", "2"),
                ("wamn.component_digest", COMPONENT_DIGEST),
                ("wamn.component_name", "recorder"),
                ("wamn.operation", "orders:notify/dispatch@1.0.0"),
                ("wamn.run_id", "run-42"),
                // A DB call is admitted by its statement set, not by a named
                // connection requirement, so this surface holds no such claim.
                ("wamn.requirement", ""),
            ]),
        );
    }

    /// A call outside a node walk holds neither claim. The wiring keys are
    /// still emitted, empty, so "raised outside a walk" never reads as "the
    /// enrichment was dropped".
    #[test]
    fn a_db_call_outside_a_node_walk_records_the_wiring_keys_empty() {
        let plugin = offline_plugin();

        let harness = SpanHarness::install("postgres-no-walk-span-test");
        drop(db_span_for_project(
            &plugin,
            COMPONENT_ID,
            "project-a",
            "query",
        ));

        assert_eq!(
            harness.attributes("wamn.postgres"),
            expected_attributes(&[
                ("db.system", "postgresql"),
                ("db.operation", "query"),
                ("wamn.env_key", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", COMPONENT_ID),
                ("wamn.package_id", ""),
                ("wamn.wiring_id", ""),
                ("wamn.wiring_version", "0"),
                ("wamn.node_id", ""),
                ("wamn.occurrence", "0"),
                ("wamn.component_digest", ""),
                ("wamn.component_name", ""),
                ("wamn.operation", ""),
            ]),
        );
    }

    fn mismatch() -> StatementError {
        StatementError::StatementContractMismatch(ContractMismatch {
            statement: "sql/statement.sql".into(),
            part: ContractPart::Columns,
            expected: ValueShape {
                count: 1,
                types: vec!["text".into()],
            },
            observed: ValueShape {
                count: 1,
                types: vec!["null".into()],
            },
        })
    }

    #[test]
    fn explicit_statement_run_restores_only_after_success() {
        assert_eq!(
            statement_run_disposition(&Ok::<_, StatementError>(())),
            StatementRunDisposition::Restore
        );
        for error in [
            mismatch(),
            StatementError::Postgres(PgError::RowLimitExceeded(100)),
            StatementError::Postgres(PgError::QueryError((
                "WAMN1".into(),
                "result decode failed".into(),
            ))),
        ] {
            assert_eq!(
                statement_run_disposition(&Err::<(), _>(error)),
                StatementRunDisposition::Destroy
            );
        }
    }
}
