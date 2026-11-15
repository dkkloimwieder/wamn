//! SQL query results with bounded buffering and a closing result.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::TryStreamExt as _;
use tokio::sync::{mpsc, oneshot};
use tracing::Instrument as _;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::wasmtime::StoreContextMut;
use wash_runtime::wasmtime::component::{
    Accessor, Destination, FutureReader, Resource, StreamProducer, StreamReader, StreamResult,
    VecBuffer,
};

use super::resources::{
    PgStatementTransaction, SharedTxnState, StatementConnectionGuard, db_span_for_project,
    plugin_of, record_query_ms, restore_connection, take_conn,
};
use super::types::{PgParam, columns_of, decode_row, map_pg_error};
use super::{PgError, SqlValue};

type WasmResult<T> = wash_runtime::wasmtime::Result<T>;
pub(super) type QueryResult = WasmResult<
    Result<
        (
            Vec<String>,
            StreamReader<Vec<SqlValue>>,
            FutureReader<Result<(), PgError>>,
        ),
        PgError,
    >,
>;

const STREAM_BATCH_ROWS: usize = 500;

enum QueryTransaction {
    Operation(SharedTxnState),
    Dynamic(String),
}

struct OpenedStream {
    columns: Vec<String>,
    batches: mpsc::Receiver<Vec<Vec<SqlValue>>>,
    finished: oneshot::Receiver<WasmResult<Result<(), PgError>>>,
}

pub(super) async fn dynamic_query<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    sql: String,
    params: Vec<SqlValue>,
) -> QueryResult {
    let (plugin, component, trace) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
        ))
    })?;
    let opened = trace
        .run(async {
            let project = plugin.project_for(&component);
            let span = db_span_for_project(&plugin, &component, &project, "query");
            let started = std::time::Instant::now();
            let result = async {
                let project_key = plugin.require_project_env_key(&component)?;
                let (connection, policy) = plugin.checkout_dynamic(&project, &project_key).await?;
                let mut connection =
                    StatementConnectionGuard::new(connection, Arc::clone(&plugin.destroyed));
                let schema = plugin.schema_for(&component);
                let runner = plugin.runner_for(&component);
                let role = plugin.role_for(&component);
                let user = plugin.user_id_for(&component);
                let operation = plugin.operation_for(&component);
                let application = plugin.application_for(&component);
                let xid = plugin
                    .begin_with_identity(
                        connection.connection(),
                        wamn_identity_protocol::AuthorityClass::DynamicSql,
                        true,
                        &project_key,
                        schema.as_deref(),
                        runner.as_deref(),
                        role.as_deref(),
                        user.as_deref(),
                        operation.as_deref(),
                        application.as_deref(),
                        None,
                        policy.statement_timeout_ms,
                    )
                    .await?;
                open_stream(connection, QueryTransaction::Dynamic(xid), sql, params).await
            }
            .instrument(span)
            .await;
            record_query_ms("query", &project, started.elapsed());
            result
        })
        .await;
    into_guest_stream(accessor, opened)
}

pub(super) async fn transaction_query<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    rep: Resource<PgStatementTransaction>,
    sql: String,
    params: Vec<SqlValue>,
) -> QueryResult {
    let (state, destroyed) = transaction_state(accessor, rep)?;
    let (plugin, component, trace) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
        ))
    })?;
    let opened = trace
        .run(async {
            let project = plugin.project_for(&component);
            let span = db_span_for_project(&plugin, &component, &project, "statement.txn.run");
            let started = std::time::Instant::now();
            let result = async {
                let connection = StatementConnectionGuard::new(take_conn(&state)?, destroyed);
                open_stream(connection, QueryTransaction::Operation(state), sql, params).await
            }
            .instrument(span)
            .await;
            record_query_ms("statement.txn.run", &project, started.elapsed());
            result
        })
        .await;
    into_guest_stream(accessor, opened)
}

fn transaction_state<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    rep: Resource<PgStatementTransaction>,
) -> WasmResult<(SharedTxnState, Arc<std::sync::atomic::AtomicU64>)> {
    accessor.with(|mut access| {
        let ctx = access.get();
        let handle = ctx.table.get(&rep)?;
        wash_runtime::wasmtime::ensure!(
            handle.owner_scope == ctx.component_id.as_ref(),
            "SQL transaction belongs to a different invocation"
        );
        Ok((
            Arc::clone(&handle.transaction.state),
            Arc::clone(&handle.transaction.destroyed),
        ))
    })
}

pub(super) async fn transaction_batch<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    rep: Resource<PgStatementTransaction>,
    sql: String,
) -> WasmResult<Result<(), PgError>> {
    let (state, destroyed) = transaction_state(accessor, rep)?;
    Ok(
        super::resources::with_txn_conn(&state, &destroyed, |mut connection| async move {
            let result = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(connection.connection())
                .await
                .map(drop);
            (connection, result)
        })
        .await,
    )
}

fn into_guest_stream<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    opened: Result<OpenedStream, PgError>,
) -> QueryResult {
    let OpenedStream {
        columns,
        batches,
        finished,
    } = match opened {
        Ok(opened) => opened,
        Err(error) => return Ok(Err(error)),
    };
    accessor.with(|mut access| {
        let rows = StreamReader::new(&mut access, RowBatches { batches })?;
        let finished = FutureReader::new(&mut access, async move {
            finished
                .await
                .unwrap_or(Ok(Err(PgError::ConnectionUnavailable)))
        })?;
        Ok(Ok((columns, rows, finished)))
    })
}

async fn open_stream(
    mut connection: StatementConnectionGuard,
    transaction: QueryTransaction,
    sql: String,
    params: Vec<SqlValue>,
) -> Result<OpenedStream, PgError> {
    let (opened, started) = oneshot::channel();
    let (rows, batches) = mpsc::channel(1);
    let (outcome, finished) = oneshot::channel();
    tokio::spawn(async move {
        let mut opened = Some(opened);
        let result = tokio::select! {
            result = async {
                let conn = connection.connection();
                let prepared = sqlx::Executor::prepare(&mut *conn, sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(sql))).await.map_err(|error| map_pg_error(&error))?;
                let columns = columns_of(&prepared);
                opened.take().expect("a query opens once").send(Ok(columns)).map_err(|_| PgError::ConnectionUnavailable)?;
                use sqlx::Statement as _;
                let mut query = sqlx::query(prepared.sql().clone());
                for (index, param) in params.iter().enumerate() {
                    query = query.bind(PgParam::prepared(param, &prepared, index));
                }
                let stream = query.fetch(&mut *conn);
                futures_util::pin_mut!(stream);
                loop {
                    let permit = rows.reserve().await.map_err(|_| PgError::ConnectionUnavailable)?;
                    let mut batch = Vec::new();
                    for _ in 0..STREAM_BATCH_ROWS {
                        let Some(row) = stream.try_next().await.map_err(|error| map_pg_error(&error))? else { break; };
                        batch.push(decode_row(&row)?);
                    }
                    if batch.is_empty() { break; }
                    permit.send(batch);
                }
                Ok::<_, PgError>(())
            } => result,
            () = rows.closed() => Err(PgError::ConnectionUnavailable),
        };
        // Release the connection before resolving completion. The next query,
        // WAL read or host commit can use it as soon as completion succeeds.
        let result = match result {
            Ok(()) => {
                if let QueryTransaction::Operation(state) = &transaction {
                    restore_connection(connection, state);
                    Ok(())
                } else {
                    let QueryTransaction::Dynamic(xid) = &transaction else { unreachable!() };
                    match sqlx::raw_sql(sqlx::AssertSqlSafe(super::write_log::commit_sql(xid))).execute(connection.connection()).await {
                        Ok(_) => { connection.repool(); Ok(()) }
                        Err(error) => {
                            let _ = outcome.send(Err(wash_runtime::wasmtime::format_err!(
                                "finish dynamic SQL host transaction: {error}"
                            )));
                            return;
                        }
                    }
                }
            }
            Err(error) => {
                if let QueryTransaction::Operation(state) = &transaction {
                    if let Ok(mut state) = state.lock() { state.finished = true; }
                }
                Err(error)
            }
        };
        drop(rows);
        if let Some(opened) = opened { let _ = opened.send(result.map(|()| Vec::new())); }
        else { let _ = outcome.send(Ok(result)); }
    }.instrument(tracing::Span::current()));
    let columns = started
        .await
        .map_err(|_| PgError::ConnectionUnavailable)??;
    Ok(OpenedStream {
        columns,
        batches,
        finished,
    })
}
/// The guest's read end of a stream: each row batch, in order.
struct RowBatches {
    batches: mpsc::Receiver<Vec<Vec<SqlValue>>>,
}

impl<D> StreamProducer<D> for RowBatches {
    type Item = Vec<SqlValue>;
    type Buffer = VecBuffer<Vec<SqlValue>>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        store: StoreContextMut<'a, D>,
        mut destination: Destination<'a, Self::Item, Self::Buffer>,
        finish: bool,
    ) -> Poll<WasmResult<StreamResult>> {
        // A zero-length read asks only whether rows are ready. Answering
        // without buffering a batch keeps no row the guest may never read.
        if destination.remaining(store) == Some(0) {
            return Poll::Ready(Ok(StreamResult::Completed));
        }
        match self.get_mut().batches.poll_recv(cx) {
            Poll::Ready(Some(batch)) => {
                destination.set_buffer(batch.into());
                Poll::Ready(Ok(StreamResult::Completed))
            }
            Poll::Ready(None) => Poll::Ready(Ok(StreamResult::Dropped)),
            Poll::Pending if finish => Poll::Ready(Ok(StreamResult::Cancelled)),
            Poll::Pending => Poll::Pending,
        }
    }
}
