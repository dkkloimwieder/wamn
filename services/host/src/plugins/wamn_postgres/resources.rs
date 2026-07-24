//! Transaction / cursor resources and the WIT host implementations for
//! `wamn:postgres` (SR4 split, wamn-cjv.18): the crash-safe `PgTransaction` /
//! `PgCursor` handles, the connection-lifecycle helpers, the statement drivers
//! (`run_query` / `run_execute`), and the `client` / `causation` host traits
//! backed by the `WamnPostgres` plugin resolved from the invoking context.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use deadpool_postgres::Object;
use futures_util::TryStreamExt as _;
use tokio_postgres::types::ToSql;
use tracing::Instrument as _;
use wash_runtime::engine::ctx::ActiveCtx;
use wash_runtime::wasmtime::component::Resource;

use wamn_event_wire::Causation;

use super::claims::{OneShotResult, reject_claim_mutation};
use super::pool::destroy_connection;
use super::types::{PgParam, columns_of, decode_row, map_pg_error};
use super::{PgError, RowSet, SqlValue, WAMN_POSTGRES_ID, WamnPostgres, causation, client};

struct TxnState {
    /// Present while the transaction owns a connection. Taken out for the
    /// duration of each call (a std mutex guard cannot be held across await).
    conn: Option<Object>,
    /// True once COMMIT or ROLLBACK ran (connection repooled).
    finished: bool,
}

type SharedTxnState = Arc<std::sync::Mutex<TxnState>>;

/// Host side of a `wamn:postgres/client.transaction`.
///
/// The [`Drop`] impl is the crash-safety guarantee: if the resource dies
/// without an explicit finish — guest trap, epoch kill, store teardown — the
/// connection is destroyed (socket closed, server aborts the transaction),
/// never repooled.
pub struct PgTransaction {
    state: SharedTxnState,
    destroyed: Arc<AtomicU64>,
    cursor_seq: u32,
    /// Row limit of the project this transaction's connection belongs to.
    row_limit: u64,
}

impl Drop for PgTransaction {
    fn drop(&mut self) {
        let mut st = match self.state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(obj) = st.conn.take() {
            if st.finished {
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

fn take_conn(state: &SharedTxnState) -> Result<Object, PgError> {
    let mut st = state.lock().map_err(|_| txn_closed())?;
    if st.finished {
        return Err(txn_closed());
    }
    st.conn.take().ok_or_else(txn_closed)
}

fn put_conn(state: &SharedTxnState, obj: Object) {
    if let Ok(mut st) = state.lock() {
        st.conn = Some(obj);
    }
}

/// Run `op` with the transaction's connection. Fatal (connection-level)
/// errors destroy the connection and poison the transaction; statement-level
/// errors return the connection to the slot (the transaction is aborted
/// server-side until the guest rolls back, mirroring libpq semantics).
async fn with_txn_conn<T, F, Fut>(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    op: F,
) -> Result<T, PgError>
where
    F: FnOnce(Object) -> Fut,
    Fut: std::future::Future<Output = (Object, Result<T, tokio_postgres::Error>)>,
{
    let conn = take_conn(state)?;
    let (conn, result) = op(conn).await;
    match result {
        Ok(v) => {
            put_conn(state, conn);
            Ok(v)
        }
        Err(e) => {
            let mapped = map_pg_error(&e);
            if e.is_closed() {
                if let Ok(mut st) = state.lock() {
                    st.finished = true;
                }
                destroy_connection(conn, destroyed);
            } else {
                put_conn(state, conn);
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
    reject_claim_mutation(sql)?;
    let stmt = conn
        .prepare_cached(sql)
        .await
        .map_err(|e| map_pg_error(&e))?;
    let columns = columns_of(&stmt);
    let wrapped: Vec<PgParam> = params.iter().map(|p| PgParam(p.clone())).collect();
    let stream = conn
        .query_raw(&stmt, wrapped.iter().map(|p| p as &dyn ToSql))
        .await
        .map_err(|e| map_pg_error(&e))?;
    futures_util::pin_mut!(stream);
    let mut rows = Vec::new();
    while let Some(row) = stream.try_next().await.map_err(|e| map_pg_error(&e))? {
        if rows.len() as u64 >= row_limit {
            return Err(PgError::RowLimitExceeded(row_limit));
        }
        rows.push(decode_row(&row)?);
    }
    Ok(RowSet { columns, rows })
}

pub(super) async fn run_execute(
    conn: &Object,
    sql: &str,
    params: &[SqlValue],
) -> Result<u64, PgError> {
    reject_claim_mutation(sql)?;
    let stmt = conn
        .prepare_cached(sql)
        .await
        .map_err(|e| map_pg_error(&e))?;
    let wrapped: Vec<PgParam> = params.iter().map(|p| PgParam(p.clone())).collect();
    conn.execute_raw(&stmt, wrapped.iter().map(|p| p as &dyn ToSql))
        .await
        .map_err(|e| map_pg_error(&e))
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<WamnPostgres>> {
    ctx.try_get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)
}

impl causation::Host for ActiveCtx<'_> {
    /// The trusted flow-runner declares (or clears, with `none`) the causation
    /// context of the run it is driving (l5i9.12.2). Only components linked with
    /// [`add_runner_causation_to_linker`] can call this. The declaration feeds
    /// the [`WamnPostgres`] plugin's per-component run map, so every subsequent
    /// transaction the plugin opens for this component stamps a `wamn.causation`
    /// message. If no postgres plugin is present in this context (a runner-less
    /// bench), the declaration is a harmless no-op.
    async fn set_run_context(
        &mut self,
        ctx: Option<causation::RunContext>,
    ) -> wash_runtime::wasmtime::Result<()> {
        let component = self.component_id.to_string();
        let run = ctx.map(|c| Causation {
            run: c.run,
            root: c.root,
            depth: c.depth,
        });
        tracing::debug!(
            target: "wamn::causation",
            component,
            run = ?run.as_ref().map(|c| &c.run),
            "per-run causation context declared"
        );
        if let Ok(plugin) = plugin_of(self) {
            plugin.set_current_run(&component, run);
        }
        Ok(())
    }
}

/// [9.8] Guest DB-call latency histogram (ms), labelled by `db.operation`
/// (query / execute / txn.query / txn.execute) and `wamn.project`. On the global
/// meter beside the 9.1 `wamn.postgres` span — a no-op until a provider is
/// installed (`OTEL_*`). Recorded around the awaited call at each `db_span` site.
static QUERY_DURATION_MS: std::sync::LazyLock<opentelemetry::metrics::Histogram<f64>> =
    std::sync::LazyLock::new(|| {
        opentelemetry::global::meter("wamn-postgres")
            .f64_histogram("wamn.postgres.query.duration_ms")
            .with_description("wamn:postgres guest DB call latency in ms, by db.operation")
            .build()
    });

/// Record one guest DB call's wall time on [`QUERY_DURATION_MS`]. `op` matches
/// the `db_span` operation; `project` is the executing component's project.
fn record_query_ms(op: &'static str, project: &str, elapsed: std::time::Duration) {
    QUERY_DURATION_MS.record(
        elapsed.as_secs_f64() * 1000.0,
        &[
            opentelemetry::KeyValue::new("db.operation", op),
            opentelemetry::KeyValue::new("wamn.project", project.to_string()),
        ],
    );
}

/// [9.1] A `wamn.postgres` span over one guest DB call, enriched host-side with
/// the executing component's tenant/project (the same claim maps that inject
/// `app.tenant`; the guest cannot spoof them). Emitted through the process's
/// global `tracing` subscriber, which the fork's `initialize_observability`
/// bridges to OTel and exports over OTLP when `OTEL_*` is set — so the span
/// nests under whatever span is current (a request handler, or a
/// [`crate::dispatch::trigger_span`]) and threads into that trace. Enriching a
/// host-created span keeps 9.1 wamn-side (no fork patch); `run_id`/`node_id`
/// enrichment on this span awaits the 9.2 guest→host run-context contract.
fn db_span(plugin: &WamnPostgres, component_id: &str, op: &'static str) -> tracing::Span {
    let tenant = plugin.tenant_for(component_id).unwrap_or_default();
    let project = plugin.project_for(component_id);
    tracing::info_span!(
        "wamn.postgres",
        db.system = "postgresql",
        db.operation = op,
        wamn.tenant = %tenant,
        wamn.project = %project,
        wamn.component = %component_id,
    )
}

impl client::Host for ActiveCtx<'_> {
    async fn query(
        &mut self,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
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
    }

    async fn execute(
        &mut self,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
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
            Ok(OneShotResult::Rows(_)) => unreachable!("one_shot(!want_rows) returns count"),
            Err(e) => Err(e),
        })
    }

    async fn begin(
        &mut self,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgTransaction>, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();

        let tenant = match plugin.require_tenant(&component_id) {
            Ok(t) => t,
            Err(e) => return Ok(Err(e)),
        };
        let project = plugin.project_for(&component_id);
        let schema = plugin.schema_for(&component_id);
        let runner = plugin.runner_for(&component_id);
        let run = plugin.current_run_for(&component_id);
        let (conn, pp) = match plugin.checkout(&project).await {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        if let Err(e) = plugin
            .begin_with_claims(
                &conn,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                run.as_ref(),
                pp.statement_timeout_ms,
            )
            .await
        {
            plugin.destroy(conn);
            return Ok(Err(e));
        }
        let txn = PgTransaction {
            state: Arc::new(std::sync::Mutex::new(TxnState {
                conn: Some(conn),
                finished: false,
            })),
            destroyed: plugin.destroyed.clone(),
            cursor_seq: 0,
            row_limit: pp.row_limit,
        };
        Ok(Ok(self.table.push(txn)?))
    }
}

impl client::HostTransaction for ActiveCtx<'_> {
    async fn query(
        &mut self,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let span = db_span(&plugin, &component_id, "txn.query");
        let project = plugin.project_for(&component_id);
        let txn = self.table.get(&rep)?;
        let row_limit = txn.row_limit;
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        let t0 = std::time::Instant::now();
        let out = with_txn_conn(&state, &destroyed, |conn| async move {
            let r = run_query(&conn, &sql, &params, row_limit).await;
            // run_query maps errors already; re-split for with_txn_conn's
            // fatal/statement distinction by probing conn liveness.
            (conn, flatten_mapped(r))
        })
        .instrument(span)
        .await
        .and_then(|r| r);
        record_query_ms("txn.query", &project, t0.elapsed());
        Ok(out)
    }

    async fn execute(
        &mut self,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let span = db_span(&plugin, &component_id, "txn.execute");
        let project = plugin.project_for(&component_id);
        let txn = self.table.get(&rep)?;
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        let t0 = std::time::Instant::now();
        let out = with_txn_conn(&state, &destroyed, |conn| async move {
            let r = run_execute(&conn, &sql, &params).await;
            (conn, flatten_mapped(r))
        })
        .instrument(span)
        .await
        .and_then(|r| r);
        record_query_ms("txn.execute", &project, t0.elapsed());
        Ok(out)
    }

    async fn open_cursor(
        &mut self,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgCursor>, PgError>> {
        // A cursor over `SELECT set_config('app.tenant', …)` would execute the
        // override on fetch; guard the same surface as query/execute (wamn-cjv.2).
        if let Err(e) = reject_claim_mutation(&sql) {
            return Ok(Err(e));
        }
        let txn = self.table.get_mut(&rep)?;
        txn.cursor_seq += 1;
        let name = format!("wamn_c{}", txn.cursor_seq);
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());

        let declare = format!("DECLARE {name} CURSOR FOR {sql}");
        let result = with_txn_conn(&state, &destroyed, |conn| async move {
            let r = async {
                let stmt = conn.prepare(&declare).await?;
                let wrapped: Vec<PgParam> = params.iter().map(|p| PgParam(p.clone())).collect();
                conn.execute_raw(&stmt, wrapped.iter().map(|p| p as &dyn ToSql))
                    .await
            }
            .await;
            (conn, r)
        })
        .await;
        Ok(match result {
            Ok(_) => Ok(self.table.push(PgCursor {
                state,
                destroyed,
                name,
            })?),
            Err(e) => Err(e),
        })
    }

    async fn commit(
        &mut self,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        let txn = self.table.get(&rep)?;
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        Ok(finish_txn(&state, &destroyed, "COMMIT").await)
    }

    async fn rollback(
        &mut self,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        let txn = self.table.get(&rep)?;
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        Ok(finish_txn(&state, &destroyed, "ROLLBACK").await)
    }

    async fn drop(&mut self, rep: Resource<PgTransaction>) -> wash_runtime::wasmtime::Result<()> {
        let txn = self.table.delete(rep)?;
        // Graceful guest-side drop without commit: contract says roll back.
        // The connection is protocol-clean after a successful ROLLBACK, so it
        // can be repooled; failure falls through to the destroying Drop.
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        let already_finished = state
            .lock()
            .map(|st| st.finished || st.conn.is_none())
            .unwrap_or(true);
        if !already_finished {
            let _ = finish_txn(&state, &destroyed, "ROLLBACK").await;
        }
        drop(txn); // Drop impl destroys the connection iff still unfinished
        Ok(())
    }
}

/// COMMIT or ROLLBACK, then repool the connection and mark the transaction
/// finished. On failure the connection is destroyed.
async fn finish_txn(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    verb: &str,
) -> Result<(), PgError> {
    let conn = take_conn(state)?;
    match conn.batch_execute(verb).await {
        Ok(()) => {
            if let Ok(mut st) = state.lock() {
                st.finished = true;
            }
            drop(conn); // back to the pool
            Ok(())
        }
        Err(e) => {
            if let Ok(mut st) = state.lock() {
                st.finished = true;
            }
            destroy_connection(conn, destroyed);
            Err(map_pg_error(&e))
        }
    }
}

/// Adapter: our helpers return `Result<T, PgError>` but [`with_txn_conn`]
/// wants the raw `tokio_postgres::Error` to judge fatality. Statement-level
/// failures were already mapped, so wrap them back up as an Ok(Err(..)).
fn flatten_mapped<T>(r: Result<T, PgError>) -> Result<Result<T, PgError>, tokio_postgres::Error> {
    Ok(r)
}

impl client::HostCursor for ActiveCtx<'_> {
    async fn fetch(
        &mut self,
        rep: Resource<PgCursor>,
        max_rows: u32,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let cursor = self.table.get(&rep)?;
        let (state, destroyed, name) = (
            cursor.state.clone(),
            cursor.destroyed.clone(),
            cursor.name.clone(),
        );
        Ok(with_txn_conn(&state, &destroyed, |conn| async move {
            let r = async {
                let sql = format!("FETCH FORWARD {max_rows} FROM {name}");
                let stmt = conn.prepare(&sql).await?;
                let columns = columns_of(&stmt);
                let rows = conn.query(&stmt, &[]).await?;
                Ok::<_, tokio_postgres::Error>((columns, rows))
            }
            .await;
            (conn, r)
        })
        .await
        .and_then(|(columns, rows)| {
            let rows = rows.iter().map(decode_row).collect::<Result<Vec<_>, _>>()?;
            Ok(RowSet { columns, rows })
        }))
    }

    async fn drop(&mut self, rep: Resource<PgCursor>) -> wash_runtime::wasmtime::Result<()> {
        // Server-side cursors die with their transaction; nothing to release.
        self.table.delete(rep)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
