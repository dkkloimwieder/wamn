//! Stream query results with bounded buffering and a separate completion result.

use futures_util::TryStreamExt as _;
use tokio::sync::{mpsc, oneshot};
use tracing::Instrument as _;
use wamn_application::database::{SqlError, SqlRows, SqlValue};

use super::ConnectionGuard;
use super::transaction::{SharedTxnState, restore_connection};
use super::values::{PgParam, columns_of, decode_row, map_pg_error};

const STREAM_BATCH_ROWS: usize = 500;

pub(super) enum QueryTransaction {
    Operation(SharedTxnState),
    Dynamic(String),
}

pub(super) async fn open_stream(
    mut connection: ConnectionGuard,
    transaction: QueryTransaction,
    sql: String,
    params: Vec<SqlValue>,
) -> Result<SqlRows, SqlError> {
    use sqlx::Statement as _;

    let (opened, started) = oneshot::channel();
    let (rows, mut batches) = mpsc::channel(1);
    let (outcome, finished) = oneshot::channel();
    tokio::spawn(async move {
        let mut opened = Some(opened);
        let result = tokio::select! {
            result = async {
                let conn = connection.connection();
                let prepared = sqlx::Executor::prepare(&mut *conn, sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(sql))).await.map_err(|error| map_pg_error(&error))?;
                let columns = columns_of(&prepared);
                opened.take().expect("a query opens once").send(Ok(columns)).map_err(|_| SqlError::ConnectionUnavailable)?;
                let mut query = sqlx::query(prepared.sql().clone());
                for (index, param) in params.iter().enumerate() {
                    query = query.bind(PgParam::prepared(param, &prepared, index));
                }
                let stream = query.fetch(&mut *conn);
                futures_util::pin_mut!(stream);
                loop {
                    let permit = rows.reserve().await.map_err(|_| SqlError::ConnectionUnavailable)?;
                    let mut batch = Vec::new();
                    for _ in 0..STREAM_BATCH_ROWS {
                        let Some(row) = stream.try_next().await.map_err(|error| map_pg_error(&error))? else { break; };
                        batch.push(decode_row(&row)?);
                    }
                    if batch.is_empty() { break; }
                    permit.send(batch);
                }
                Ok::<_, SqlError>(())
            } => result,
            () = rows.closed() => Err(SqlError::ConnectionUnavailable),
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
                    let committed = async {
                        super::write_log::commit_transaction(connection.connection(), xid, None, None)
                            .await.map_err(|error| error.source)
                    }.await;
                    match committed {
                        Ok(()) => { connection.repool(); Ok(()) }
                        Err(error) => {
                            let _ = outcome.send(Err(anyhow::anyhow!(
                                "finish dynamic SQL host transaction: {error}"
                            )));
                            return;
                        }
                    }
                }
            }
            Err(error) => {
                if let QueryTransaction::Operation(state) = &transaction
                    && let Ok(mut state) = state.lock() {
                    state.finished = true;
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
        .map_err(|_| SqlError::ConnectionUnavailable)??;
    Ok(SqlRows {
        columns,
        batches: Box::pin(futures_util::stream::poll_fn(move |context| {
            batches.poll_recv(context)
        })),
        finished: Box::pin(async move {
            finished
                .await
                .unwrap_or(Ok(Err(SqlError::ConnectionUnavailable)))
        }),
    })
}
