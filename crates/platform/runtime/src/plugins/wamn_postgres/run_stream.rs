//! `statements.run-stream` of `wamn:postgres@0.4.0`: one prepared
//! statement, with bounded host buffering and PostgreSQL's statement snapshot.

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

use super::claims::reject_claim_mutation;
use super::claims::transactions::{
    bind_session_settings, prepare_session_settings, read_wal_position,
};
use super::resources::{
    PgStatementTransaction, SharedTxnState, StatementConnectionGuard, admit_transaction_statement,
    db_span_for_project, plugin_of, record_query_ms, take_conn,
};
use super::statements::{
    VerifiedStatement, validate_prepared_statement, validate_statement_result,
};
use super::types::{PgParam, columns_of, decode_row, map_pg_error};
use super::{Column, PgError, SqlValue, StatementError, WamnPostgres};

type WasmResult<T> = wash_runtime::wasmtime::Result<T>;

/// The host holds at most one row batch and one batch in flight to the guest.
const STREAM_BATCH_ROWS: usize = 500;

pub(super) struct OpenedStream {
    pub(super) columns: Vec<Column>,
    pub(super) wal_position: Option<String>,
    pub(super) batches: mpsc::Receiver<Vec<Vec<SqlValue>>>,
    pub(super) finished: oneshot::Receiver<Result<(), StatementError>>,
}

/// The columns, rows, and completion of one admitted statement.
pub(super) async fn run_stream<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    instance: Option<&str>,
    statement_path: String,
    binds: Vec<SqlValue>,
    wal: Option<super::bindings::wamn::postgres::statements::WantWalPosition>,
) -> WasmResult<
    Result<
        (
            Vec<Column>,
            Option<String>,
            StreamReader<Vec<SqlValue>>,
            FutureReader<Result<(), StatementError>>,
        ),
        StatementError,
    >,
> {
    let (plugin, component_id, trace) = accessor.with(|mut access| {
        let mut ctx = access.get();
        let trace = wamn_engine::invocation_trace::invocation_trace(&mut ctx);
        Ok::<_, wash_runtime::wasmtime::Error>((
            plugin_of(&ctx)?,
            ctx.component_id.to_string(),
            trace,
        ))
    })?;
    let opened = trace
        .run(async move {
            plugin.refuse_in_host_transaction(&component_id, "run-stream")?;
            let project = plugin.project_for(&component_id);
            let active = plugin.active_statement_set(&component_id, instance);
            let statement = admit_transaction_statement(
                &plugin,
                &component_id,
                active.as_deref(),
                &statement_path,
                &binds,
            )?;
            let span = db_span_for_project(&plugin, &component_id, &project, "statement.stream");
            let started = std::time::Instant::now();
            let opened = open_stream(
                &plugin,
                &component_id,
                &project,
                statement_path,
                statement,
                binds,
                wal.is_some(),
            )
            .instrument(span)
            .await;
            record_query_ms("statement.stream", &project, started.elapsed());
            opened
        })
        .await;
    into_guest_stream(accessor, opened)
}

pub(super) type StreamCallResult = WasmResult<
    Result<
        (
            Vec<Column>,
            StreamReader<Vec<SqlValue>>,
            FutureReader<Result<(), StatementError>>,
        ),
        StatementError,
    >,
>;

pub(super) type ConnectionStreamCallResult = WasmResult<
    Result<
        (
            Vec<Column>,
            Option<String>,
            StreamReader<Vec<SqlValue>>,
            FutureReader<Result<(), StatementError>>,
        ),
        StatementError,
    >,
>;

fn into_guest_stream<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    opened: Result<OpenedStream, StatementError>,
) -> ConnectionStreamCallResult {
    let OpenedStream {
        columns,
        wal_position,
        batches,
        finished,
    } = match opened {
        Ok(opened) => opened,
        Err(error) => return Ok(Err(error)),
    };
    accessor.with(|mut access| {
        let stream = StreamReader::new(&mut access, RowBatches { batches })?;
        let finished = FutureReader::new(&mut access, async move {
            Ok::<_, wash_runtime::wasmtime::Error>(finished.await.unwrap_or(Err(
                StatementError::Postgres(PgError::ConnectionUnavailable),
            )))
        })?;
        Ok(Ok((columns, wal_position, stream, finished)))
    })
}

/// Stream on the connection owned by this transaction, never a second session.
pub(super) async fn transaction_run_stream<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
    rep: Resource<PgStatementTransaction>,
    statement_path: String,
    binds: Vec<SqlValue>,
) -> StreamCallResult {
    let (plugin, component_id, trace, state, destroyed, statements) =
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
                transaction.statements.clone(),
            ))
        })?;
    let opened = trace
        .run(async move {
            let statement = admit_transaction_statement(
                &plugin,
                &component_id,
                statements.as_deref(),
                &statement_path,
                &binds,
            )?;
            reject_claim_mutation(&statement.exact_sql).map_err(StatementError::Postgres)?;
            let connection = StatementConnectionGuard::new(
                take_conn(&state).map_err(StatementError::Postgres)?,
                destroyed,
            );
            let project = plugin.project_for(&component_id);
            let span =
                db_span_for_project(&plugin, &component_id, &project, "statement.txn.stream");
            let started = std::time::Instant::now();
            let result =
                open_transaction_stream(connection, state, statement_path, statement, binds)
                    .instrument(span)
                    .await;
            record_query_ms("statement.txn.stream", &project, started.elapsed());
            result
        })
        .await;
    Ok(into_guest_stream(accessor, opened)?.map(|(columns, _, rows, end)| (columns, rows, end)))
}

pub(super) async fn open_transaction_stream(
    connection: StatementConnectionGuard,
    state: SharedTxnState,
    statement_path: String,
    statement: Arc<VerifiedStatement>,
    binds: Vec<SqlValue>,
) -> Result<OpenedStream, StatementError> {
    let (opened, started) = oneshot::channel();
    let (rows, batches) = mpsc::channel(1);
    let (outcome, finished) = oneshot::channel();
    tokio::spawn(
        async move {
            let mut opened = Some(opened);
            let read = async {
                let conn = connection.connection();
                let prepared = conn
                    .prepare_cached(&statement.exact_sql)
                    .await
                    .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?;
                validate_prepared_statement(&statement_path, &statement, &prepared)?;
                let stream = conn
                    .query_raw(&prepared, binds.iter().map(PgParam))
                    .await
                    .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?;
                let columns = columns_of(&prepared);
                opened
                    .take()
                    .expect("a stream opens once")
                    .send(Ok(columns.clone()))
                    .map_err(|_| StatementError::Postgres(PgError::ConnectionUnavailable))?;
                pump(stream, &statement_path, &statement, columns, &rows).await
            };
            let result = tokio::select! {
                biased;
                result = read => result,
                () = rows.closed() => Err(StatementError::Postgres(PgError::ConnectionUnavailable)),
            };
            if result.is_ok() {
                connection.restore(&state);
            } else {
                if let Ok(mut transaction) = state.lock() {
                    transaction.finished = true;
                }
                // Closing the session aborts the whole transaction, without sending
                // a protocol cancellation that could reach the next statement.
                drop(connection);
            }
            if let Some(opened) = opened {
                let _ = opened.send(result.map(|()| Vec::new()));
            } else {
                let _ = outcome.send(result);
            }
        }
        .instrument(tracing::Span::current()),
    );
    let columns = started
        .await
        .map_err(|_| StatementError::Postgres(PgError::ConnectionUnavailable))??;
    Ok(OpenedStream {
        wal_position: None,
        columns,
        batches,
        finished,
    })
}

/// Execute once. Claims keep their transaction when they carry caller authority.
pub(super) async fn open_stream(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &str,
    statement_path: String,
    statement: Arc<VerifiedStatement>,
    binds: Vec<SqlValue>,
    want_wal: bool,
) -> Result<OpenedStream, StatementError> {
    reject_claim_mutation(&statement.exact_sql).map_err(StatementError::Postgres)?;
    let tenant = plugin
        .require_tenant(component_id)
        .map_err(StatementError::Postgres)?;
    let schema = plugin.schema_for(component_id);
    let runner = plugin.runner_for(component_id);
    let role = plugin.role_for(component_id);
    let (connection, policy, authority) = plugin
        .checkout_workload(component_id, project, &tenant)
        .await
        .map_err(StatementError::Postgres)?;
    let connection = StatementConnectionGuard::new(connection, Arc::clone(&plugin.destroyed));
    let postgres = |error: tokio_postgres::Error| StatementError::Postgres(map_pg_error(&error));
    let mut claims = None;
    let mut settings = None;
    let completion = if statement.transactional || role.is_some() {
        claims = Some(
            plugin
                .prepare_claims(
                    connection.connection(),
                    authority,
                    plugin.active_read(component_id),
                    &tenant,
                    schema.as_deref(),
                    runner.as_deref(),
                    role.as_deref(),
                    plugin.user_id_for(component_id).as_deref(),
                    plugin.operation_for(component_id).as_deref(),
                    plugin.current_run_for(component_id).as_ref(),
                    policy.statement_timeout_ms,
                )
                .await
                .map_err(StatementError::Postgres)?,
        );
        Some(
            connection
                .connection()
                .prepare_cached("COMMIT")
                .await
                .map_err(postgres)?,
        )
    } else {
        settings = prepare_session_settings(
            connection.connection(),
            &policy,
            schema.as_deref(),
            runner.as_deref(),
        )
        .await
        .map_err(StatementError::Postgres)?;
        None
    };
    let wal_statement = if want_wal {
        Some(
            connection
                .connection()
                .prepare_cached("SELECT pg_current_wal_lsn()::text")
                .await
                .map_err(postgres)?,
        )
    } else {
        None
    };
    // A cold Parse follows claims. COMMIT must wait until Execute was queued,
    // rather than overtaking a Parse that is still waiting for its description.
    let (mut queued, completion) = match completion {
        Some(commit) => {
            let (queued, ready) = oneshot::channel();
            (Some(queued), Some((commit, ready)))
        }
        None => (None, None),
    };
    let (opened, started) = oneshot::channel();
    let (rows, batches) = mpsc::channel(1);
    let (outcome, finished) = oneshot::channel();
    tokio::spawn(async move {
        let conn = connection.connection();
        let mut opened = Some(opened);
        let result = {
            let read = async {
                let read = async {
                    let bind = async {
                        if let Some(claims) = claims {
                            claims.bind(conn).await
                        } else {
                            bind_session_settings(conn, &policy, settings.as_ref(), schema, runner).await
                        }.map_err(StatementError::Postgres)
                    };
                    let execute = async {
                        let prepared = conn.prepare_cached(&statement.exact_sql).await.map_err(postgres)?;
                        validate_prepared_statement(&statement_path, &statement, &prepared)?;
                        if !statement.transactional
                            && let Some(queued) = queued.take()
                        {
                            let _ = queued.send(());
                        }
                        let stream = conn.query_raw(&prepared, binds.iter().map(PgParam)).await.map_err(postgres)?;
                        Ok::<_, StatementError>((prepared, stream))
                    };
                    let ((), wal_position, (prepared, stream)) = tokio::try_join!(biased; bind, read_wal_position(conn, wal_statement.as_ref()), execute)?;
                    let columns = columns_of(&prepared);
                    opened.take().expect("a stream opens once").send(Ok((columns.clone(), wal_position)))
                        .map_err(|_| StatementError::Postgres(PgError::ConnectionUnavailable))?;
                    pump(stream, &statement_path, &statement, columns, &rows).await?;
                    // A writing statement commits only after every row passed its contract.
                    if let Some(queued) = queued.take() {
                        let _ = queued.send(());
                    }
                    Ok::<_, StatementError>(())
                };
                // Receiver ownership also covers cancellation before the first row.
                // Once the read ends, a pending COMMIT can finish without the reader.
                tokio::select! {
                    biased;
                    result = read => result,
                    () = rows.closed() => Err(StatementError::Postgres(PgError::ConnectionUnavailable)),
                }
            };
            let finish = async {
                if let Some((commit, ready)) = completion {
                    ready.await.map_err(|_| StatementError::Postgres(PgError::ConnectionUnavailable))?;
                    conn.execute(&commit, &[]).await.map_err(postgres)?;
                }
                Ok::<_, StatementError>(())
            };
            tokio::try_join!(biased; read, finish).map(drop)
        };
        match result {
            Ok(()) => {
                connection.repool();
                let _ = outcome.send(Ok(()));
            }
            Err(error) => {
                // Destroy the session on cancellation or error. Closing the socket
                // aborts a query that may still be producing rows on the server.
                if let Some(opened) = opened {
                    let _ = opened.send(Err(error));
                } else {
                    let _ = outcome.send(Err(error));
                }
            }
        }
    }.instrument(tracing::Span::current()));
    let (columns, wal_position) = started
        .await
        .map_err(|_| StatementError::Postgres(PgError::ConnectionUnavailable))??;
    Ok(OpenedStream {
        columns,
        wal_position,
        batches,
        finished,
    })
}

/// Buffer rows from the one Execute, with backpressure from the guest.
async fn pump(
    stream: tokio_postgres::RowStream,
    statement_path: &str,
    statement: &VerifiedStatement,
    columns: Vec<Column>,
    rows: &mpsc::Sender<Vec<Vec<SqlValue>>>,
) -> Result<(), StatementError> {
    futures_util::pin_mut!(stream);
    loop {
        let permit = rows
            .reserve()
            .await
            .map_err(|_| StatementError::Postgres(PgError::ConnectionUnavailable))?;
        let mut batch = Vec::new();
        let mut ended = false;
        for _ in 0..STREAM_BATCH_ROWS {
            let Some(row) = stream
                .try_next()
                .await
                .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?
            else {
                ended = true;
                break;
            };
            batch.push(decode_row(&row).map_err(StatementError::Postgres)?);
        }
        if batch.is_empty() {
            return Ok(());
        }
        validate_statement_result(statement_path, statement, &columns, &batch)?;
        permit.send(batch);
        if ended {
            return Ok(());
        }
    }
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
