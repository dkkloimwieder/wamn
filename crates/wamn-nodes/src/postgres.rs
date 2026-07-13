//! The two Postgres nodes (D8, wamn-r13):
//!
//! - **`postgres`** — catalog-derived entity operations, the UNFLAGGED default.
//!   Ops compile through the SAME audited surface the generated REST gateway
//!   uses (`wamn_api::Router`, 4.1): identifiers are catalog-allowlisted +
//!   quoted, values are ALWAYS `$n` params, `tenant_id` on create is injected
//!   server-side, and the RLS floor does isolation underneath.
//! - **`postgres-query`** — author-written SQL, values still bound as `$n`
//!   params, behind the per-project `RawSql` capability (DEFAULT OFF; the
//!   dispatch check refuses it before this node runs — enablement for real
//!   projects is gated on the dedicated user-SQL role, wamn-1nd).
//!
//! Both classify `wamn:postgres` failures MECHANICALLY per the frozen 0.1 WIT
//! annotation (`docs/wamn-postgres.wit`): serialization-failure /
//! connection-unavailable / statement-timeout → retryable; the rest terminal.

use serde_json::{Map, Value, json};
use wamn_api::{ApiError, Catalog, Method, PlanKind, Router, SqlValue, shape_rows};
use wamn_node_sdk::{
    Capability, Emission, ErrorDetail, Node, NodeCtx, NodeError, PgCapError, PgValue, RunContext,
};

use crate::expr::{config_str, eval_to_value};
use crate::template::expand;

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

/// `wamn_api::SqlValue` → SDK `PgValue` (1:1 WIT mirrors on both sides).
pub(crate) fn api_to_pg(v: &SqlValue) -> PgValue {
    match v {
        SqlValue::Null => PgValue::Null,
        SqlValue::Bool(b) => PgValue::Bool(*b),
        SqlValue::Int32(n) => PgValue::Int32(*n),
        SqlValue::Int64(n) => PgValue::Int64(*n),
        SqlValue::Float64(f) => PgValue::Float64(*f),
        SqlValue::Text(s) => PgValue::Text(s.clone()),
        SqlValue::Bytes(b) => PgValue::Bytes(b.clone()),
        SqlValue::Numeric(s) => PgValue::Numeric(s.clone()),
        SqlValue::Timestamptz(s) => PgValue::Timestamptz(s.clone()),
        SqlValue::Json(s) => PgValue::Json(s.clone()),
        SqlValue::Uuid(s) => PgValue::Uuid(s.clone()),
    }
}

/// SDK `PgValue` → `wamn_api::SqlValue` (for response shaping).
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
// postgres — catalog-derived entity ops
// ---------------------------------------------------------------------------

/// Config:
/// ```jsonc
/// {
///   "entity": "receipts",
///   "op": "create" | "get" | "update" | "delete" | "list",
///   "id": "receipt_id",             // get/update/delete: jmespath over the
///                                   // input (default "id")
///   "body": "@",                    // create/update: jmespath selecting the
///                                   // field object (default the whole input;
///                                   // managed id/tenant_id keys are stripped)
///   "filters": {"status": "open"},  // list: field -> value ({{...}} templated,
///                                   // PostgREST-ish "op." prefixes allowed)
///   "sort": "-received_at",         // list
///   "limit": 50, "offset": 0        // list
/// }
/// ```
/// Payloads: create/get/update → the (returned) row object; delete →
/// `{"deleted": true, "id": ...}`; list → the row array. A missing row is
/// `Terminal("not-found")` — routable down the error edge.
pub(crate) struct PostgresEntity;

impl Node for PostgresEntity {
    fn capabilities(&self) -> &'static [Capability] {
        &[Capability::Postgres]
    }

    fn run(
        &self,
        ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        let config = run.config;
        let entity = config_str(config, "entity")?;
        let op = config_str(config, "op")?;

        let raw_catalog = ctx.catalog_json().map_err(classify_pg)?;
        let catalog = Catalog::from_json(&raw_catalog).map_err(|e| {
            NodeError::Terminal(ErrorDetail::coded(
                "catalog-invalid",
                format!("the project catalog snapshot did not parse: {e}"),
            ))
        })?;
        let router = Router::new(&catalog);

        let base = format!("/api/rest/{entity}");
        let plan = match op {
            "create" => {
                let body = body_from(config, input)?;
                router.compile(Method::Post, &base, &[], Some(&body))
            }
            "get" => {
                let id = id_from(config, input)?;
                router.compile(Method::Get, &format!("{base}/{id}"), &[], None)
            }
            "update" => {
                let id = id_from(config, input)?;
                let body = body_from(config, input)?;
                router.compile(Method::Patch, &format!("{base}/{id}"), &[], Some(&body))
            }
            "delete" => {
                let id = id_from(config, input)?;
                router.compile(Method::Delete, &format!("{base}/{id}"), &[], None)
            }
            "list" => {
                let query = list_query(config, input)?;
                router.compile(Method::Get, &base, &query, None)
            }
            other => {
                return Err(NodeError::Terminal(ErrorDetail::coded(
                    "invalid-config",
                    format!("unknown postgres op {other:?}"),
                )));
            }
        }
        .map_err(classify_api)?;

        let params: Vec<PgValue> = plan.query.params.iter().map(api_to_pg).collect();
        let rows = ctx
            .pg_query(&plan.query.sql, &params)
            .map_err(classify_pg)?;
        let api_rows: Vec<Vec<SqlValue>> = rows
            .rows
            .iter()
            .map(|r| r.iter().map(pg_to_api).collect())
            .collect();
        let shaped = shape_rows(&plan.query.columns, &api_rows);

        let payload = match plan.kind {
            PlanKind::List => Value::Array(shaped),
            PlanKind::GetOne | PlanKind::CreateOne | PlanKind::UpdateOne => {
                shaped.into_iter().next().ok_or_else(not_found)?
            }
            PlanKind::DeleteOne => {
                let row = shaped.into_iter().next().ok_or_else(not_found)?;
                let mut out = Map::new();
                out.insert("deleted".into(), Value::Bool(true));
                if let Some(id) = row.get("id") {
                    out.insert("id".into(), id.clone());
                }
                Value::Object(out)
            }
        };
        Ok(Emission::main(payload))
    }
}

fn not_found() -> NodeError {
    NodeError::Terminal(ErrorDetail::coded("not-found", "no such row"))
}

/// `wamn_api` compile refusal → taxonomy: value/payload faults are the INPUT's
/// (`invalid-input`, never retried, distinct in run history); everything else
/// names a config/flow bug (`terminal`).
pub(crate) fn classify_api(e: ApiError) -> NodeError {
    let detail = ErrorDetail {
        message: e.message(),
        code: Some(e.code().to_string()),
        data: None,
    };
    match e {
        ApiError::InvalidValue { .. } | ApiError::PayloadRequired => {
            NodeError::InvalidInput(detail)
        }
        _ => NodeError::Terminal(detail),
    }
}

/// The row id for get/update/delete: config `"id"` is a jmespath over the
/// input (default the input's `id` key). Must resolve to a string.
fn id_from(config: &Value, input: &Value) -> Result<String, NodeError> {
    let expr = config.get("id").and_then(Value::as_str).unwrap_or("id");
    match eval_to_value(expr, input)? {
        Value::String(s) if !s.is_empty() => Ok(s),
        other => Err(NodeError::InvalidInput(ErrorDetail::coded(
            "missing-id",
            format!("id expression {expr:?} must yield a string id, got {other}"),
        ))),
    }
}

/// The field object for create/update: config `"body"` is a jmespath over the
/// input (default `@`, the whole input). Managed `id`/`tenant_id` keys are
/// stripped — the platform owns them (the 3.2 floor injects both).
fn body_from(config: &Value, input: &Value) -> Result<Value, NodeError> {
    let expr = config.get("body").and_then(Value::as_str).unwrap_or("@");
    match eval_to_value(expr, input)? {
        Value::Object(mut m) => {
            m.remove("id");
            m.remove("tenant_id");
            Ok(Value::Object(m))
        }
        other => Err(NodeError::InvalidInput(ErrorDetail::coded(
            "invalid-body",
            format!("body expression {expr:?} must yield an object, got {other}"),
        ))),
    }
}

/// The list op's query pairs: templated filters + sort/limit/offset.
fn list_query(config: &Value, input: &Value) -> Result<Vec<(String, String)>, NodeError> {
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(filters) = config.get("filters") {
        let obj = filters.as_object().ok_or_else(|| {
            NodeError::Terminal(ErrorDetail::coded(
                "invalid-config",
                "postgres list \"filters\" must be an object",
            ))
        })?;
        for (field, v) in obj {
            let raw = match v {
                Value::String(s) => expand(s, input)?,
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                other => {
                    return Err(NodeError::Terminal(ErrorDetail::coded(
                        "invalid-config",
                        format!("filter {field:?} must be a scalar, got {other}"),
                    )));
                }
            };
            query.push((field.clone(), raw));
        }
    }
    if let Some(sort) = config.get("sort").and_then(Value::as_str) {
        query.push(("sort".into(), sort.to_string()));
    }
    for key in ["limit", "offset"] {
        if let Some(n) = config.get(key).and_then(Value::as_u64) {
            query.push((key.into(), n.to_string()));
        }
    }
    Ok(query)
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
                params.push(value_to_pg(eval_to_value(expr, input)?));
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
    /// annotation (docs/wamn-postgres.wit): serialization-failure /
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

    /// `wamn_api` refusals split by fault: value/payload → invalid-input (the
    /// caller's data), everything else → terminal (a flow/config bug).
    #[test]
    fn api_errors_split_input_faults_from_config_bugs() {
        let input_faults = [
            ApiError::InvalidValue {
                field: "quantity".into(),
                message: "not an exact decimal".into(),
            },
            ApiError::PayloadRequired,
        ];
        for e in input_faults {
            assert!(
                matches!(classify_api(e.clone()), NodeError::InvalidInput(_)),
                "{e:?} must be invalid-input"
            );
        }
        let config_bugs = [
            ApiError::UnknownEntity("nope".into()),
            ApiError::UnknownField {
                entity: "receipts".into(),
                field: "bogus".into(),
            },
            ApiError::MethodNotAllowed,
        ];
        for e in config_bugs {
            assert!(
                matches!(classify_api(e.clone()), NodeError::Terminal(_)),
                "{e:?} must be terminal"
            );
        }
    }

    /// The value mirrors are 1:1 in both directions.
    #[test]
    fn value_mirrors_round_trip() {
        let all = [
            SqlValue::Null,
            SqlValue::Bool(true),
            SqlValue::Int32(1),
            SqlValue::Int64(2),
            SqlValue::Float64(0.5),
            SqlValue::Text("t".into()),
            SqlValue::Bytes(vec![1]),
            SqlValue::Numeric("12.50".into()),
            SqlValue::Timestamptz("2026-07-12T00:00:00Z".into()),
            SqlValue::Json("{}".into()),
            SqlValue::Uuid("00000000-0000-0000-0000-000000000000".into()),
        ];
        for v in all {
            assert_eq!(pg_to_api(&api_to_pg(&v)), v);
        }
    }
}
