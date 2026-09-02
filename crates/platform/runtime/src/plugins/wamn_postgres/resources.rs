//! Transaction / cursor resources and the WIT host implementations for
//! `wamn:postgres` (SR4 split, wamn-cjv.18): the crash-safe `PgTransaction` /
//! `PgCursor` handles, the connection-lifecycle helpers, the statement drivers
//! (`run_query` / `run_execute`), and the `client` host traits backed by the
//! `WamnPostgres` plugin resolved from the invoking context.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use deadpool_postgres::Object;
use futures_util::TryStreamExt as _;
use tokio_postgres::types::ToSql;
use tracing::Instrument as _;
use wash_runtime::engine::ctx::ActiveCtx;
use wash_runtime::wasmtime::component::Resource;

use crate::plugins::effect_span::{EffectIdentity, effect_span, record_effect_ms};

use super::claims::{OneShotResult, reject_claim_mutation};
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

/// Host side of a `wamn:postgres/statements.transaction`.
///
/// The operation's statement set is snapshotted at `begin`; changing or
/// revoking the invocation scope cannot widen an already-open transaction.
pub struct PgStatementTransaction {
    transaction: PgTransaction,
    statements: Option<Arc<BoundStatementSet>>,
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
    async {
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
        let wrapped: Vec<PgParam> = params.iter().map(|p| PgParam(p.clone())).collect();
        conn.execute_raw(&stmt, wrapped.iter().map(|p| p as &dyn ToSql))
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
    digest: &str,
    statement: &VerifiedStatement,
    binds: &[SqlValue],
    row_limit: u64,
) -> Result<RowSet, StatementError> {
    async {
        reject_claim_mutation(&statement.exact_sql).map_err(StatementError::Postgres)?;
        let prepared = conn
            .prepare_cached(&statement.exact_sql)
            .await
            .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?;
        validate_prepared_statement(digest, statement, &prepared)?;
        let columns = columns_of(&prepared);
        let wrapped: Vec<PgParam> = binds.iter().map(|value| PgParam(value.clone())).collect();
        let stream = conn
            .query_raw(&prepared, wrapped.iter().map(|value| value as &dyn ToSql))
            .await
            .map_err(|error| StatementError::Postgres(map_pg_error(&error)))?;
        futures_util::pin_mut!(stream);
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
        validate_statement_result(digest, statement, RowSet { columns, rows })
    }
    .instrument(tracing::info_span!(
        "wamn.postgres.statement",
        db.system = "postgresql",
        db.operation = "verified-query",
        statement.digest = digest,
    ))
    .await
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<WamnPostgres>> {
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
fn record_query_ms(op: &'static str, project: &str, elapsed: std::time::Duration) {
    record_effect_ms(&QUERY_DURATION_MS, DB_OPERATION, op, project, elapsed);
}

/// [9.1] A `wamn.postgres` span over one guest DB call, enriched host-side with
/// the executing component's tenant/project (the same claim maps that inject
/// `app.tenant`; the guest cannot spoof them). The name and the `db.*` fields are
/// this surface's own — a Tempo panel in `dashboards.md` slices traces by the
/// span name — while the `wamn.*` identity block is [`effect_span`]'s, shared with
/// every other effect surface.
///
/// `run_id`/`node_id` enrichment awaits a guest→host run-context contract; the
/// trusted HTTP effect is the one surface whose WIT carries those coordinates
/// today.
fn db_span(plugin: &WamnPostgres, component_id: &str, op: &'static str) -> tracing::Span {
    let project = plugin.project_for(component_id);
    db_span_for_project(plugin, component_id, &project, op)
}

fn db_span_for_project(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &str,
    op: &'static str,
) -> tracing::Span {
    let tenant = plugin.tenant_for(component_id).unwrap_or_default();
    effect_span!(
        "wamn.postgres",
        EffectIdentity {
            tenant: &tenant,
            project,
            component: component_id,
        },
        None,
        db.system = "postgresql",
        db.operation = op,
    )
}

async fn begin_transaction(
    plugin: &WamnPostgres,
    component_id: &str,
    project: &str,
) -> Result<PgTransaction, PgError> {
    let tenant = plugin.require_tenant(component_id)?;
    let schema = plugin.schema_for(component_id);
    let runner = plugin.runner_for(component_id);
    let role = plugin.role_for(component_id);
    let user_id = plugin.user_id_for(component_id);
    let run = plugin.current_run_for(component_id);
    let (conn, pp, authority) = plugin
        .checkout_workload(component_id, project, &tenant)
        .await?;
    if let Err(e) = plugin
        .begin_with_claims(
            &conn,
            authority,
            &tenant,
            schema.as_deref(),
            runner.as_deref(),
            role.as_deref(),
            user_id.as_deref(),
            run.as_ref(),
            pp.statement_timeout_ms,
        )
        .await
    {
        plugin.destroy(conn);
        return Err(e);
    }
    Ok(PgTransaction {
        state: Arc::new(std::sync::Mutex::new(TxnState {
            conn: Some(conn),
            finished: false,
        })),
        destroyed: plugin.destroyed.clone(),
        cursor_seq: 0,
        row_limit: pp.row_limit,
    })
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
        Ok(Ok(self.table.push(txn)?))
    }
}

#[cfg(feature = "wasm_component_model_implements")]
impl bindings::named_imports::wamn::postgres::client::Host for ActiveCtx<'_> {
    async fn query(
        &mut self,
        id: super::NamedProject,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
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
    }

    async fn execute(
        &mut self,
        id: super::NamedProject,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
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
            Ok(OneShotResult::Rows(_)) => unreachable!("one_shot(!want_rows) returns count"),
            Err(e) => Err(e),
        })
    }

    async fn begin(
        &mut self,
        id: super::NamedProject,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgTransaction>, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let project = id.project().to_string();
        let span = db_span_for_project(&plugin, &component_id, &project, "begin");
        let t0 = std::time::Instant::now();
        let opened = begin_transaction(&plugin, &component_id, &project)
            .instrument(span)
            .await;
        record_query_ms("begin", &project, t0.elapsed());
        match opened {
            Ok(txn) => Ok(Ok(self.table.push(txn)?)),
            Err(e) => Ok(Err(e)),
        }
    }
}

async fn txn_query(
    ctx: &mut ActiveCtx<'_>,
    project: &str,
    rep: Resource<PgTransaction>,
    sql: String,
    params: Vec<SqlValue>,
) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
    let plugin = plugin_of(ctx)?;
    let component_id = ctx.component_id.to_string();
    let span = db_span_for_project(&plugin, &component_id, project, "txn.query");
    let txn = ctx.table.get(&rep)?;
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
    record_query_ms("txn.query", project, t0.elapsed());
    Ok(out)
}

async fn txn_execute(
    ctx: &mut ActiveCtx<'_>,
    project: &str,
    rep: Resource<PgTransaction>,
    sql: String,
    params: Vec<SqlValue>,
) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
    let plugin = plugin_of(ctx)?;
    let component_id = ctx.component_id.to_string();
    let span = db_span_for_project(&plugin, &component_id, project, "txn.execute");
    let txn = ctx.table.get(&rep)?;
    let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
    let t0 = std::time::Instant::now();
    let out = with_txn_conn(&state, &destroyed, |conn| async move {
        let r = run_execute(&conn, &sql, &params).await;
        (conn, flatten_mapped(r))
    })
    .instrument(span)
    .await
    .and_then(|r| r);
    record_query_ms("txn.execute", project, t0.elapsed());
    Ok(out)
}

async fn txn_open_cursor(
    ctx: &mut ActiveCtx<'_>,
    project: &str,
    rep: Resource<PgTransaction>,
    sql: String,
    params: Vec<SqlValue>,
) -> wash_runtime::wasmtime::Result<Result<Resource<PgCursor>, PgError>> {
    // A cursor over `SELECT set_config('app.tenant', …)` would execute the
    // override on fetch; guard the same surface as query/execute (wamn-cjv.2).
    if let Err(e) = reject_claim_mutation(&sql) {
        return Ok(Err(e));
    }
    let plugin = plugin_of(ctx)?;
    let component_id = ctx.component_id.to_string();
    let span = db_span_for_project(&plugin, &component_id, project, "txn.open_cursor");
    let txn = ctx.table.get_mut(&rep)?;
    txn.cursor_seq += 1;
    let name = format!("wamn_c{}", txn.cursor_seq);
    let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
    let declare = format!("DECLARE {name} CURSOR FOR {sql}");
    let t0 = std::time::Instant::now();
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
    .instrument(span)
    .await;
    record_query_ms("txn.open_cursor", project, t0.elapsed());
    Ok(match result {
        Ok(_) => Ok(ctx.table.push(PgCursor {
            state,
            destroyed,
            name,
        })?),
        Err(e) => Err(e),
    })
}

async fn txn_finish(
    ctx: &mut ActiveCtx<'_>,
    project: &str,
    rep: Resource<PgTransaction>,
    verb: &'static str,
) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
    let plugin = plugin_of(ctx)?;
    let component_id = ctx.component_id.to_string();
    let op = match verb {
        "COMMIT" => "txn.commit",
        "ROLLBACK" => "txn.rollback",
        _ => unreachable!("transaction finish verb is fixed"),
    };
    let span = db_span_for_project(&plugin, &component_id, project, op);
    let txn = ctx.table.get(&rep)?;
    let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
    let t0 = std::time::Instant::now();
    let result = finish_txn(&state, &destroyed, verb).instrument(span).await;
    record_query_ms(op, project, t0.elapsed());
    Ok(result)
}

async fn txn_drop(
    ctx: &mut ActiveCtx<'_>,
    rep: Resource<PgTransaction>,
) -> wash_runtime::wasmtime::Result<()> {
    let txn = ctx.table.delete(rep)?;
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

async fn cursor_fetch(
    ctx: &mut ActiveCtx<'_>,
    project: &str,
    rep: Resource<PgCursor>,
    max_rows: u32,
) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
    let plugin = plugin_of(ctx)?;
    let component_id = ctx.component_id.to_string();
    let span = db_span_for_project(&plugin, &component_id, project, "cursor.fetch");
    let cursor = ctx.table.get(&rep)?;
    let (state, destroyed, name) = (
        cursor.state.clone(),
        cursor.destroyed.clone(),
        cursor.name.clone(),
    );
    let t0 = std::time::Instant::now();
    let fetched = with_txn_conn(&state, &destroyed, |conn| async move {
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
    .instrument(span)
    .await;
    record_query_ms("cursor.fetch", project, t0.elapsed());
    Ok(fetched.and_then(|(columns, rows)| {
        let rows = rows.iter().map(decode_row).collect::<Result<Vec<_>, _>>()?;
        Ok(RowSet { columns, rows })
    }))
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
impl bindings::named_imports::wamn::postgres::client::HostTransaction for ActiveCtx<'_> {
    async fn query(
        &mut self,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        txn_query(self, id.project(), rep, sql, params).await
    }

    async fn execute(
        &mut self,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        txn_execute(self, id.project(), rep, sql, params).await
    }

    async fn open_cursor(
        &mut self,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgCursor>, PgError>> {
        txn_open_cursor(self, id.project(), rep, sql, params).await
    }

    async fn commit(
        &mut self,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        txn_finish(self, id.project(), rep, "COMMIT").await
    }

    async fn rollback(
        &mut self,
        id: super::NamedProject,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        txn_finish(self, id.project(), rep, "ROLLBACK").await
    }

    async fn drop(
        &mut self,
        _id: super::NamedProject,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<()> {
        txn_drop(self, rep).await
    }
}

#[cfg(feature = "wasm_component_model_implements")]
impl bindings::named_imports::wamn::postgres::client::HostCursor for ActiveCtx<'_> {
    async fn fetch(
        &mut self,
        id: super::NamedProject,
        rep: Resource<PgCursor>,
        max_rows: u32,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        cursor_fetch(self, id.project(), rep, max_rows).await
    }

    async fn drop(
        &mut self,
        _id: super::NamedProject,
        rep: Resource<PgCursor>,
    ) -> wash_runtime::wasmtime::Result<()> {
        cursor_drop(self, rep)
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
        let project = plugin.project_for(&component_id);
        txn_query(self, &project, rep, sql, params).await
    }

    async fn execute(
        &mut self,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let project = plugin.project_for(&component_id);
        txn_execute(self, &project, rep, sql, params).await
    }

    async fn open_cursor(
        &mut self,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgCursor>, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let project = plugin.project_for(&component_id);
        txn_open_cursor(self, &project, rep, sql, params).await
    }

    async fn commit(
        &mut self,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let project = plugin.project_for(&component_id);
        txn_finish(self, &project, rep, "COMMIT").await
    }

    async fn rollback(
        &mut self,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let project = plugin.project_for(&component_id);
        txn_finish(self, &project, rep, "ROLLBACK").await
    }

    async fn drop(&mut self, rep: Resource<PgTransaction>) -> wash_runtime::wasmtime::Result<()> {
        txn_drop(self, rep).await
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
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let project = plugin.project_for(&component_id);
        cursor_fetch(self, &project, rep, max_rows).await
    }

    async fn drop(&mut self, rep: Resource<PgCursor>) -> wash_runtime::wasmtime::Result<()> {
        cursor_drop(self, rep)
    }
}

impl statement_wit::Host for ActiveCtx<'_> {
    async fn run(
        &mut self,
        statement_digest: String,
        binds: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let active = plugin.active_statement_set(&component_id);
        let statement = match resolve_statement(active.as_deref(), &statement_digest, &binds) {
            Ok(statement) => statement,
            Err(error) => return Ok(Err(error)),
        };
        let project = plugin.project_for(&component_id);
        let span = db_span_for_project(&plugin, &component_id, &project, "statement.run");
        let started = std::time::Instant::now();
        let result = plugin
            .one_shot_statement(&component_id, &statement_digest, &statement, &binds)
            .instrument(span)
            .await;
        record_query_ms("statement.run", &project, started.elapsed());
        Ok(result)
    }

    async fn begin(
        &mut self,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgStatementTransaction>, StatementError>>
    {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let project = plugin.project_for(&component_id);
        let statements = plugin.active_statement_set(&component_id);
        let span = db_span_for_project(&plugin, &component_id, &project, "statement.begin");
        let started = std::time::Instant::now();
        let opened = begin_transaction(&plugin, &component_id, &project)
            .instrument(span)
            .await;
        record_query_ms("statement.begin", &project, started.elapsed());
        match opened {
            Ok(transaction) => Ok(Ok(self.table.push(PgStatementTransaction {
                transaction,
                statements,
            })?)),
            Err(error) => Ok(Err(StatementError::Postgres(error))),
        }
    }
}

impl statement_wit::HostTransaction for ActiveCtx<'_> {
    async fn run(
        &mut self,
        rep: Resource<PgStatementTransaction>,
        statement_digest: String,
        binds: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, StatementError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let project = plugin.project_for(&component_id);
        let transaction = self.table.get(&rep)?;
        let statement =
            match resolve_statement(transaction.statements.as_deref(), &statement_digest, &binds) {
                Ok(statement) => statement,
                Err(error) => return Ok(Err(error)),
            };
        let state = Arc::clone(&transaction.transaction.state);
        let destroyed = Arc::clone(&transaction.transaction.destroyed);
        let row_limit = transaction.transaction.row_limit;
        let conn = match take_conn(&state) {
            Ok(conn) => conn,
            Err(error) => return Ok(Err(StatementError::Postgres(error))),
        };
        let span = db_span_for_project(&plugin, &component_id, &project, "statement.txn.run");
        let started = std::time::Instant::now();
        let result = run_verified_query(&conn, &statement_digest, &statement, &binds, row_limit)
            .instrument(span)
            .await;
        record_query_ms("statement.txn.run", &project, started.elapsed());

        let poison = matches!(
            &result,
            Err(StatementError::StatementContractMismatch(_))
                | Err(StatementError::Postgres(PgError::ConnectionUnavailable))
        );
        if poison {
            if let Ok(mut transaction) = state.lock() {
                transaction.finished = true;
            }
            destroy_connection(conn, &destroyed);
        } else {
            put_conn(&state, conn);
        }
        Ok(result)
    }

    async fn commit(
        &mut self,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
        statement_txn_finish(self, rep, "COMMIT").await
    }

    async fn rollback(
        &mut self,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
        statement_txn_finish(self, rep, "ROLLBACK").await
    }

    async fn drop(
        &mut self,
        rep: Resource<PgStatementTransaction>,
    ) -> wash_runtime::wasmtime::Result<()> {
        let transaction = self.table.delete(rep)?;
        let state = Arc::clone(&transaction.transaction.state);
        let destroyed = Arc::clone(&transaction.transaction.destroyed);
        let already_finished = state
            .lock()
            .map(|transaction| transaction.finished || transaction.conn.is_none())
            .unwrap_or(true);
        if !already_finished {
            let _ = finish_txn(&state, &destroyed, "ROLLBACK").await;
        }
        drop(transaction);
        Ok(())
    }
}

async fn statement_txn_finish(
    ctx: &mut ActiveCtx<'_>,
    rep: Resource<PgStatementTransaction>,
    verb: &'static str,
) -> wash_runtime::wasmtime::Result<Result<(), StatementError>> {
    let plugin = plugin_of(ctx)?;
    let component_id = ctx.component_id.to_string();
    let project = plugin.project_for(&component_id);
    let operation = match verb {
        "COMMIT" => "statement.txn.commit",
        "ROLLBACK" => "statement.txn.rollback",
        _ => unreachable!("transaction finish verb is fixed"),
    };
    let span = db_span_for_project(&plugin, &component_id, &project, operation);
    let transaction = ctx.table.get(&rep)?;
    let state = Arc::clone(&transaction.transaction.state);
    let destroyed = Arc::clone(&transaction.transaction.destroyed);
    let started = std::time::Instant::now();
    let result = finish_txn(&state, &destroyed, verb)
        .instrument(span)
        .await
        .map_err(StatementError::Postgres);
    record_query_ms(operation, &project, started.elapsed());
    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
