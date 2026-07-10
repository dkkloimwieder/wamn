//! Guest bench/gate component for the S2 `wamn:postgres` spike.
//!
//! One export, `run(op, arg)`, that the `wamn-host pgbench` harness calls
//! repeatedly. All SQL is parameterized ($1..$n) — there is no string
//! interpolation of `arg` into SQL anywhere in this file; `arg` only ever
//! travels as a bound parameter value. The guest never learns or chooses its
//! tenant; the host injects it from workload identity.

wit_bindgen::generate!({
    world: "pgprobe",
    path: "wit",
    generate_all,
});

use wamn::postgres::client::{self, Transaction};
use wamn::postgres::types::{PgError, SqlValue};

struct Component;
export!(Component);

/// Name a pg-error by its variant, with no host-supplied detail beyond the
/// taxonomy tag. Used as the export's error channel so gates can assert on the
/// error kind. `permission-denied` deliberately stays detail-free.
fn err_name(e: &PgError) -> String {
    match e {
        PgError::SerializationFailure => "serialization-failure".into(),
        PgError::ConnectionUnavailable => "connection-unavailable".into(),
        PgError::StatementTimeout => "statement-timeout".into(),
        PgError::RowLimitExceeded(n) => format!("row-limit-exceeded:{n}"),
        PgError::UniqueViolation(c) => format!("unique-violation:{c}"),
        PgError::ForeignKeyViolation(c) => format!("foreign-key-violation:{c}"),
        PgError::CheckViolation(c) => format!("check-violation:{c}"),
        PgError::PermissionDenied => "permission-denied".into(),
        PgError::QueryError((code, msg)) => format!("query-error:{code}:{msg}"),
    }
}

/// The 8-parameter, ≤10-row single-statement query the qps gate measures.
const QPS_SQL: &str = "SELECT id, tenant_id, g, a, b, num, ts, payload \
     FROM s2.bench \
     WHERE g = $1 AND a >= $2 AND b >= $3 AND c >= $4 \
       AND num >= $5 AND ts >= $6 AND payload LIKE $7 \
     ORDER BY id LIMIT $8";

fn qps_params(g: i32) -> Vec<SqlValue> {
    vec![
        SqlValue::Int32(g),
        SqlValue::Int32(0),
        SqlValue::Int64(0),
        SqlValue::Float64(0.0),
        SqlValue::Numeric("0.0000".into()),
        SqlValue::Timestamptz("2026-01-01T00:00:00+00:00".into()),
        SqlValue::Text("payload-%".into()),
        SqlValue::Int64(10),
    ]
}

impl Guest for Component {
    fn run(op: u32, arg: String) -> Result<u64, String> {
        match op {
            // qps: one 8-param query, up to 10 rows.
            0 => {
                // `arg` is a decimal group selector, kept as a bound param.
                let g: i32 = arg.parse().unwrap_or(0);
                match client::query(QPS_SQL, &qps_params(g)) {
                    Ok(rs) => Ok(rs.rows.len() as u64),
                    Err(e) => Err(err_name(&e)),
                }
            }
            // chaos: begin, write inside the txn, then busy-loop until the
            // host epoch-kills the store mid-transaction.
            1 => {
                let txn: Transaction = client::begin().map_err(|e| err_name(&e))?;
                // A write so the transaction holds real state to roll back.
                let _ = txn.execute(
                    "INSERT INTO s2.scratch (tenant_id, k, v) \
                     VALUES (current_setting('app.tenant', true), $1, $2) \
                     ON CONFLICT (tenant_id, k) DO UPDATE SET v = excluded.v",
                    &[
                        SqlValue::Text(format!("chaos-{arg}")),
                        SqlValue::Text("in-flight".into()),
                    ],
                );
                // Never returns; the harness traps this via the epoch deadline.
                let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
                loop {
                    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                    core::hint::black_box(x);
                }
            }
            // rls-count: count rows matching a LIKE pattern; RLS must confine
            // the visible set to this guest's own tenant regardless of pattern.
            2 => {
                let sql = "SELECT count(*) FROM s2.rls_secrets WHERE secret LIKE $1";
                match client::query(sql, &[SqlValue::Text(arg)]) {
                    Ok(rs) => match rs.rows.first().and_then(|r| r.first()) {
                        Some(SqlValue::Int64(n)) => Ok(*n as u64),
                        Some(SqlValue::Int32(n)) => Ok(*n as u64),
                        _ => Err("unexpected count shape".into()),
                    },
                    Err(e) => Err(err_name(&e)),
                }
            }
            // rls-write-other: try to insert a row tagged with a FOREIGN
            // tenant. WITH CHECK must reject it (permission-denied).
            3 => match client::execute(
                "INSERT INTO s2.rls_secrets (tenant_id, secret) VALUES ($1, $2)",
                &[
                    SqlValue::Text(arg),
                    SqlValue::Text("cross-tenant-write-attempt".into()),
                ],
            ) {
                Ok(n) => Ok(n),
                Err(e) => Err(err_name(&e)),
            },
            // injection: store `arg` as a bound param value, read it back, and
            // report whether the round-trip is byte-identical.
            4 => {
                let key = "inj-probe";
                if let Err(e) = client::execute(
                    "INSERT INTO s2.scratch (tenant_id, k, v) \
                     VALUES (current_setting('app.tenant', true), $1, $2) \
                     ON CONFLICT (tenant_id, k) DO UPDATE SET v = excluded.v",
                    &[SqlValue::Text(key.into()), SqlValue::Text(arg.clone())],
                ) {
                    return Err(err_name(&e));
                }
                match client::query(
                    "SELECT v FROM s2.scratch WHERE k = $1",
                    &[SqlValue::Text(key.into())],
                ) {
                    Ok(rs) => match rs.rows.first().and_then(|r| r.first()) {
                        Some(SqlValue::Text(v)) => Ok((*v == arg) as u64),
                        Some(SqlValue::Null) => Ok((arg.is_empty()) as u64),
                        other => Err(format!("unexpected readback: {other:?}")),
                    },
                    Err(e) => Err(err_name(&e)),
                }
            }
            // cursor: stream this tenant's bench rows in bounded batches.
            5 => {
                let txn = client::begin().map_err(|e| err_name(&e))?;
                let cursor = txn
                    .open_cursor("SELECT id FROM s2.bench ORDER BY id", &[])
                    .map_err(|e| err_name(&e))?;
                let mut total: u64 = 0;
                loop {
                    let batch = cursor.fetch(256).map_err(|e| err_name(&e))?;
                    if batch.rows.is_empty() {
                        break;
                    }
                    total += batch.rows.len() as u64;
                }
                txn.commit().map_err(|e| err_name(&e))?;
                Ok(total)
            }
            // slow: hold a connection for `arg` seconds (saturation probe).
            // Uses execute so the void result of pg_sleep isn't decoded.
            6 => {
                let secs: f64 = arg.parse().unwrap_or(0.2);
                match client::execute("SELECT pg_sleep($1)", &[SqlValue::Float64(secs)]) {
                    Ok(_) => Ok(1),
                    Err(e) => Err(err_name(&e)),
                }
            }
            // count-items: unqualified name resolves in this component's project
            // DB; RLS confines to its tenant; subject to the project's row limit.
            10 => match client::query("SELECT id FROM items", &[]) {
                Ok(rs) => Ok(rs.rows.len() as u64),
                Err(e) => Err(err_name(&e)),
            },
            // db-marker: a distinct constant per project DB — proves routing.
            11 => match client::query("SELECT n FROM marker", &[]) {
                Ok(rs) => match rs.rows.first().and_then(|r| r.first()) {
                    Some(SqlValue::Int32(n)) => Ok(*n as u64),
                    Some(SqlValue::Int64(n)) => Ok(*n as u64),
                    other => Err(format!("unexpected marker shape: {other:?}")),
                },
                Err(e) => Err(err_name(&e)),
            },
            other => Err(format!("unknown op {other}")),
        }
    }
}
