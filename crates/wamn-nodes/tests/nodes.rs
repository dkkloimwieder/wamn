//! Standard node library gates (5.3): every node against a mock capability
//! facade — behavior, config validation, the MECHANICAL taxonomy maps, the
//! dispatch-time policy table, and the injection-safety of both Postgres
//! nodes. No DB, no network, no cluster (the wamn-api split).

use std::collections::VecDeque;

use serde_json::{Value, json};
use wamn_nodes::{
    Capability, CredentialCapError, HttpCapError, HttpRequest, HttpResponse, NodeCtx, NodeError,
    PgCapError, PgRows, PgValue, RunContext, dispatch, granted_for, required_capabilities, respond,
};

// ---------------------------------------------------------------------------
// Mock capability facade
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Mock {
    /// Every pg statement the node ran, with its bound params.
    pg_calls: Vec<(String, Vec<PgValue>)>,
    /// Scripted pg_query results, popped per call.
    pg_results: VecDeque<Result<PgRows, PgCapError>>,
    execute_result: u64,
    /// Every outbound request the node made.
    http_calls: Vec<HttpRequest>,
    /// Scripted http results, popped per call.
    http_results: VecDeque<Result<HttpResponse, HttpCapError>>,
    catalog: Option<String>,
    raw_sql: bool,
    /// The vault's answer for this node's declared credential. `None` mirrors
    /// the trait's fail-closed default (`NotGranted` — no credential is in
    /// this node's context).
    credential: Option<Result<String, CredentialCapError>>,
}

impl NodeCtx for Mock {
    fn http(&mut self, req: &HttpRequest) -> Result<HttpResponse, HttpCapError> {
        self.http_calls.push(req.clone());
        self.http_results.pop_front().expect("scripted http result")
    }
    fn pg_query(&mut self, sql: &str, params: &[PgValue]) -> Result<PgRows, PgCapError> {
        self.pg_calls.push((sql.to_string(), params.to_vec()));
        self.pg_results.pop_front().expect("scripted pg result")
    }
    fn pg_execute(&mut self, sql: &str, params: &[PgValue]) -> Result<u64, PgCapError> {
        self.pg_calls.push((sql.to_string(), params.to_vec()));
        Ok(self.execute_result)
    }
    fn catalog_json(&mut self) -> Result<String, PgCapError> {
        Ok(self.catalog.clone().expect("test provides a catalog"))
    }
    fn raw_sql_enabled(&self) -> bool {
        self.raw_sql
    }
    fn credential(&mut self) -> Result<String, CredentialCapError> {
        self.credential
            .clone()
            .unwrap_or(Err(CredentialCapError::NotGranted))
    }
}

fn run<'a>(config: &'a Value) -> RunContext<'a> {
    RunContext {
        run_id: "r-1",
        flow_id: "f",
        flow_version: 1,
        node_id: "n",
        attempt: 0,
        idempotency_key: "r-1:n",
        deadline_ms: None,
        traceparent: None,
        tracestate: None,
        config,
    }
}

/// Dispatch under the default grants (D8 flag OFF).
fn go(
    node_type: &str,
    mock: &mut Mock,
    config: &Value,
    input: &Value,
) -> Result<wamn_nodes::Emission, NodeError> {
    dispatch(
        node_type,
        granted_for(mock.raw_sql),
        mock,
        &run(config),
        input,
    )
}

fn ok_http(
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<HttpResponse, HttpCapError> {
    Ok(HttpResponse {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.as_bytes().to_vec(),
    })
}

fn terminal_code(e: &NodeError) -> &str {
    match e {
        NodeError::Terminal(d) => d.code.as_deref().unwrap_or(""),
        other => panic!("expected Terminal, got {other:?}"),
    }
}

/// The test catalog: a suppliers entity with a required text field, an
/// optional exact-decimal field, and a receipts entity referencing it.
fn catalog_json() -> String {
    json!({
        "schema-version": "0.1",
        "catalog-id": "nodes-test",
        "version": 1,
        "entities": [
            {"id": "e-suppliers", "name": "suppliers", "fields": [
                {"id": "f-name", "name": "name", "type": {"kind": "text"}},
                {"id": "f-cost", "name": "standard_cost", "nullable": true,
                 "type": {"kind": "numeric", "precision": 10, "scale": 2}}
            ]},
            {"id": "e-receipts", "name": "receipts", "fields": [
                {"id": "f-rno", "name": "receipt_no", "type": {"kind": "text"}},
                {"id": "f-sup", "name": "supplier_id",
                 "type": {"kind": "reference", "entity": "e-suppliers"}}
            ]}
        ]
    })
    .to_string()
}

const UUID_1: &str = "00000000-0000-0000-0000-000000000001";

// ---------------------------------------------------------------------------
// transform / conditional (JMESPath)
// ---------------------------------------------------------------------------

#[test]
fn transform_reshapes_with_jmespath() {
    let mut mock = Mock::default();
    let config = json!({"expression": "{out: a.b, qty: q}"});
    let input = json!({"a": {"b": "x"}, "q": "12.50"});
    let em = go("transform", &mut mock, &config, &input).unwrap();
    assert_eq!(em.port, "main");
    // The exact-decimal STRING passes through untouched — the no-float rule
    // holds through a transform by construction (JMESPath has no arithmetic).
    assert_eq!(em.payload, json!({"out": "x", "qty": "12.50"}));
}

/// Pins the off-the-shelf engine's number handling: integers ride
/// serde_json::Number exactly (no f64 round trip). If a jmespath upgrade ever
/// breaks this, this test fails loudly.
#[test]
fn transform_preserves_big_integers_exactly() {
    let mut mock = Mock::default();
    let config = json!({"expression": "{v: n}"});
    let input = json!({"n": 9007199254740993i64});
    let em = go("transform", &mut mock, &config, &input).unwrap();
    assert_eq!(em.payload, json!({"v": 9007199254740993i64}));
}

#[test]
fn transform_config_faults_are_terminal() {
    let mut mock = Mock::default();
    let e = go("transform", &mut mock, &json!({}), &json!({})).unwrap_err();
    assert_eq!(terminal_code(&e), "invalid-config");
    let e = go(
        "transform",
        &mut mock,
        &json!({"expression": "{unclosed"}),
        &json!({}),
    )
    .unwrap_err();
    assert_eq!(terminal_code(&e), "invalid-expression");
}

#[test]
fn transform_missing_path_yields_null_not_an_error() {
    let mut mock = Mock::default();
    let em = go(
        "transform",
        &mut mock,
        &json!({"expression": "nope.deep"}),
        &json!({"a": 1}),
    )
    .unwrap();
    assert_eq!(em.payload, Value::Null);
}

#[test]
fn conditional_branches_by_jmespath_truthiness() {
    let mut mock = Mock::default();
    let config = json!({"expression": "qty > `10`"});
    let hot = go("conditional", &mut mock, &config, &json!({"qty": 12})).unwrap();
    assert_eq!(hot.port, "true");
    assert_eq!(
        hot.payload,
        json!({"qty": 12}),
        "payload passes through unchanged"
    );
    let cold = go("conditional", &mut mock, &config, &json!({"qty": 5})).unwrap();
    assert_eq!(cold.port, "false");
    // JMESPath truthiness: an empty array is falsy.
    let empty = go(
        "conditional",
        &mut mock,
        &json!({"expression": "holds"}),
        &json!({"holds": []}),
    )
    .unwrap();
    assert_eq!(empty.port, "false");
}

// ---------------------------------------------------------------------------
// time-shift (deterministic time arithmetic — the F3 cutoff gap)
// ---------------------------------------------------------------------------

/// The F3 shape: shift the cron `fire-at-ms` back 48h into an RFC 3339 cutoff a
/// `timestamptz` filter compares against. 1_700_000_000_000 ms = 2023-11-14
/// 22:13:20Z; − 172_800_000 (48h) = 2023-11-12 22:13:20Z.
#[test]
fn time_shift_computes_an_iso_cutoff() {
    let mut mock = Mock::default();
    let config = json!({
        "base": "\"fire-at-ms\"", "offset-ms": -172_800_000, "format": "iso", "key": "cutoff"
    });
    let input = json!({"trigger": "cron", "fire-at-ms": 1_700_000_000_000i64});
    let em = go("time-shift", &mut mock, &config, &input).unwrap();
    assert_eq!(em.port, "main");
    assert_eq!(em.payload, json!({"cutoff": "2023-11-12T22:13:20.000Z"}));
}

/// MUTANT WITNESS (mutant #1): the offset SIGN is respected — a NEGATIVE offset
/// yields an instant strictly EARLIER than the base, a positive one strictly
/// later. A node that dropped the sign (added `|offset|`) or flipped it would
/// escalate FRESH holds (cutoff in the future); this pins the direction.
#[test]
fn time_shift_offset_sign_is_respected() {
    let mut mock = Mock::default();
    let input = json!({"fire-at-ms": 1_700_000_000_000i64});

    let back = go(
        "time-shift",
        &mut mock,
        &json!({"base": "\"fire-at-ms\"", "offset-ms": -172_800_000, "format": "epoch-ms"}),
        &input,
    )
    .unwrap();
    assert_eq!(
        back.payload,
        json!({"cutoff": 1_699_827_200_000i64}),
        "− subtracts"
    );

    let fwd = go(
        "time-shift",
        &mut mock,
        &json!({"base": "\"fire-at-ms\"", "offset-ms": 172_800_000, "format": "epoch-ms"}),
        &input,
    )
    .unwrap();
    assert_eq!(
        fwd.payload,
        json!({"cutoff": 1_700_172_800_000i64}),
        "+ adds"
    );
}

/// `format`/`key` default to `iso`/`cutoff`; `key` is configurable.
#[test]
fn time_shift_format_and_key_defaults() {
    let mut mock = Mock::default();
    let input = json!({"fire-at-ms": 0});
    // Defaults: iso under "cutoff".
    let em = go(
        "time-shift",
        &mut mock,
        &json!({"base": "\"fire-at-ms\"", "offset-ms": 0}),
        &input,
    )
    .unwrap();
    assert_eq!(em.payload, json!({"cutoff": "1970-01-01T00:00:00.000Z"}));
    // Configurable key.
    let em = go(
        "time-shift",
        &mut mock,
        &json!({"base": "\"fire-at-ms\"", "offset-ms": 1000, "format": "epoch-ms", "key": "before"}),
        &input,
    )
    .unwrap();
    assert_eq!(em.payload, json!({"before": 1000}));
}

/// The base is a JMESPath over runtime INPUT: a missing or non-integer value is
/// the input's fault (invalid-input, never retried) — not a flow-config bug.
#[test]
fn time_shift_bad_base_is_invalid_input() {
    let mut mock = Mock::default();
    let config = json!({"base": "\"fire-at-ms\"", "offset-ms": -1000});
    // Missing path -> JMESPath null -> not an epoch-ms number.
    let e = go("time-shift", &mut mock, &config, &json!({"other": 1})).unwrap_err();
    assert!(
        matches!(&e, NodeError::InvalidInput(d) if d.code.as_deref() == Some("invalid-base")),
        "missing base is invalid-input: {e:?}"
    );
    // Present but a string, not a number.
    let e = go(
        "time-shift",
        &mut mock,
        &config,
        &json!({"fire-at-ms": "soon"}),
    )
    .unwrap_err();
    assert!(
        matches!(&e, NodeError::InvalidInput(_)),
        "non-number base: {e:?}"
    );
}

/// Config faults (missing base / missing offset / unknown format) are terminal
/// flow-authoring bugs.
#[test]
fn time_shift_config_faults_are_terminal() {
    let mut mock = Mock::default();
    let input = json!({"fire-at-ms": 0});
    // Missing base string.
    let e = go("time-shift", &mut mock, &json!({"offset-ms": 0}), &input).unwrap_err();
    assert_eq!(terminal_code(&e), "invalid-config");
    // Missing / non-integer offset-ms.
    let e = go(
        "time-shift",
        &mut mock,
        &json!({"base": "\"fire-at-ms\""}),
        &input,
    )
    .unwrap_err();
    assert_eq!(terminal_code(&e), "invalid-config");
    // Unknown format.
    let e = go(
        "time-shift",
        &mut mock,
        &json!({"base": "\"fire-at-ms\"", "offset-ms": 0, "format": "unix"}),
        &input,
    )
    .unwrap_err();
    assert_eq!(terminal_code(&e), "invalid-config");
}

/// The F3 flow drains stale holds via a STRUCTURAL cycle whose loop state is an
/// in-memory work-list threaded through `conditional` (pass-through) + a
/// `transform` slice — `escalate`/`notify` hang off a dead-end branch and never
/// carry loop state. This pins the exact JMESPath the fixture relies on, so a
/// jmespath upgrade that broke slicing / indexing / array-truthiness would fail
/// here rather than silently at run time.
#[test]
fn f3_cycle_jmespath_surface_holds() {
    let mut mock = Mock::default();
    let rows = json!([{"id": "h1"}, {"id": "h2"}, {"id": "h3"}]);

    // gate: are there rows left? (empty array is falsy)
    let more = go(
        "conditional",
        &mut mock,
        &json!({"expression": "length(@) > `0`"}),
        &rows,
    )
    .unwrap();
    assert_eq!(more.port, "true");
    let none = go(
        "conditional",
        &mut mock,
        &json!({"expression": "length(@) > `0`"}),
        &json!([]),
    )
    .unwrap();
    assert_eq!(none.port, "false");

    // escalate id selects the head row's id.
    let head = go(
        "transform",
        &mut mock,
        &json!({"expression": "[0].id"}),
        &rows,
    )
    .unwrap();
    assert_eq!(head.payload, json!("h1"));

    // advance drops the head; the tail of a single-element list is the empty
    // array (loop terminates), NOT null.
    let tail = go(
        "transform",
        &mut mock,
        &json!({"expression": "[1:]"}),
        &rows,
    )
    .unwrap();
    assert_eq!(tail.payload, json!([{"id": "h2"}, {"id": "h3"}]));
    let done = go(
        "transform",
        &mut mock,
        &json!({"expression": "[1:]"}),
        &json!([{"id": "h1"}]),
    )
    .unwrap();
    assert_eq!(done.payload, json!([]));

    // escalate body is a constant object; notify body reshapes the escalated row.
    let body = go(
        "transform",
        &mut mock,
        &json!({"expression": "{status: 'escalated'}"}),
        &rows,
    )
    .unwrap();
    assert_eq!(body.payload, json!({"status": "escalated"}));
    let row = json!({"id": "h1", "status": "escalated", "opened_at": "2026-01-01T00:00:00.000Z"});
    let notify = go(
        "transform",
        &mut mock,
        &json!({"expression": "{hold: id, status: status, opened_at: opened_at}"}),
        &row,
    )
    .unwrap();
    assert_eq!(
        notify.payload,
        json!({"hold": "h1", "status": "escalated", "opened_at": "2026-01-01T00:00:00.000Z"})
    );
}

// ---------------------------------------------------------------------------
// http-request
// ---------------------------------------------------------------------------

#[test]
fn http_request_templates_url_headers_and_body() {
    let mut mock = Mock::default();
    mock.http_results.push_back(ok_http(
        200,
        &[("content-type", "application/json")],
        r#"{"ok":true}"#,
    ));
    let config = json!({
        "method": "post",
        "url": "http://api.test/r/{{id}}",
        "headers": {"x-token": "{{tok}}"},
        "body": "payload"
    });
    let input = json!({"id": "42", "tok": "T", "payload": {"a": 1}});
    let em = go("http-request", &mut mock, &config, &input).unwrap();

    let req = &mock.http_calls[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.url, "http://api.test/r/42");
    assert!(req.headers.contains(&("x-token".into(), "T".into())));
    assert!(
        req.headers
            .iter()
            .any(|(k, v)| k == "content-type" && v == "application/json"),
        "json body sets content-type"
    );
    assert_eq!(req.body.as_deref(), Some(br#"{"a":1}"#.as_slice()));

    assert_eq!(em.payload["status"], 200);
    assert_eq!(em.payload["body"], json!({"ok": true}));
}

#[test]
fn http_request_null_body_expression_sends_no_body() {
    let mut mock = Mock::default();
    mock.http_results.push_back(ok_http(204, &[], ""));
    let config = json!({"url": "http://api.test/x", "body": "missing.path"});
    let em = go("http-request", &mut mock, &config, &json!({})).unwrap();
    assert_eq!(mock.http_calls[0].method, "GET", "method defaults to GET");
    assert_eq!(mock.http_calls[0].body, None);
    assert_eq!(em.payload["status"], 204);
}

/// 9.2: the SDK http-request node forwards the run's active W3C trace context
/// onto the outbound request it builds.
#[test]
fn http_request_forwards_active_traceparent() {
    let mut mock = Mock::default();
    mock.http_results.push_back(ok_http(200, &[], "{}"));
    let config = json!({"url": "http://api.test/x"});
    let rc = RunContext {
        run_id: "r-1",
        flow_id: "f",
        flow_version: 1,
        node_id: "n",
        attempt: 0,
        idempotency_key: "r-1:n",
        deadline_ms: None,
        traceparent: Some("00-abc-def-01"),
        tracestate: Some("vendor=1"),
        config: &config,
    };
    dispatch(
        "http-request",
        granted_for(false),
        &mut mock,
        &rc,
        &json!({}),
    )
    .unwrap();
    let req = &mock.http_calls[0];
    assert!(
        req.headers
            .iter()
            .any(|(k, v)| k == "traceparent" && v == "00-abc-def-01"),
        "http-request forwards the active traceparent (9.2); got {:?}",
        req.headers
    );
    assert!(
        req.headers
            .iter()
            .any(|(k, v)| k == "tracestate" && v == "vendor=1"),
        "and tracestate alongside it"
    );
}

/// An explicit config `traceparent` header must win over the run's context.
#[test]
fn http_request_explicit_traceparent_header_wins() {
    let mut mock = Mock::default();
    mock.http_results.push_back(ok_http(200, &[], "{}"));
    let config = json!({
        "url": "http://api.test/x",
        "headers": {"traceparent": "00-explicit-01"}
    });
    let rc = RunContext {
        run_id: "r-1",
        flow_id: "f",
        flow_version: 1,
        node_id: "n",
        attempt: 0,
        idempotency_key: "r-1:n",
        deadline_ms: None,
        traceparent: Some("00-host-01"),
        tracestate: None,
        config: &config,
    };
    dispatch(
        "http-request",
        granted_for(false),
        &mut mock,
        &rc,
        &json!({}),
    )
    .unwrap();
    let tps: Vec<_> = mock.http_calls[0]
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("traceparent"))
        .collect();
    assert_eq!(tps.len(), 1, "exactly one traceparent header");
    assert_eq!(tps[0].1, "00-explicit-01", "config header wins");
}

/// 5.9: the node's DECLARED credential resolves through the vault and rides
/// as the `authorization` header by default — the secret exists only in the
/// outbound request, never in config or the emitted payload.
#[test]
fn http_request_sends_the_declared_credential() {
    let mut mock = Mock {
        credential: Some(Ok("Bearer s3cr3t-tok".into())),
        ..Mock::default()
    };
    mock.http_results.push_back(ok_http(200, &[], "{}"));
    let config = json!({"url": "http://notify.test/x"});
    let em = go("http-request", &mut mock, &config, &json!({})).unwrap();
    assert!(
        mock.http_calls[0]
            .headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer s3cr3t-tok"),
        "declared credential sent as authorization; got {:?}",
        mock.http_calls[0].headers
    );
    assert!(
        !em.payload.to_string().contains("s3cr3t-tok"),
        "the secret never enters the emitted payload"
    );
}

/// The header the credential rides in is config-selectable
/// (`credential-header`), and an explicit config header of the same name wins
/// (the trace-context rule).
#[test]
fn http_request_credential_header_is_configurable_and_config_wins() {
    let mut mock = Mock {
        credential: Some(Ok("k-123".into())),
        ..Mock::default()
    };
    mock.http_results.push_back(ok_http(200, &[], "{}"));
    let config = json!({"url": "http://notify.test/x", "credential-header": "x-api-key"});
    go("http-request", &mut mock, &config, &json!({})).unwrap();
    let req = &mock.http_calls[0];
    assert!(
        req.headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "k-123"),
        "credential rides the configured header; got {:?}",
        req.headers
    );
    assert!(
        !req.headers.iter().any(|(k, _)| k == "authorization"),
        "no stray authorization header"
    );

    // An explicit config header of the credential's name wins outright.
    let mut mock = Mock {
        credential: Some(Ok("from-vault".into())),
        ..Mock::default()
    };
    mock.http_results.push_back(ok_http(200, &[], "{}"));
    let config = json!({
        "url": "http://notify.test/x",
        "headers": {"Authorization": "explicit"}
    });
    go("http-request", &mut mock, &config, &json!({})).unwrap();
    let auths: Vec<_> = mock.http_calls[0]
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .collect();
    assert_eq!(auths.len(), 1, "exactly one authorization header");
    assert_eq!(auths[0].1, "explicit", "explicit config header wins");
}

/// A node that declared NO credential proceeds bare (`NotGranted` is the
/// no-credential signal, not an error); vault failures classify mechanically —
/// unknown name is config-shaped (terminal), a down store is retryable — and
/// in both cases nothing leaves the node.
#[test]
fn http_request_credential_errors_classify_mechanically() {
    // No declaration: request proceeds without a credential header.
    let mut mock = Mock::default();
    mock.http_results.push_back(ok_http(200, &[], "{}"));
    let config = json!({"url": "http://notify.test/x"});
    go("http-request", &mut mock, &config, &json!({})).unwrap();
    assert!(
        !mock.http_calls[0]
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization")),
        "no credential declared ⇒ bare request"
    );

    let mut mock = Mock {
        credential: Some(Err(CredentialCapError::NotFound)),
        ..Mock::default()
    };
    let e = go("http-request", &mut mock, &config, &json!({})).unwrap_err();
    assert_eq!(terminal_code(&e), "credential-not-found");
    assert!(mock.http_calls.is_empty(), "nothing left the node");

    let mut mock = Mock {
        credential: Some(Err(CredentialCapError::Unavailable)),
        ..Mock::default()
    };
    let e = go("http-request", &mut mock, &config, &json!({})).unwrap_err();
    assert!(
        matches!(&e, NodeError::Retryable(d) if d.code.as_deref() == Some("credential-unavailable")),
        "a down vault is retryable, got {e:?}"
    );
    assert!(mock.http_calls.is_empty(), "nothing left the node");
}

/// THE mechanical status → taxonomy map (docs/wamn-node.wit): 429 →
/// rate-limited with the source delay + throttle host; 408/5xx → retryable;
/// other 4xx → terminal; transport → retryable; host egress denial → terminal.
#[test]
fn http_statuses_classify_mechanically() {
    let mut mock = Mock::default();
    let config = json!({"url": "http://api.test/x"});

    mock.http_results
        .push_back(ok_http(429, &[("Retry-After", "7")], "slow down"));
    let e = go("http-request", &mut mock, &config, &json!({})).unwrap_err();
    let NodeError::RateLimited(rl) = &e else {
        panic!("429 must be rate-limited, got {e:?}");
    };
    assert_eq!(rl.retry_after_ms, Some(7000), "Retry-After honored");
    assert_eq!(
        rl.target_host.as_deref(),
        Some("api.test"),
        "throttle key host"
    );

    for status in [500u16, 503, 408] {
        mock.http_results.push_back(ok_http(status, &[], "boom"));
        let e = go("http-request", &mut mock, &config, &json!({})).unwrap_err();
        assert!(
            matches!(e, NodeError::Retryable(_)),
            "{status} must be retryable, got {e:?}"
        );
    }

    mock.http_results.push_back(ok_http(404, &[], "nope"));
    let e = go("http-request", &mut mock, &config, &json!({})).unwrap_err();
    assert_eq!(terminal_code(&e), "HTTP_404");

    mock.http_results
        .push_back(Err(HttpCapError::Transport("connection refused".into())));
    let e = go("http-request", &mut mock, &config, &json!({})).unwrap_err();
    assert!(
        matches!(e, NodeError::Retryable(_)),
        "transport is transient"
    );

    mock.http_results.push_back(Err(HttpCapError::Denied));
    let e = go("http-request", &mut mock, &config, &json!({})).unwrap_err();
    assert_eq!(terminal_code(&e), "egress-denied");
}

#[test]
fn http_request_rejects_relative_urls() {
    let mut mock = Mock::default();
    let e = go(
        "http-request",
        &mut mock,
        &json!({"url": "/relative/{{id}}"}),
        &json!({"id": "x"}),
    )
    .unwrap_err();
    assert_eq!(terminal_code(&e), "invalid-config");
    assert!(mock.http_calls.is_empty(), "nothing left the node");
}

// ---------------------------------------------------------------------------
// postgres (catalog-derived entity ops)
// ---------------------------------------------------------------------------

fn one_row(cells: Vec<PgValue>) -> Result<PgRows, PgCapError> {
    Ok(PgRows {
        columns: vec![], // entity ops shape on the COMPILED projection
        rows: vec![cells],
    })
}

#[test]
fn postgres_create_compiles_through_the_audited_surface() {
    let mut mock = Mock {
        catalog: Some(catalog_json()),
        ..Mock::default()
    };
    mock.pg_results.push_back(one_row(vec![
        PgValue::Uuid(UUID_1.into()),
        PgValue::Text("acme".into()),
        PgValue::Numeric("12.50".into()),
    ]));
    let config = json!({"entity": "suppliers", "op": "create"});
    // The managed id key is STRIPPED, not rejected — a prior node's row output
    // can feed a create directly.
    let input = json!({"id": "stripped", "name": "acme", "standard_cost": "12.50"});
    let em = go("postgres", &mut mock, &config, &input).unwrap();

    let (sql, params) = &mock.pg_calls[0];
    assert!(sql.starts_with("INSERT INTO \"suppliers\""), "sql: {sql}");
    assert!(
        sql.contains("current_setting('app.tenant', true)"),
        "tenant is injected server-side: {sql}"
    );
    assert!(sql.contains("RETURNING"));
    assert!(params.contains(&PgValue::Text("acme".into())));
    assert!(
        !params
            .iter()
            .any(|p| p == &PgValue::Text("stripped".into())),
        "managed id never binds"
    );
    assert_eq!(em.payload["name"], "acme");
    assert_eq!(
        em.payload["standard_cost"], "12.50",
        "exact decimal out as a string"
    );
}

#[test]
fn postgres_get_missing_row_is_not_found() {
    let mut mock = Mock {
        catalog: Some(catalog_json()),
        ..Mock::default()
    };
    mock.pg_results.push_back(Ok(PgRows::default()));
    let config = json!({"entity": "suppliers", "op": "get"});
    let e = go("postgres", &mut mock, &config, &json!({"id": UUID_1})).unwrap_err();
    assert_eq!(terminal_code(&e), "not-found");
    let (sql, params) = &mock.pg_calls[0];
    assert!(sql.contains("WHERE \"id\" = $1"), "sql: {sql}");
    assert_eq!(params[0], PgValue::Uuid(UUID_1.into()));
}

/// The injection witness: a hostile templated filter value stays a `$n`
/// param; the SQL text never contains it.
#[test]
fn postgres_list_filter_values_bind_never_splice() {
    let mut mock = Mock {
        catalog: Some(catalog_json()),
        ..Mock::default()
    };
    mock.pg_results.push_back(Ok(PgRows::default()));
    let config = json!({
        "entity": "suppliers", "op": "list",
        "filters": {"name": "{{evil}}"}, "limit": 10
    });
    let hostile = "x' OR '1'='1";
    let em = go("postgres", &mut mock, &config, &json!({"evil": hostile})).unwrap();
    assert_eq!(em.payload, json!([]));
    let (sql, params) = &mock.pg_calls[0];
    assert!(!sql.contains(hostile), "value never splices: {sql}");
    assert!(sql.contains("\"name\" = $1"), "sql: {sql}");
    assert_eq!(params[0], PgValue::Text(hostile.into()));
}

#[test]
fn postgres_delete_reports_the_deleted_row() {
    let mut mock = Mock {
        catalog: Some(catalog_json()),
        ..Mock::default()
    };
    mock.pg_results
        .push_back(one_row(vec![PgValue::Uuid(UUID_1.into())]));
    let config = json!({"entity": "suppliers", "op": "delete"});
    let em = go("postgres", &mut mock, &config, &json!({"id": UUID_1})).unwrap();
    assert_eq!(em.payload["deleted"], true);
    assert_eq!(em.payload["id"], UUID_1);
}

#[test]
fn postgres_config_and_input_faults_classify_apart() {
    let mut mock = Mock {
        catalog: Some(catalog_json()),
        ..Mock::default()
    };
    // Unknown entity = a flow/config bug -> terminal.
    let e = go(
        "postgres",
        &mut mock,
        &json!({"entity": "bogus", "op": "list"}),
        &json!({}),
    )
    .unwrap_err();
    assert_eq!(terminal_code(&e), "unknown-entity");

    // A JSON float where an exact decimal belongs = the INPUT's fault ->
    // invalid-input (never retried, distinct in run history).
    let e = go(
        "postgres",
        &mut mock,
        &json!({"entity": "suppliers", "op": "create"}),
        &json!({"name": "acme", "standard_cost": 12.5}),
    )
    .unwrap_err();
    assert!(
        matches!(&e, NodeError::InvalidInput(d) if d.code.as_deref() == Some("invalid-value")),
        "float rejected as the input's fault: {e:?}"
    );
    assert!(mock.pg_calls.is_empty(), "nothing reached the database");
}

/// A transient pg failure surfaces as retryable THROUGH the node (the engine
/// retries mechanically); a constraint violation is terminal and carries the
/// constraint name for the error branch.
#[test]
fn postgres_failures_flow_through_the_taxonomy() {
    let mut mock = Mock {
        catalog: Some(catalog_json()),
        ..Mock::default()
    };
    mock.pg_results
        .push_back(Err(PgCapError::ConnectionUnavailable));
    let config = json!({"entity": "suppliers", "op": "list"});
    let e = go("postgres", &mut mock, &config, &json!({})).unwrap_err();
    assert!(matches!(e, NodeError::Retryable(_)));

    mock.pg_results.push_back(Err(PgCapError::UniqueViolation(
        "suppliers_name_key".into(),
    )));
    let config = json!({"entity": "suppliers", "op": "create"});
    let e = go("postgres", &mut mock, &config, &json!({"name": "acme"})).unwrap_err();
    let NodeError::Terminal(d) = &e else {
        panic!("unique violation must be terminal");
    };
    assert_eq!(d.code.as_deref(), Some("unique-violation"));
    assert_eq!(d.data.as_ref().unwrap()["constraint"], "suppliers_name_key");
}

// ---------------------------------------------------------------------------
// postgres-query (raw SQL, D8 flag)
// ---------------------------------------------------------------------------

/// D8: the raw-SQL node is DEAD by default — the dispatch check refuses it
/// before the node runs, nothing reaches the database, and the error names
/// the flag.
#[test]
fn raw_sql_is_denied_when_the_flag_is_off() {
    let mut mock = Mock::default(); // raw_sql: false = the default grant set
    let config = json!({"sql": "SELECT 1"});
    let e = go("postgres-query", &mut mock, &config, &json!({})).unwrap_err();
    assert_eq!(terminal_code(&e), "capability-denied");
    let NodeError::Terminal(d) = &e else {
        unreachable!()
    };
    assert!(d.message.contains("D8"), "names the flag: {}", d.message);
    assert!(mock.pg_calls.is_empty(), "nothing reached the database");
}

#[test]
fn raw_sql_binds_jmespath_params_when_granted() {
    let mut mock = Mock {
        raw_sql: true,
        ..Mock::default()
    };
    mock.pg_results.push_back(Ok(PgRows {
        columns: vec!["n".into(), "qty".into()],
        rows: vec![vec![PgValue::Int64(1), PgValue::Numeric("12.50".into())]],
    }));
    let config = json!({
        "sql": "SELECT n, qty FROM t WHERE id = $1 AND ok = $2",
        "params": ["receipt.id", "flags.ok"]
    });
    let input = json!({"receipt": {"id": "abc"}, "flags": {"ok": true}});
    let em = go("postgres-query", &mut mock, &config, &input).unwrap();

    let (sql, params) = &mock.pg_calls[0];
    assert_eq!(sql, "SELECT n, qty FROM t WHERE id = $1 AND ok = $2");
    assert_eq!(params[0], PgValue::Text("abc".into()));
    assert_eq!(params[1], PgValue::Bool(true));
    assert_eq!(em.payload["rows"], json!([{"n": 1, "qty": "12.50"}]));
}

#[test]
fn raw_sql_execute_mode_reports_affected_rows() {
    let mut mock = Mock {
        raw_sql: true,
        execute_result: 3,
        ..Mock::default()
    };
    let config = json!({"sql": "UPDATE t SET x = 1", "mode": "execute"});
    let em = go("postgres-query", &mut mock, &config, &json!({})).unwrap();
    assert_eq!(em.payload, json!({"rows-affected": 3}));
}

// ---------------------------------------------------------------------------
// registry / policy table / respond
// ---------------------------------------------------------------------------

#[test]
fn unknown_node_type_is_terminal() {
    let mut mock = Mock::default();
    let e = go("teleport", &mut mock, &json!({}), &json!({})).unwrap_err();
    assert_eq!(terminal_code(&e), "unknown-node-type");
}

/// C2-3 (wamn-bd5): the PUBLIC node-resolution surface is descriptor-only — a
/// caller can learn that a type exists and what it may touch, but can NOT obtain
/// a runnable `&dyn Node` and skip the capability gate. This test pins that
/// surface: it references `describe` / `NodeDescriptor` / `is_standard` (so
/// reverting the tightening — re-exposing `pub fn node` and dropping the
/// descriptor surface — fails to COMPILE here) and asserts the descriptor
/// carries the same policy row `required_capabilities` reports. The ONLY run
/// path out of the crate stays `dispatch`, which is gate-tested below.
#[test]
fn public_resolution_surface_is_descriptor_only() {
    use wamn_nodes::{NodeDescriptor, describe, is_standard};

    // Pin the descriptor-returning signature (compile-time API-surface guard).
    let resolve: fn(&str) -> Option<NodeDescriptor> = describe;

    let d = resolve("http-request").expect("http-request is a standard node");
    assert_eq!(d.node_type, "http-request");
    assert_eq!(d.capabilities, &[Capability::HttpEgress][..]);
    // The descriptor's row and the capability query agree, by construction.
    assert_eq!(Some(d.capabilities), required_capabilities("http-request"));

    // The existence check the flow-runner uses (the non-running replacement for
    // the old `node(t).is_some()` leak) covers exactly the shipped types.
    for t in wamn_nodes::NODE_TYPES {
        assert!(is_standard(t), "{t} is a standard node");
    }
    assert!(
        !is_standard("custom"),
        "a custom node is not standard-library"
    );
    assert!(describe("delay").is_none(), "delay is runner-intrinsic");
}

/// The dispatch-time capability policy table, pinned row by row.
#[test]
fn capability_table_rows_are_exact() {
    assert_eq!(required_capabilities("transform"), Some(&[][..]));
    assert_eq!(required_capabilities("conditional"), Some(&[][..]));
    assert_eq!(required_capabilities("time-shift"), Some(&[][..]));
    assert_eq!(required_capabilities("respond"), Some(&[][..]));
    assert_eq!(
        required_capabilities("http-request"),
        Some(&[Capability::HttpEgress][..])
    );
    assert_eq!(
        required_capabilities("postgres"),
        Some(&[Capability::Postgres][..])
    );
    assert_eq!(
        required_capabilities("postgres-query"),
        Some(&[Capability::Postgres, Capability::RawSql][..])
    );
    assert_eq!(
        required_capabilities("delay"),
        None,
        "delay is runner-intrinsic"
    );
}

/// A node type is refused OUTRIGHT when the runner cannot grant its row —
/// e.g. an http-request dispatched by a runner granting nothing.
#[test]
fn dispatch_refuses_ungranted_capability_rows() {
    let mut mock = Mock::default();
    let e = dispatch(
        "http-request",
        &[],
        &mut mock,
        &run(&json!({"url": "http://api.test/x"})),
        &json!({}),
    )
    .unwrap_err();
    assert_eq!(terminal_code(&e), "capability-denied");
    assert!(mock.http_calls.is_empty(), "the node never ran");
}

/// Drift guard: every node type the committed F3 fixture uses must be a node
/// this library actually SHIPS (`is_standard`). The F3 flow (wamn-flow's
/// fixtures) drives the deployed runner, whose std nodes are exactly this
/// crate — so a fixture that named a type the library dropped (or renamed
/// `time-shift`) would fail to dispatch in-cluster; this catches it in unit
/// tests instead. Reads the sibling crate's fixture by manifest-relative path
/// (the wit_coherence precedent), keeping the two crates decoupled.
#[test]
fn f3_fixture_node_types_are_all_standard() {
    use wamn_nodes::is_standard;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../wamn-flow/tests/fixtures/f3-escalate-stale-holds.flow.json");
    let raw = std::fs::read_to_string(&path).expect("read F3 fixture");
    let flow: Value = serde_json::from_str(&raw).expect("F3 fixture is json");
    let nodes = flow["nodes"].as_array().expect("nodes array");
    let types: Vec<&str> = nodes.iter().map(|n| n["type"].as_str().unwrap()).collect();
    for t in &types {
        assert!(
            is_standard(t),
            "F3 node type {t:?} is not a shipped standard node"
        );
    }
    // The cutoff-gap node is the one this lane adds — pin that it is present.
    assert!(
        types.contains(&"time-shift"),
        "F3 must use the time-shift node"
    );
}

#[test]
fn respond_passes_through_and_exposes_status() {
    let mut mock = Mock::default();
    let input = json!({"receipt_id": UUID_1, "holds": []});
    let em = go("respond", &mut mock, &json!({"status": 201}), &input).unwrap();
    assert_eq!(em.payload, input);
    assert_eq!(respond::status_for(&json!({"status": 201})), Some(201));
    assert_eq!(respond::status_for(&json!({})), None);
    assert_eq!(respond::status_for(&json!({"status": 9999})), None);
}
