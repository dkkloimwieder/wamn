//! The `postgres-query` standard node (D8, wamn-r13).
//!
//! Author-written SQL keeps values bound as `$n`
//!   params, behind the per-project `RawSql` capability (DEFAULT OFF; the
//!   dispatch check refuses it before this node runs — enablement for real
//!   projects is gated on the dedicated user-SQL role, wamn-1nd).
//!
//! It classifies `wamn:postgres` failures MECHANICALLY per the frozen 0.1 WIT
//! annotation (`docs/archive/contracts/wamn-postgres.wit`): serialization-failure /
//! connection-unavailable / statement-timeout → retryable; the rest terminal.

use serde_json::{Value, json};
use wamn_entity_access::shape_rows;
use wamn_flow::node_contract::{
    Capability, Emission, ErrorDetail, Node, NodeCtx, NodeError, PgCapError, PgValue, RunContext,
};
use wamn_pg_core::SqlValue;

use crate::expr::{config_str, eval_to_value};

// ---------------------------------------------------------------------------
// Shared classification + value mirrors
// ---------------------------------------------------------------------------

/// `wamn:postgres` failure → node taxonomy, mechanically per the WIT.
pub(crate) fn classify_pg(e: PgCapError) -> NodeError {
    match e {
        PgCapError::NotGranted => NodeError::Terminal(ErrorDetail::coded(
            "capability-denied",
            "postgres access is not granted to this node",
        )),
        PgCapError::SerializationFailure => NodeError::Retryable(ErrorDetail::coded(
            "serialization-failure",
            "the transaction serialization failed; safe to retry",
        )),
        PgCapError::ConnectionUnavailable => NodeError::Retryable(ErrorDetail::coded(
            "connection-unavailable",
            "no database connection was available",
        )),
        PgCapError::StatementTimeout => NodeError::Retryable(ErrorDetail::coded(
            "statement-timeout",
            "the statement exceeded its time budget",
        )),
        PgCapError::RowLimitExceeded(n) => NodeError::Terminal(ErrorDetail::coded(
            "row-limit-exceeded",
            format!("the result exceeded the project row limit ({n})"),
        )),
        PgCapError::UniqueViolation(c) => constraint_err("unique-violation", c),
        PgCapError::ForeignKeyViolation(c) => constraint_err("foreign-key-violation", c),
        PgCapError::CheckViolation(c) => constraint_err("check-violation", c),
        PgCapError::PermissionDenied => NodeError::Terminal(ErrorDetail::coded(
            "permission-denied",
            "the database role refused the statement",
        )),
        PgCapError::QueryError { code, message } => NodeError::Terminal(ErrorDetail {
            message,
            code: Some("query-error".into()),
            data: Some(json!({ "sqlstate": code })),
        }),
    }
}

fn constraint_err(code: &str, constraint: String) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: format!("{code} on constraint {constraint:?}"),
        code: Some(code.to_string()),
        data: Some(json!({ "constraint": constraint })),
    })
}

/// SDK `PgValue` → `wamn_pg_core::SqlValue` (for response shaping).
pub(crate) fn pg_to_api(v: &PgValue) -> SqlValue {
    match v {
        PgValue::Null => SqlValue::Null,
        PgValue::Bool(b) => SqlValue::Bool(*b),
        PgValue::Int32(n) => SqlValue::Int32(*n),
        PgValue::Int64(n) => SqlValue::Int64(*n),
        PgValue::Float64(f) => SqlValue::Float64(*f),
        PgValue::Text(s) => SqlValue::Text(s.clone()),
        PgValue::Bytes(b) => SqlValue::Bytes(b.clone()),
        PgValue::Numeric(s) => SqlValue::Numeric(s.clone()),
        PgValue::Timestamptz(s) => SqlValue::Timestamptz(s.clone()),
        PgValue::Json(s) => SqlValue::Json(s.clone()),
        PgValue::Uuid(s) => SqlValue::Uuid(s.clone()),
    }
}

// ---------------------------------------------------------------------------
// postgres-query — author-written SQL (D8, flag-gated)
// ---------------------------------------------------------------------------

/// Config:
/// ```jsonc
/// {
///   "sql": "SELECT ... WHERE x = $1",
///   "params": ["receipt.id", "lines[0].quantity"],  // jmespath per $n
///   "mode": "query" | "execute"                     // default "query"
/// }
/// ```
/// Values ALWAYS bind as `$n` params (never spliced); the statement runs as
/// the project role under the tenant claim + RLS floor. Payloads: query →
/// `{"rows": [...]}`; execute → `{"rows-affected": n}`.
pub(crate) struct PostgresQuery;

impl Node for PostgresQuery {
    fn capabilities(&self) -> &'static [Capability] {
        &[Capability::Postgres, Capability::RawSql]
    }

    fn run(
        &self,
        ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        let config = run.config;
        let sql = config_str(config, "sql")?;

        let mut params: Vec<PgValue> = Vec::new();
        if let Some(exprs) = config.get("params") {
            let list = exprs.as_array().ok_or_else(|| {
                NodeError::Terminal(ErrorDetail::coded(
                    "invalid-config",
                    "postgres-query \"params\" must be an array of expressions",
                ))
            })?;
            for e in list {
                let expr = e.as_str().ok_or_else(|| {
                    NodeError::Terminal(ErrorDetail::coded(
                        "invalid-config",
                        "postgres-query params must be jmespath strings",
                    ))
                })?;
                params.push(value_to_pg(eval_to_value(expr, input, run.context)?));
            }
        }

        match config
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("query")
        {
            "execute" => {
                let n = ctx.pg_execute(sql, &params).map_err(classify_pg)?;
                Ok(Emission::main(json!({ "rows-affected": n })))
            }
            "query" => {
                let rows = ctx.pg_query(sql, &params).map_err(classify_pg)?;
                let api_rows: Vec<Vec<SqlValue>> = rows
                    .rows
                    .iter()
                    .map(|r| r.iter().map(pg_to_api).collect())
                    .collect();
                Ok(Emission::main(json!({
                    "rows": shape_rows(&rows.columns, &api_rows)
                })))
            }
            other => Err(NodeError::Terminal(ErrorDetail::coded(
                "invalid-config",
                format!("unknown postgres-query mode {other:?}"),
            ))),
        }
    }
}

/// A JSON param value → wire value. Strings go as text (the server casts per
/// the declared column type — exact decimals/uuids/timestamps travel as
/// strings, the S2 text wire format); JSON floats map to `float64` (raw SQL is
/// the author's power tool — catalog numerics should be passed as strings).
fn value_to_pg(v: Value) -> PgValue {
    match v {
        Value::Null => PgValue::Null,
        Value::Bool(b) => PgValue::Bool(b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => PgValue::Int64(i),
            None => PgValue::Float64(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => PgValue::Text(s),
        v @ (Value::Array(_) | Value::Object(_)) => PgValue::Json(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE mechanical pg-error → taxonomy map, pinned per the frozen 0.1 WIT
    /// annotation (docs/archive/contracts/wamn-postgres.wit): serialization-failure /
    /// connection-unavailable / statement-timeout → retryable; the rest
    /// terminal. A swapped arm here is the taxonomy mutant the retry engine
    /// would silently amplify (retrying a unique violation forever / failing a
    /// transient outage instantly).
    #[test]
    fn pg_errors_classify_mechanically_per_the_wit() {
        let retryable = [
            PgCapError::SerializationFailure,
            PgCapError::ConnectionUnavailable,
            PgCapError::StatementTimeout,
        ];
        for e in retryable {
            assert!(
                matches!(classify_pg(e.clone()), NodeError::Retryable(_)),
                "{e:?} must be retryable"
            );
        }
        let terminal = [
            PgCapError::NotGranted,
            PgCapError::RowLimitExceeded(4),
            PgCapError::UniqueViolation("u".into()),
            PgCapError::ForeignKeyViolation("f".into()),
            PgCapError::CheckViolation("c".into()),
            PgCapError::PermissionDenied,
            PgCapError::QueryError {
                code: "42601".into(),
                message: "syntax error".into(),
            },
        ];
        for e in terminal {
            assert!(
                matches!(classify_pg(e.clone()), NodeError::Terminal(_)),
                "{e:?} must be terminal"
            );
        }
    }

    /// Constraint failures carry the constraint name as machine-readable data
    /// (the F1/S2 precedent: the taxonomy keeps raw constraint names).
    #[test]
    fn constraint_violations_carry_the_constraint() {
        let NodeError::Terminal(d) = classify_pg(PgCapError::UniqueViolation("receipts_nk".into()))
        else {
            panic!("unique violation must be terminal");
        };
        assert_eq!(d.code.as_deref(), Some("unique-violation"));
        assert_eq!(d.data.unwrap()["constraint"], "receipts_nk");
    }

    /// Every SDK result-cell variant projects onto the retained row shaper.
    #[test]
    fn every_pg_value_projects_to_sql_value() {
        let all = [
            (PgValue::Null, SqlValue::Null),
            (PgValue::Bool(true), SqlValue::Bool(true)),
            (PgValue::Int32(1), SqlValue::Int32(1)),
            (PgValue::Int64(2), SqlValue::Int64(2)),
            (PgValue::Float64(0.5), SqlValue::Float64(0.5)),
            (PgValue::Text("t".into()), SqlValue::Text("t".into())),
            (PgValue::Bytes(vec![1]), SqlValue::Bytes(vec![1])),
            (
                PgValue::Numeric("12.50".into()),
                SqlValue::Numeric("12.50".into()),
            ),
            (
                PgValue::Timestamptz("2026-07-12T00:00:00Z".into()),
                SqlValue::Timestamptz("2026-07-12T00:00:00Z".into()),
            ),
            (PgValue::Json("{}".into()), SqlValue::Json("{}".into())),
            (
                PgValue::Uuid("00000000-0000-0000-0000-000000000000".into()),
                SqlValue::Uuid("00000000-0000-0000-0000-000000000000".into()),
            ),
        ];
        for (wire, expected) in all {
            assert_eq!(pg_to_api(&wire), expected);
        }
    }
}
