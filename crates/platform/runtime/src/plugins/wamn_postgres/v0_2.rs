//! `wamn:postgres@0.2.0`: every 0.1.0 call, served by the 0.1.0 host
//! implementation, plus `statements.run-stream` (owner ruling on `wamn-utci`).
//!
//! The 0.2.0 types interface and the four resources are the 0.1.0 Rust types,
//! so a value or a handle crosses both versions unchanged. Only the error
//! records that `statements` declares itself are distinct types, and
//! [`statement_error`] converts them.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use deadpool_postgres::Object;
use tokio::sync::{mpsc, oneshot};
use tokio_postgres::types::ToSql;
use tracing::Instrument as _;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx};
use wash_runtime::wasmtime::StoreContextMut;
use wash_runtime::wasmtime::component::{
    Accessor, Destination, FutureReader, Resource, StreamProducer, StreamReader, StreamResult,
    VecBuffer,
};

use super::claims::reject_claim_mutation;
use super::resources::{
    StatementConnectionGuard, admit_transaction_statement, begin_statement_transaction,
    db_span_for_project, plugin_of, record_query_ms, take_conn,
};
use super::statements::{
    VerifiedStatement, validate_prepared_statement, validate_statement_result,
};
use super::types::{PgParam, columns_of, decode_row, map_pg_error};
use super::{
    Column, PgCursor, PgError, PgStatementTransaction, PgTransaction, PgTransactionView, RowSet,
    SqlValue, StatementError, WamnPostgres, client, statement_wit,
};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "postgres-plugin-v0-2",
        imports: { default: async | trappable | tracing },
        with: {
            "wamn:postgres/types": super::super::bindings::wamn::postgres0_1_0::types,
            "wamn:postgres/client.transaction": super::super::PgTransaction,
            "wamn:postgres/client.cursor": super::super::PgCursor,
            "wamn:postgres/statements.transaction": super::super::PgStatementTransaction,
            "wamn:postgres/statements.transaction-view": super::super::PgTransactionView,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::postgres0_2_0::client as client_v2;
use bindings::wamn::postgres0_2_0::statements as statements_v2;

type WasmResult<T> = wash_runtime::wasmtime::Result<T>;

/// Rows one fetch reads from the stream's cursor. The host holds at most one
/// fetched batch and one batch in flight to the guest.
const STREAM_BATCH_ROWS: u32 = 500;

/// The cursor of a stream. Each stream has its own transaction, so one name
/// is enough.
const STREAM_CURSOR: &str = "wamn_stream";

/// Link the 0.2.0 client and statements interfaces on `linker`.
pub(super) fn add_to_linker(
    linker: &mut wash_runtime::wasmtime::component::Linker<SharedCtx>,
    client: bool,
    statements: bool,
) -> WasmResult<()> {
    if client {
        client_v2::add_to_linker::<_, SharedCtx>(
            linker,
            wash_runtime::engine::ctx::extract_active_ctx,
        )?;
    }
    if statements {
        statements_v2::add_to_linker::<_, SharedCtx>(
            linker,
            wash_runtime::engine::ctx::extract_active_ctx,
        )?;
    }
    Ok(())
}

/// The same failure in the 0.2.0 vocabulary, which repeats the 0.1.0 one.
fn statement_error(error: StatementError) -> statements_v2::StatementError {
    match error {
        StatementError::UnknownStatement(digest) => {
            statements_v2::StatementError::UnknownStatement(digest)
        }
        StatementError::StatementContractMismatch(mismatch) => {
            let shape = |shape: statement_wit::ValueShape| statements_v2::ValueShape {
                count: shape.count,
                types: shape.types,
            };
            statements_v2::StatementError::StatementContractMismatch(
                statements_v2::ContractMismatch {
                    statement_digest: mismatch.statement_digest,
                    part: match mismatch.part {
                        statement_wit::ContractPart::Binds => statements_v2::ContractPart::Binds,
                        statement_wit::ContractPart::Columns => {
                            statements_v2::ContractPart::Columns
                        }
                    },
                    expected: shape(mismatch.expected),
                    observed: shape(mismatch.observed),
                },
            )
        }
        StatementError::Postgres(error) => statements_v2::StatementError::Postgres(error),
    }
}

fn converted<T>(
    result: WasmResult<Result<T, StatementError>>,
) -> WasmResult<Result<T, statements_v2::StatementError>> {
    result.map(|result| result.map_err(statement_error))
}

impl statements_v2::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> statements_v2::HostWithStore<T> for SharedCtx {
    async fn selected_participation(
        accessor: &Accessor<T, Self>,
    ) -> WasmResult<Result<Option<statements_v2::Participation>, statements_v2::StatementError>>
    {
        converted(<Self as statement_wit::HostWithStore<T>>::selected_participation(accessor).await)
            .map(|result| {
                result.map(|selected| {
                    selected.map(|selected| statements_v2::Participation {
                        operation: selected.operation,
                        intent: selected.intent,
                    })
                })
            })
    }

    async fn participant_view(
        accessor: &Accessor<T, Self>,
    ) -> WasmResult<Result<Resource<PgTransactionView>, statements_v2::StatementError>> {
        converted(<Self as statement_wit::HostWithStore<T>>::participant_view(accessor).await)
    }

    async fn run(
        accessor: &Accessor<T, Self>,
        statement_digest: String,
        binds: Vec<SqlValue>,
    ) -> WasmResult<Result<RowSet, statements_v2::StatementError>> {
        converted(
            <Self as statement_wit::HostWithStore<T>>::run(accessor, statement_digest, binds).await,
        )
    }

    async fn run_stream(
        accessor: &Accessor<T, Self>,
        statement_digest: String,
        binds: Vec<SqlValue>,
    ) -> WasmResult<
        Result<
            (
                Vec<Column>,
                StreamReader<Vec<SqlValue>>,
                FutureReader<Result<(), statements_v2::StatementError>>,
            ),
            statements_v2::StatementError,
        >,
    > {
        let (plugin, component_id, trace) = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.component_id.to_string(),
                wamn_engine::invocation_trace::invocation_trace(&ctx),
            ))
        })?;
        let opened = trace
            .run(async move {
                let project = plugin.project_for(&component_id);
                let active = plugin.active_statement_set(&component_id);
                let statement = admit_transaction_statement(
                    &plugin,
                    &component_id,
                    active.as_deref(),
                    &statement_digest,
                    &binds,
                )?;
                let span =
                    db_span_for_project(&plugin, &component_id, &project, "statement.stream");
                let started = std::time::Instant::now();
                let opened = open_stream(
                    &plugin,
                    &component_id,
                    &project,
                    &statement_digest,
                    &statement,
                    &binds,
                )
                .instrument(span.clone())
                .await;
                record_query_ms("statement.stream", &project, started.elapsed());
                opened.map(|(columns, connection)| {
                    (columns, connection, statement_digest, statement, span)
                })
            })
            .await;
        let (columns, connection, statement_digest, statement, span) = match opened {
            Ok(opened) => opened,
            Err(error) => return Ok(Err(statement_error(error))),
        };
        let (rows, batches) = mpsc::channel(1);
        let (outcome, finished) = oneshot::channel();
        let stream_columns = columns.clone();
        tokio::spawn(
            async move {
                let pumped = pump(
                    &connection,
                    &statement_digest,
                    &statement,
                    stream_columns,
                    rows,
                )
                .await;
                // A read that ended, or a reader that left, closes the cursor.
                // ROLLBACK ends a query the reader no longer wants.
                let verb = if matches!(pumped, Ok(Pumped::Ended)) {
                    "COMMIT"
                } else {
                    "ROLLBACK"
                };
                let finished = match connection.connection().batch_execute(verb).await {
                    Ok(()) => {
                        connection.repool();
                        pumped.map(drop)
                    }
                    // The guard destroys a connection whose transaction state
                    // is unknown, so the server aborts it.
                    Err(error) => pumped.and(Err(StatementError::Postgres(map_pg_error(&error)))),
                };
                let _ = outcome.send(finished);
            }
            .instrument(span),
        );
        accessor.with(|mut access| {
            let stream = StreamReader::new(&mut access, RowBatches { batches })?;
            let finished = FutureReader::new(&mut access, async move {
                Ok::<_, wash_runtime::wasmtime::Error>(
                    finished
                        .await
                        .unwrap_or(Err(StatementError::Postgres(
                            PgError::ConnectionUnavailable,
                        )))
                        .map_err(statement_error),
                )
            })?;
            Ok(Ok((columns, stream, finished)))
        })
    }

    async fn begin(
        accessor: &Accessor<T, Self>,
    ) -> WasmResult<Result<Resource<PgStatementTransaction>, statements_v2::StatementError>> {
        converted(<Self as statement_wit::HostWithStore<T>>::begin(accessor).await)
    }
}

/// How a stream's reads ended when no error stopped them.
enum Pumped {
    /// The cursor returned an empty batch: every row reached the reader.
    Ended,
    /// The reader dropped the stream before the last row.
    Left,
}

/// Begin the claim transaction and declare the stream's cursor in it.
async fn open_stream(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &str,
    statement_digest: &str,
    statement: &VerifiedStatement,
    binds: &[SqlValue],
) -> Result<(Vec<Column>, StatementConnectionGuard), StatementError> {
    reject_claim_mutation(&statement.exact_sql).map_err(StatementError::Postgres)?;
    let transaction = begin_statement_transaction(plugin, component_id, project)
        .await
        .map_err(StatementError::Postgres)?;
    let connection = owned_connection(&transaction).map_err(StatementError::Postgres)?;
    drop(transaction);
    let postgres = |error: tokio_postgres::Error| StatementError::Postgres(map_pg_error(&error));
    let prepared = connection
        .connection()
        .prepare_cached(&statement.exact_sql)
        .await
        .map_err(postgres)?;
    validate_prepared_statement(statement_digest, statement, &prepared)?;
    let columns = columns_of(&prepared);
    let declare = format!(
        "DECLARE {STREAM_CURSOR} NO SCROLL CURSOR FOR {}",
        statement.exact_sql
    );
    let declared = connection
        .connection()
        .prepare(&declare)
        .await
        .map_err(postgres)?;
    let values: Vec<PgParam> = binds.iter().map(|value| PgParam(value.clone())).collect();
    connection
        .connection()
        .execute_raw(&declared, values.iter().map(|value| value as &dyn ToSql))
        .await
        .map_err(postgres)?;
    Ok((columns, connection))
}

/// Take the connection out of a begun transaction, which then owns nothing.
fn owned_connection(transaction: &PgTransaction) -> Result<StatementConnectionGuard, PgError> {
    let connection = take_conn(&transaction.state)?;
    if let Ok(mut state) = transaction.state.lock() {
        state.finished = true;
    }
    Ok(StatementConnectionGuard::new(
        connection,
        Arc::clone(&transaction.destroyed),
    ))
}

/// Fetch batches from the cursor and hand each one to the reader.
async fn pump(
    connection: &StatementConnectionGuard,
    statement_digest: &str,
    statement: &VerifiedStatement,
    columns: Vec<Column>,
    rows: mpsc::Sender<Vec<Vec<SqlValue>>>,
) -> Result<Pumped, StatementError> {
    let connection: &Object = connection.connection();
    let postgres = |error: tokio_postgres::Error| StatementError::Postgres(map_pg_error(&error));
    // The server describes a FETCH by the cursor it names, and each stream's
    // cursor has its own columns. A cached FETCH would keep the columns of
    // the first cursor on this pooled connection, so each stream prepares its
    // own, once.
    let fetch = connection
        .prepare(&format!(
            "FETCH FORWARD {STREAM_BATCH_ROWS} FROM {STREAM_CURSOR}"
        ))
        .await
        .map_err(postgres)?;
    loop {
        let Ok(permit) = rows.reserve().await else {
            return Ok(Pumped::Left);
        };
        let fetched = connection.query(&fetch, &[]).await.map_err(postgres)?;
        if fetched.is_empty() {
            return Ok(Pumped::Ended);
        }
        let batch = fetched
            .iter()
            .map(decode_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(StatementError::Postgres)?;
        let batch = validate_statement_result(
            statement_digest,
            statement,
            RowSet {
                columns: columns.clone(),
                rows: batch,
            },
        )?;
        permit.send(batch.rows);
    }
}

/// The guest's read end of a stream: each fetched batch, in order.
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

impl<T: 'static + Send> statements_v2::HostTransactionWithStore<T> for SharedCtx {
    async fn select_participant(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
        participant_operation: String,
    ) -> WasmResult<Result<(), statements_v2::StatementError>> {
        converted(
            <Self as statement_wit::HostTransactionWithStore<T>>::select_participant(
                accessor,
                rep,
                participant_operation,
            )
            .await,
        )
    }

    async fn run(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
        statement_digest: String,
        binds: Vec<SqlValue>,
    ) -> WasmResult<Result<RowSet, statements_v2::StatementError>> {
        converted(
            <Self as statement_wit::HostTransactionWithStore<T>>::run(
                accessor,
                rep,
                statement_digest,
                binds,
            )
            .await,
        )
    }

    async fn commit(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
    ) -> WasmResult<Result<(), statements_v2::StatementError>> {
        converted(<Self as statement_wit::HostTransactionWithStore<T>>::commit(accessor, rep).await)
    }

    async fn rollback(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgStatementTransaction>,
    ) -> WasmResult<Result<(), statements_v2::StatementError>> {
        converted(
            <Self as statement_wit::HostTransactionWithStore<T>>::rollback(accessor, rep).await,
        )
    }
}

impl statements_v2::HostTransaction for ActiveCtx<'_> {
    async fn drop(&mut self, rep: Resource<PgStatementTransaction>) -> WasmResult<()> {
        statement_wit::HostTransaction::drop(self, rep).await
    }
}

impl<T: 'static + Send> statements_v2::HostTransactionViewWithStore<T> for SharedCtx {
    async fn run(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransactionView>,
        statement_digest: String,
        binds: Vec<SqlValue>,
    ) -> WasmResult<Result<RowSet, statements_v2::StatementError>> {
        converted(
            <Self as statement_wit::HostTransactionViewWithStore<T>>::run(
                accessor,
                rep,
                statement_digest,
                binds,
            )
            .await,
        )
    }
}

impl statements_v2::HostTransactionView for ActiveCtx<'_> {
    async fn drop(&mut self, rep: Resource<PgTransactionView>) -> WasmResult<()> {
        statement_wit::HostTransactionView::drop(self, rep).await
    }
}

impl client_v2::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> client_v2::HostWithStore<T> for SharedCtx {
    async fn query(
        accessor: &Accessor<T, Self>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> WasmResult<Result<RowSet, PgError>> {
        <Self as client::HostWithStore<T>>::query(accessor, sql, params).await
    }

    async fn execute(
        accessor: &Accessor<T, Self>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> WasmResult<Result<u64, PgError>> {
        <Self as client::HostWithStore<T>>::execute(accessor, sql, params).await
    }

    async fn begin(
        accessor: &Accessor<T, Self>,
    ) -> WasmResult<Result<Resource<PgTransaction>, PgError>> {
        <Self as client::HostWithStore<T>>::begin(accessor).await
    }
}

impl<T: 'static + Send> client_v2::HostTransactionWithStore<T> for SharedCtx {
    async fn query(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> WasmResult<Result<RowSet, PgError>> {
        <Self as client::HostTransactionWithStore<T>>::query(accessor, rep, sql, params).await
    }

    async fn execute(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> WasmResult<Result<u64, PgError>> {
        <Self as client::HostTransactionWithStore<T>>::execute(accessor, rep, sql, params).await
    }

    async fn open_cursor(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> WasmResult<Result<Resource<PgCursor>, PgError>> {
        <Self as client::HostTransactionWithStore<T>>::open_cursor(accessor, rep, sql, params).await
    }

    async fn commit(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
    ) -> WasmResult<Result<(), PgError>> {
        <Self as client::HostTransactionWithStore<T>>::commit(accessor, rep).await
    }

    async fn rollback(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgTransaction>,
    ) -> WasmResult<Result<(), PgError>> {
        <Self as client::HostTransactionWithStore<T>>::rollback(accessor, rep).await
    }
}

impl<T: 'static + Send> client_v2::HostCursorWithStore<T> for SharedCtx {
    async fn fetch(
        accessor: &Accessor<T, Self>,
        rep: Resource<PgCursor>,
        max_rows: u32,
    ) -> WasmResult<Result<RowSet, PgError>> {
        <Self as client::HostCursorWithStore<T>>::fetch(accessor, rep, max_rows).await
    }
}

impl client_v2::HostTransaction for ActiveCtx<'_> {
    async fn drop(&mut self, rep: Resource<PgTransaction>) -> WasmResult<()> {
        client::HostTransaction::drop(self, rep).await
    }
}

impl client_v2::HostCursor for ActiveCtx<'_> {
    async fn drop(&mut self, rep: Resource<PgCursor>) -> WasmResult<()> {
        client::HostCursor::drop(self, rep).await
    }
}
