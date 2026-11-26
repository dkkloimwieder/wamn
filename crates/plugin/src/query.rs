//! SQL query results with bounded buffering and a closing result.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::stream::BoxStream;
use tracing::Instrument as _;
use wamn_app::database::{DatabaseTarget, SqlError, SqlRows, SqlValue as DatabaseValue};
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::wasmtime::StoreContextMut;
use wash_runtime::wasmtime::component::{
    Accessor, Destination, FutureReader, Resource, StreamProducer, StreamReader, StreamResult,
    VecBuffer,
};

use super::operation_sql::Transaction;
use super::resources::{PgStatementTransaction, db_span_for_project, plugin_of, record_query_ms};
use super::scope_registry;
use super::types::{into_database_value, into_guest_error, into_guest_value};
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
            let project = scope_registry::project_for(&plugin.scopes, &component);
            let span = db_span_for_project(&plugin, &component, &project, "query");
            let started = std::time::Instant::now();
            let result = async {
                let identity = scope_registry::require_sql_identity(&plugin.scopes, &component)?;
                let target = DatabaseTarget {
                    project: &project,
                    env: identity
                        .project_env_key
                        .as_ref()
                        .expect("required SQL identity"),
                };
                crate::operation_sql::read(
                    &plugin.store,
                    target,
                    &identity,
                    sql,
                    params.into_iter().map(into_database_value).collect(),
                )
                .await
                .map_err(into_guest_error)
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
    rep: &Resource<PgStatementTransaction>,
    sql: String,
    params: Vec<SqlValue>,
) -> QueryResult {
    let Some(transaction) = transaction_session(accessor, rep)? else {
        return Ok(Err(into_guest_error(SqlError::closed_transaction())));
    };
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
            let project = scope_registry::project_for(&plugin.scopes, &component);
            let span = db_span_for_project(&plugin, &component, &project, "statement.txn.run");
            let started = std::time::Instant::now();
            let result = transaction
                .query(sql, params.into_iter().map(into_database_value).collect())
                .instrument(span)
                .await
                .map_err(into_guest_error);
            record_query_ms("statement.txn.run", &project, started.elapsed());
            result
        })
        .await;
    into_guest_stream(accessor, opened)
}

fn transaction_session<T: 'static>(
    accessor: &Accessor<T, SharedCtx>,
    rep: &Resource<PgStatementTransaction>,
) -> WasmResult<Option<Arc<Transaction>>> {
    accessor.with(|mut access| {
        let ctx = access.get();
        let handle = ctx.table.get(rep)?;
        wash_runtime::wasmtime::ensure!(
            handle.owner_scope == ctx.component_id.as_ref(),
            "SQL transaction belongs to a different invocation"
        );
        Ok(handle.transaction.upgrade())
    })
}

pub(super) async fn transaction_batch<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    rep: &Resource<PgStatementTransaction>,
    sql: String,
) -> WasmResult<Result<(), PgError>> {
    let Some(transaction) = transaction_session(accessor, rep)? else {
        return Ok(Err(into_guest_error(SqlError::closed_transaction())));
    };
    Ok(transaction.query_batch(sql).await.map_err(into_guest_error))
}

fn into_guest_stream<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    opened: Result<SqlRows, PgError>,
) -> QueryResult {
    let SqlRows {
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
                .map(|completion| completion.map_err(into_guest_error))
                .map_err(|error| wash_runtime::wasmtime::format_err!("{error}"))
        })?;
        Ok(Ok((columns, rows, finished)))
    })
}

/// The guest's read end of a stream: each row batch, in order.
struct RowBatches {
    batches: BoxStream<'static, Vec<Vec<DatabaseValue>>>,
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
        match self.get_mut().batches.as_mut().poll_next(cx) {
            Poll::Ready(Some(batch)) => {
                let batch = batch
                    .into_iter()
                    .map(|row| row.into_iter().map(into_guest_value).collect())
                    .collect::<Vec<_>>();
                destination.set_buffer(batch.into());
                Poll::Ready(Ok(StreamResult::Completed))
            }
            Poll::Ready(None) => Poll::Ready(Ok(StreamResult::Dropped)),
            Poll::Pending if finish => Poll::Ready(Ok(StreamResult::Cancelled)),
            Poll::Pending => Poll::Pending,
        }
    }
}
