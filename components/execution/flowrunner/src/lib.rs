//! Single-shot flow-runner component.
//!
//! The public world has one product operation, `run`. Execution remains
//! fail-closed until the immutable plan supply, frame stack, runtime budgets,
//! and effect activation land in their owning changes. This component retains
//! the standard-node capability adapter, including the self-describing trusted
//! HTTP-effect context consumed by that later execution path.

wit_bindgen::generate!({
    world: "flowrunner",
    path: "wit",
    generate_all,
});

use wamn_flow::node_contract::{self as sdk};

use wamn::postgres::client;
use wamn::postgres::types::{PgError, SqlValue};
use wamn::runner::http_effect;

struct Component;
export!(Component);

/// Trusted identity facts passed with one HTTP effect attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpEffectContext {
    pub version: String,
    pub run_id: String,
    pub root_plan_hash: String,
    pub current_plan_hash: String,
    pub frame_id: u64,
    pub local_node_id: String,
    pub occurrence: u32,
    pub source_artifact_hash: String,
    pub requirement_name: String,
}

/// The standard-node capability facade over this component's real imports.
#[derive(Default)]
pub struct CapsCtx {
    /// Whether the `RawSql` capability is granted.
    pub raw_sql: bool,
    /// Complete claims for this single effect attempt.
    pub http_effect: Option<HttpEffectContext>,
}

impl sdk::NodeCtx for CapsCtx {
    fn http(&mut self, request: &sdk::HttpRequest) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
        trusted_http_effect(self.http_effect.as_ref(), request)
    }

    fn pg_query(
        &mut self,
        sql: &str,
        params: &[sdk::PgValue],
    ) -> Result<sdk::PgRows, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        let result = client::query(sql, &params).map_err(wit_err_to_sdk)?;
        Ok(sdk::PgRows {
            columns: result
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
            rows: result
                .rows
                .iter()
                .map(|row| row.iter().map(wit_to_sdk).collect())
                .collect(),
        })
    }

    fn pg_execute(&mut self, sql: &str, params: &[sdk::PgValue]) -> Result<u64, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        client::execute(sql, &params).map_err(wit_err_to_sdk)
    }

    fn catalog_json(&mut self) -> Result<String, sdk::PgCapError> {
        let result = client::query("SELECT document::text FROM wamn_catalog LIMIT 1", &[])
            .map_err(wit_err_to_sdk)?;
        match result.rows.first().and_then(|row| row.first()) {
            Some(SqlValue::Text(value)) | Some(SqlValue::Json(value)) => Ok(value.clone()),
            _ => Err(sdk::PgCapError::QueryError {
                code: String::new(),
                message: "no catalog snapshot published for this project".into(),
            }),
        }
    }

    fn raw_sql_enabled(&self) -> bool {
        self.raw_sql
    }
}

fn sdk_to_wit(value: &sdk::PgValue) -> SqlValue {
    match value {
        sdk::PgValue::Null => SqlValue::Null,
        sdk::PgValue::Bool(value) => SqlValue::Boolean(*value),
        sdk::PgValue::Int32(value) => SqlValue::Int32(*value),
        sdk::PgValue::Int64(value) => SqlValue::Int64(*value),
        sdk::PgValue::Float64(value) => SqlValue::Float64(*value),
        sdk::PgValue::Text(value) => SqlValue::Text(value.clone()),
        sdk::PgValue::Bytes(value) => SqlValue::Bytes(value.clone()),
        sdk::PgValue::Numeric(value) => SqlValue::Numeric(value.clone()),
        sdk::PgValue::Timestamptz(value) => SqlValue::Timestamptz(value.clone()),
        sdk::PgValue::Json(value) => SqlValue::Json(value.clone()),
        sdk::PgValue::Uuid(value) => SqlValue::Uuid(value.clone()),
    }
}

fn wit_to_sdk(value: &SqlValue) -> sdk::PgValue {
    match value {
        SqlValue::Null => sdk::PgValue::Null,
        SqlValue::Boolean(value) => sdk::PgValue::Bool(*value),
        SqlValue::Int32(value) => sdk::PgValue::Int32(*value),
        SqlValue::Int64(value) => sdk::PgValue::Int64(*value),
        SqlValue::Float64(value) => sdk::PgValue::Float64(*value),
        SqlValue::Text(value) => sdk::PgValue::Text(value.clone()),
        SqlValue::Bytes(value) => sdk::PgValue::Bytes(value.clone()),
        SqlValue::Numeric(value) => sdk::PgValue::Numeric(value.clone()),
        SqlValue::Timestamptz(value) => sdk::PgValue::Timestamptz(value.clone()),
        SqlValue::Json(value) => sdk::PgValue::Json(value.clone()),
        SqlValue::Uuid(value) => sdk::PgValue::Uuid(value.clone()),
    }
}

fn wit_err_to_sdk(error: PgError) -> sdk::PgCapError {
    match error {
        PgError::SerializationFailure => sdk::PgCapError::SerializationFailure,
        PgError::ConnectionUnavailable => sdk::PgCapError::ConnectionUnavailable,
        PgError::StatementTimeout => sdk::PgCapError::StatementTimeout,
        PgError::RowLimitExceeded(limit) => sdk::PgCapError::RowLimitExceeded(limit),
        PgError::UniqueViolation(constraint) => sdk::PgCapError::UniqueViolation(constraint),
        PgError::ForeignKeyViolation(constraint) => {
            sdk::PgCapError::ForeignKeyViolation(constraint)
        }
        PgError::CheckViolation(constraint) => sdk::PgCapError::CheckViolation(constraint),
        PgError::PermissionDenied => sdk::PgCapError::PermissionDenied,
        PgError::QueryError((code, message)) => sdk::PgCapError::QueryError { code, message },
    }
}

fn trusted_http_effect(
    context: Option<&HttpEffectContext>,
    request: &sdk::HttpRequest,
) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
    let context = context.ok_or(sdk::HttpCapError::NotGranted)?;
    if context.requirement_name != request.requirement {
        return Err(sdk::HttpCapError::BadRequest(
            "request requirement does not match the attempt context".into(),
        ));
    }
    let context = http_effect_context_to_wit(context);
    let request = http_effect::RelativeRequest {
        method: request.method.clone(),
        path_and_query: request.path_and_query.clone(),
        headers: request
            .headers
            .iter()
            .map(|(name, value)| http_effect::Header {
                name: name.clone(),
                value: value.as_bytes().to_vec(),
            })
            .collect(),
        body: request.body.clone(),
    };
    http_effect::send(&context, &request)
        .map(|response| sdk::HttpResponse {
            status: response.status,
            headers: response
                .headers
                .into_iter()
                .map(|header| {
                    (
                        header.name,
                        String::from_utf8_lossy(&header.value).into_owned(),
                    )
                })
                .collect(),
            body: response.body,
        })
        .map_err(|error| match error {
            http_effect::EffectError::InvalidContext
            | http_effect::EffectError::UndeclaredRequirement
            | http_effect::EffectError::NodeNotPermitted
            | http_effect::EffectError::Unbound
            | http_effect::EffectError::InactiveGeneration
            | http_effect::EffectError::Incompatible
            | http_effect::EffectError::AuthorityDenied => sdk::HttpCapError::Denied,
            http_effect::EffectError::CredentialUnavailable => {
                sdk::HttpCapError::Transport("credential unavailable".into())
            }
            http_effect::EffectError::Timeout => sdk::HttpCapError::Transport("timeout".into()),
            http_effect::EffectError::Transport(detail) => sdk::HttpCapError::Transport(detail),
        })
}

fn http_effect_context_to_wit(context: &HttpEffectContext) -> http_effect::InvocationContext {
    http_effect::InvocationContext {
        version: context.version.clone(),
        run_id: context.run_id.clone(),
        root_plan_hash: context.root_plan_hash.clone(),
        current_plan_hash: context.current_plan_hash.clone(),
        frame_id: context.frame_id,
        local_node_id: context.local_node_id.clone(),
        occurrence: context.occurrence,
        source_artifact_hash: context.source_artifact_hash.clone(),
        requirement_name: context.requirement_name.clone(),
    }
}

const EXECUTION_INTERPRETER_REFUSAL: &str =
    "execution refuses until authoritative execution-plan interpretation is installed";

impl Guest for Component {
    fn run(_run_id: String, _payload: String) -> Result<u32, String> {
        Err(EXECUTION_INTERPRETER_REFUSAL.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_refuses_before_database_mutation_until_plan_execution_lands() {
        assert_eq!(
            <Component as Guest>::run("run".into(), "{}".into()),
            Err(EXECUTION_INTERPRETER_REFUSAL.to_string())
        );
    }
}
