//! The capability facade — everything a node may do to the outside world flows
//! through [`NodeCtx`], which the runner implements over its real host imports
//! (`wasi:http`, `wamn:postgres`) and a test implements over fixtures. The
//! request/response/value types are 1:1 mirrors of the host WIT shapes (the
//! `wamn_api::SqlValue` pattern) so the runner's glue is a trivial `match`.
//!
//! Capability access is POLICY-GATED at dispatch time: the standard library's
//! capability table declares what each node type may use, and the runner grants
//! a set per dispatch — an undeclared or ungranted call fails with
//! `NotGranted`, never silently succeeds (docs/platform-plan.md 5.3).

use serde_json::Value;

/// A capability a node type may declare and a runner may grant. The
/// dispatch-time policy table maps node types to the capabilities they need;
/// the runner refuses a dispatch whose declared set is not covered by the
/// granted set, and the gated context refuses undeclared calls outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Outbound HTTP via the runner's `wasi:http` import (still subject to the
    /// host's `allowedHosts` policy underneath).
    HttpEgress,
    /// Catalog-derived Postgres access via the `wamn:postgres` plugin, under
    /// the tenant claim + RLS floor.
    Postgres,
    /// Author-written SQL through the same plugin path (D8): per-project
    /// permission flag, DEFAULT OFF — granted only when the project enables it
    /// (enablement is gated on the dedicated user-SQL role, wamn-1nd).
    RawSql,
}

/// Everything the runner knows that a node execution may need. Mirrors the
/// frozen WIT `run-context` (`docs/wamn-node.wit`, 0.1.0) with `config`
/// pre-parsed to JSON. Deliberately contains NO secrets — credentials resolve
/// lazily (5.9).
#[derive(Debug, Clone, Copy)]
pub struct RunContext<'a> {
    /// Unique id of this flow run (stable across retries of any node).
    pub run_id: &'a str,
    pub flow_id: &'a str,
    pub flow_version: u32,
    /// The node instance id within the flow graph.
    pub node_id: &'a str,
    /// 0 on first execution, incremented per retry.
    pub attempt: u32,
    /// Runner-generated, stable across retries of this node in this run.
    /// Forward to external systems supporting idempotency headers.
    pub idempotency_key: &'a str,
    /// Remaining execution budget in ms; lets well-behaved nodes set client
    /// timeouts and fail gracefully before the host's hard epoch deadline.
    pub deadline_ms: Option<u64>,
    /// W3C trace context. Present once the host tracing plumbing (9.2) is
    /// wired and a trace is active; nodes making outbound calls MUST
    /// propagate it when present.
    pub traceparent: Option<&'a str>,
    pub tracestate: Option<&'a str>,
    /// Node configuration (already parsed; the flow graph carries it as JSON).
    pub config: &'a Value,
}

impl RunContext<'_> {
    /// W3C trace headers to propagate on an outbound request: `traceparent`
    /// (and `tracestate` when present). Empty when no trace is active.
    ///
    /// This is the SDK propagation helper (9.2). The host also *stamps*
    /// outbound `wasi:http` calls, so trace continuity holds even for nodes
    /// that ignore this; forwarding it explicitly keeps the header present on
    /// the node's own request before it reaches the host handler.
    pub fn trace_headers(&self) -> Vec<(String, String)> {
        let mut hs = Vec::new();
        if let Some(tp) = self.traceparent {
            hs.push(("traceparent".to_string(), tp.to_string()));
            // `tracestate` is only valid alongside `traceparent` (W3C), so it
            // rides inside this arm.
            if let Some(ts) = self.tracestate {
                hs.push(("tracestate".to_string(), ts.to_string()));
            }
        }
        hs
    }

    /// Append this run's trace headers to `headers`, skipping any key already
    /// present (case-insensitive) so an explicit config header wins.
    pub fn apply_trace_context(&self, headers: &mut Vec<(String, String)>) {
        for (k, v) in self.trace_headers() {
            if !headers.iter().any(|(hk, _)| hk.eq_ignore_ascii_case(&k)) {
                headers.push((k, v));
            }
        }
    }
}

/// An outbound HTTP request a node asks the runner to make.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HttpRequest {
    /// Uppercase HTTP method, e.g. `"GET"`, `"POST"`.
    pub method: String,
    /// Absolute `http://` / `https://` URL.
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Request body bytes; `None` sends no body.
    pub body: Option<Vec<u8>>,
}

/// The response to an [`HttpRequest`].
#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Why an HTTP capability call failed **before** an HTTP status existed. The
/// node classifies these into the error taxonomy mechanically.
#[derive(Debug, Clone, PartialEq)]
pub enum HttpCapError {
    /// The policy table / grant set does not allow this node HTTP egress.
    NotGranted,
    /// The host refused the egress (e.g. `allowedHosts`); permanent policy.
    Denied,
    /// The URL could not be parsed / the request could not be built.
    BadRequest(String),
    /// Connection / transport failure with no response (transient).
    Transport(String),
}

/// A single bound parameter or result cell. Variants match the `wamn:postgres`
/// `sql-value` cases exactly (the third mirror alongside the guest binding and
/// `wamn_api::SqlValue`); the runner's conversion is a trivial `match`.
#[derive(Debug, Clone, PartialEq)]
pub enum PgValue {
    Null,
    Bool(bool),
    Int32(i32),
    Int64(i64),
    Float64(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// Exact decimal as a canonical string, e.g. `"12.50"` (the no-float rule).
    Numeric(String),
    /// RFC 3339 timestamp string.
    Timestamptz(String),
    /// A JSON document string (a `jsonb` column).
    Json(String),
    /// Canonical UUID string.
    Uuid(String),
}

/// A query result: projected column names plus row cells in the same order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PgRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<PgValue>>,
}

/// Why a Postgres capability call failed. Mirrors the `wamn:postgres`
/// `pg-error` taxonomy plus `NotGranted`; the node maps these into the node
/// error taxonomy mechanically (see `wamn-nodes`' classification).
#[derive(Debug, Clone, PartialEq)]
pub enum PgCapError {
    /// The policy table / grant set does not allow this node the call.
    NotGranted,
    SerializationFailure,
    ConnectionUnavailable,
    StatementTimeout,
    RowLimitExceeded(u64),
    UniqueViolation(String),
    ForeignKeyViolation(String),
    CheckViolation(String),
    PermissionDenied,
    QueryError {
        code: String,
        message: String,
    },
}

/// Why a credential resolution failed. Mirrors the frozen `wamn:node`
/// `credential-error` (docs/wamn-node.wit, the 5.9 vault) plus the
/// facade-level `NotGranted`; the node maps these into the error taxonomy
/// mechanically (unavailable is retryable at the flow level, per the WIT
/// annotation; not-found is config-shaped and terminal).
#[derive(Debug, Clone, PartialEq)]
pub enum CredentialCapError {
    /// No credential is in this node execution's context — the node declared
    /// none in the flow (`node.credential`), or the vault refused the grant.
    NotGranted,
    /// The declared name is unknown in this project's vault.
    NotFound,
    /// The backing secret store is unavailable (retryable at flow level).
    Unavailable,
}

/// The runner-implemented capability surface. Every node effect flows through
/// here — which is what lets the test host swap fixtures for the world and the
/// policy table refuse what a node type did not declare.
pub trait NodeCtx {
    /// Make one outbound HTTP request.
    fn http(&mut self, req: &HttpRequest) -> Result<HttpResponse, HttpCapError>;

    /// Run a statement that returns rows (`SELECT`, `... RETURNING`).
    fn pg_query(&mut self, sql: &str, params: &[PgValue]) -> Result<PgRows, PgCapError>;

    /// Run a statement and return the affected-row count.
    fn pg_execute(&mut self, sql: &str, params: &[PgValue]) -> Result<u64, PgCapError>;

    /// The project's published catalog snapshot (the `wamn_catalog` document),
    /// for catalog-derived nodes. Loaded through the Postgres capability, so
    /// it is gated by [`Capability::Postgres`].
    fn catalog_json(&mut self) -> Result<String, PgCapError>;

    /// Whether the project's raw-SQL permission flag (D8) is ON. Default OFF;
    /// the runner resolves it from host-injected project config.
    fn raw_sql_enabled(&self) -> bool {
        false
    }

    /// The secret material of the credential this node DECLARED in the flow
    /// (`node.credential`, resolved by the vault — 5.9). Deliberately takes no
    /// name: a node can only read its OWN declared credential, so the secret
    /// is scoped to the executing node's context STRUCTURALLY (a sibling node
    /// never observes it). The runner resolves the declared name through the
    /// frozen `wamn:node` `credentials.get` host import lazily — the secret
    /// never rides flow data, node config, or [`RunContext`].
    ///
    /// Fail-closed default: an implementation without a vault grants nothing.
    fn credential(&mut self) -> Result<String, CredentialCapError> {
        Err(CredentialCapError::NotGranted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx<'a>(tp: Option<&'a str>, ts: Option<&'a str>, config: &'a Value) -> RunContext<'a> {
        RunContext {
            run_id: "r1",
            flow_id: "f1",
            flow_version: 1,
            node_id: "n1",
            attempt: 0,
            idempotency_key: "r1:n1",
            deadline_ms: None,
            traceparent: tp,
            tracestate: ts,
            config,
        }
    }

    #[test]
    fn trace_headers_present_carry_traceparent_and_tracestate() {
        let cfg = json!({});
        let c = ctx(Some("00-abc-def-01"), Some("vendor=1"), &cfg);
        assert_eq!(
            c.trace_headers(),
            vec![
                ("traceparent".to_string(), "00-abc-def-01".to_string()),
                ("tracestate".to_string(), "vendor=1".to_string()),
            ]
        );
    }

    #[test]
    fn trace_headers_absent_when_no_traceparent() {
        // `tracestate` without `traceparent` is invalid W3C, so it is dropped.
        let cfg = json!({});
        assert!(ctx(None, Some("vendor=1"), &cfg).trace_headers().is_empty());
    }

    #[test]
    fn apply_trace_context_appends_when_absent() {
        let cfg = json!({});
        let c = ctx(Some("00-abc-def-01"), None, &cfg);
        let mut headers = vec![("x-token".to_string(), "t".to_string())];
        c.apply_trace_context(&mut headers);
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "traceparent" && v == "00-abc-def-01")
        );
    }

    #[test]
    fn apply_trace_context_does_not_override_explicit_header() {
        let cfg = json!({});
        let c = ctx(Some("00-host-injected-01"), None, &cfg);
        // A caller set `traceparent` explicitly (any case) — it must win.
        let mut headers = vec![("TraceParent".to_string(), "00-explicit-01".to_string())];
        c.apply_trace_context(&mut headers);
        let tps: Vec<_> = headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("traceparent"))
            .collect();
        assert_eq!(tps.len(), 1);
        assert_eq!(tps[0].1, "00-explicit-01");
    }
}
