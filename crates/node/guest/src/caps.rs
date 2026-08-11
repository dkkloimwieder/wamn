//! The capability-bearing twin of the no-caps scaffolding (SR2): the
//! `wamn_flow::node_contract::NodeCtx` facade a component shell implements over its real
//! imports — `wamn:postgres` for data and the trusted HTTP effect — plus
//! the WIT↔flow-model contract value mirrors both directions.
//! `components/execution/flowrunner` grew the first copy of this glue; this
//! module is where it lives so the next capability-bearing component links it
//! instead of copying it.
//!
//! Feature-gated (`caps`) so the default build stays exactly the zero-import
//! scaffolding: a custom node built on `export_node!` alone must remain
//! physically incapable of I/O (the `world node` claim egressbench pins).
//! The digest-pinned runner component using [`CapsCtx`] imports
//! `wamn:postgres/{types,client}` and `wamn:runner/http-effect` at the versions
//! pinned in `wit-caps/world.wit`; ordinary custom-node worlds are not granted
//! the trusted runner effect.

use wamn_flow::node_contract as sdk;

mod bindings {
    wit_bindgen::generate!({
        world: "caps-node",
        path: "wit-caps",
        generate_all,
    });
}

use bindings::wamn::postgres::client;
use bindings::wamn::postgres::types::{PgError, SqlValue};
use bindings::wamn::runner::http_effect;

/// Identity claims passed with one trusted HTTP effect call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpEffectContext {
    pub version: String,
    pub tenant_id: String,
    pub environment: String,
    pub catalog_id: String,
    pub catalog_version: i32,
    pub run_id: String,
    pub flow_id: String,
    pub flow_version: u32,
    pub artifact_digest: String,
    pub node_id: String,
    pub occurrence: u32,
    pub attempt: u32,
    pub requirement_name: String,
}

/// The component-shell capability facade: dispatch `wamn-standard-nodes`
/// against the flow-model contract over the component's real imports. The D8
/// raw-SQL flag defaults OFF — per-project enablement wiring lands with the
/// user-SQL role split (wamn-1nd).
///
#[derive(Default)]
pub struct CapsCtx {
    /// Whether the `RawSql` capability is granted (D8; default off).
    pub raw_sql: bool,
    /// Complete claims for this single effect attempt. Absent means HTTP is
    /// refused locally without entering the host.
    pub http_effect: Option<HttpEffectContext>,
}

impl sdk::NodeCtx for CapsCtx {
    fn http(&mut self, req: &sdk::HttpRequest) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
        trusted_http_effect(self.http_effect.as_ref(), req)
    }

    fn pg_query(
        &mut self,
        sql: &str,
        params: &[sdk::PgValue],
    ) -> Result<sdk::PgRows, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        let rs = client::query(sql, &params).map_err(wit_err_to_sdk)?;
        Ok(sdk::PgRows {
            columns: rs.columns.iter().map(|c| c.name.clone()).collect(),
            rows: rs
                .rows
                .iter()
                .map(|r| r.iter().map(wit_to_sdk).collect())
                .collect(),
        })
    }

    fn pg_execute(&mut self, sql: &str, params: &[sdk::PgValue]) -> Result<u64, sdk::PgCapError> {
        let params: Vec<SqlValue> = params.iter().map(sdk_to_wit).collect();
        client::execute(sql, &params).map_err(wit_err_to_sdk)
    }

    fn catalog_json(&mut self) -> Result<String, sdk::PgCapError> {
        // The published project snapshot the api-gateway also reads (4.1b);
        // unqualified, resolved through the host-injected search_path.
        let rs = client::query("SELECT document::text FROM wamn_catalog LIMIT 1", &[])
            .map_err(wit_err_to_sdk)?;
        match rs.rows.first().and_then(|r| r.first()) {
            Some(SqlValue::Text(s)) | Some(SqlValue::Json(s)) => Ok(s.clone()),
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

/// Flow-model contract value → binding value (both mirror the WIT `sql-value`).
fn sdk_to_wit(v: &sdk::PgValue) -> SqlValue {
    match v {
        sdk::PgValue::Null => SqlValue::Null,
        sdk::PgValue::Bool(b) => SqlValue::Boolean(*b),
        sdk::PgValue::Int32(n) => SqlValue::Int32(*n),
        sdk::PgValue::Int64(n) => SqlValue::Int64(*n),
        sdk::PgValue::Float64(f) => SqlValue::Float64(*f),
        sdk::PgValue::Text(s) => SqlValue::Text(s.clone()),
        sdk::PgValue::Bytes(b) => SqlValue::Bytes(b.clone()),
        sdk::PgValue::Numeric(s) => SqlValue::Numeric(s.clone()),
        sdk::PgValue::Timestamptz(s) => SqlValue::Timestamptz(s.clone()),
        sdk::PgValue::Json(s) => SqlValue::Json(s.clone()),
        sdk::PgValue::Uuid(s) => SqlValue::Uuid(s.clone()),
    }
}

/// Binding value → flow-model contract value.
fn wit_to_sdk(v: &SqlValue) -> sdk::PgValue {
    match v {
        SqlValue::Null => sdk::PgValue::Null,
        SqlValue::Boolean(b) => sdk::PgValue::Bool(*b),
        SqlValue::Int32(n) => sdk::PgValue::Int32(*n),
        SqlValue::Int64(n) => sdk::PgValue::Int64(*n),
        SqlValue::Float64(f) => sdk::PgValue::Float64(*f),
        SqlValue::Text(s) => sdk::PgValue::Text(s.clone()),
        SqlValue::Bytes(b) => sdk::PgValue::Bytes(b.clone()),
        SqlValue::Numeric(s) => sdk::PgValue::Numeric(s.clone()),
        SqlValue::Timestamptz(s) => sdk::PgValue::Timestamptz(s.clone()),
        SqlValue::Json(s) => sdk::PgValue::Json(s.clone()),
        SqlValue::Uuid(s) => sdk::PgValue::Uuid(s.clone()),
    }
}

/// Binding pg-error → flow-model capability error (the node classifies).
fn wit_err_to_sdk(e: PgError) -> sdk::PgCapError {
    match e {
        PgError::SerializationFailure => sdk::PgCapError::SerializationFailure,
        PgError::ConnectionUnavailable => sdk::PgCapError::ConnectionUnavailable,
        PgError::StatementTimeout => sdk::PgCapError::StatementTimeout,
        PgError::RowLimitExceeded(n) => sdk::PgCapError::RowLimitExceeded(n),
        PgError::UniqueViolation(c) => sdk::PgCapError::UniqueViolation(c),
        PgError::ForeignKeyViolation(c) => sdk::PgCapError::ForeignKeyViolation(c),
        PgError::CheckViolation(c) => sdk::PgCapError::CheckViolation(c),
        PgError::PermissionDenied => sdk::PgCapError::PermissionDenied,
        PgError::QueryError((code, message)) => sdk::PgCapError::QueryError { code, message },
    }
}

fn trusted_http_effect(
    context: Option<&HttpEffectContext>,
    req: &sdk::HttpRequest,
) -> Result<sdk::HttpResponse, sdk::HttpCapError> {
    let context = context.ok_or(sdk::HttpCapError::NotGranted)?;
    if context.requirement_name != req.requirement {
        return Err(sdk::HttpCapError::BadRequest(
            "request requirement does not match the attempt context".into(),
        ));
    }
    let context = http_effect::InvocationContext {
        version: context.version.clone(),
        tenant_id: context.tenant_id.clone(),
        environment: context.environment.clone(),
        catalog_id: context.catalog_id.clone(),
        catalog_version: context.catalog_version,
        run_id: context.run_id.clone(),
        flow_id: context.flow_id.clone(),
        flow_version: context.flow_version,
        artifact_digest: context.artifact_digest.clone(),
        node_id: context.node_id.clone(),
        occurrence: context.occurrence,
        attempt: context.attempt,
        requirement_name: context.requirement_name.clone(),
    };
    let request = http_effect::RelativeRequest {
        method: req.method.clone(),
        path_and_query: req.path_and_query.clone(),
        headers: req
            .headers
            .iter()
            .map(|(name, value)| http_effect::Header {
                name: name.clone(),
                value: value.as_bytes().to_vec(),
            })
            .collect(),
        body: req.body.clone(),
    };
    http_effect::send(&context, &req.requirement, &request)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every sql-value variant survives the contract→WIT→contract round trip —
    /// the two mirrors cannot drift apart variant-for-variant.
    #[test]
    fn sql_value_mirrors_round_trip() {
        let vals = [
            sdk::PgValue::Null,
            sdk::PgValue::Bool(true),
            sdk::PgValue::Int32(-7),
            sdk::PgValue::Int64(1 << 40),
            sdk::PgValue::Float64(2.5),
            sdk::PgValue::Text("t".into()),
            sdk::PgValue::Bytes(vec![1, 2]),
            sdk::PgValue::Numeric("12.50".into()),
            sdk::PgValue::Timestamptz("2026-01-01T00:00:00Z".into()),
            sdk::PgValue::Json("{\"a\":1}".into()),
            sdk::PgValue::Uuid("a0000000-0000-0000-0000-000000000001".into()),
        ];
        for v in &vals {
            assert_eq!(&wit_to_sdk(&sdk_to_wit(v)), v);
        }
    }

    /// The pg-error map is 1:1 (the node classifies; this glue never does).
    #[test]
    fn pg_error_maps_variant_for_variant() {
        assert!(matches!(
            wit_err_to_sdk(PgError::SerializationFailure),
            sdk::PgCapError::SerializationFailure
        ));
        assert!(matches!(
            wit_err_to_sdk(PgError::StatementTimeout),
            sdk::PgCapError::StatementTimeout
        ));
        assert!(matches!(
            wit_err_to_sdk(PgError::UniqueViolation("c".into())),
            sdk::PgCapError::UniqueViolation(c) if c == "c"
        ));
        assert!(matches!(
            wit_err_to_sdk(PgError::QueryError(("22P02".into(), "m".into()))),
            sdk::PgCapError::QueryError { code, .. } if code == "22P02"
        ));
    }
}
