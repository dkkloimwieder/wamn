//! Receiving login and client selection over real identity and route execution.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use hyper::Method;
use serde_json::{Value, json};
use wamn_client::{
    ClientError, CredentialProvider, HttpRequest, HttpResponse, ItemOutcome, RouteMetadata,
    Transport, WamnClient,
};
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_execution_host::RouterDeliveryBridge;
use wamn_platform_identity::{Principal, issue_pat, revoke_pat};
use wamn_receiving_tui::login;
use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;

use super::{
    BASE_RECORD_RECEIPT, JourneyRuntime, OPERATION, assert_direct_route_trace, fresh_only,
    invoke_journey_method, journey_trace, nested_receipt_state, span_attribute,
    trace_component_invocations,
};

fn transport_failure() -> ClientError {
    ClientError::Transport {
        detail: "live client fixture transport failed".to_owned(),
    }
}

/// Only this adapter crosses the real TLS identity endpoint. It never records bodies.
struct ExchangeTransport {
    http: reqwest::Client,
    endpoint: String,
    calls: AtomicUsize,
    status: AtomicU16,
}

impl std::fmt::Debug for ExchangeTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExchangeTransport")
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Transport for ExchangeTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        if request.url != self.endpoint || request.method != "POST" {
            return Err(transport_failure());
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut outgoing = self.http.post(&request.url).body(request.body);
        for (name, value) in request.headers {
            outgoing = outgoing.header(name, value);
        }
        let mut response = outgoing.send().await.map_err(|_| transport_failure())?;
        let status = response.status().as_u16();
        self.status.store(status, Ordering::SeqCst);
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| transport_failure())? {
            if chunk.len() > 65_536 - body.len() {
                return Err(transport_failure());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpResponse {
            actor_labels: std::collections::BTreeMap::new(),
            status,
            body: String::from_utf8(body).map_err(|_| transport_failure())?,
        })
    }
}

pub(super) struct Login {
    pub credentials: Arc<dyn CredentialProvider>,
    transport: Arc<ExchangeTransport>,
    issuer: String,
    audience: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Login").finish_non_exhaustive()
    }
}

pub(super) async fn login(
    http: reqwest::Client,
    endpoint: &reqwest::Url,
    audience: &str,
    pat: &str,
) -> anyhow::Result<Login> {
    let transport = Arc::new(ExchangeTransport {
        http,
        endpoint: endpoint.join("/session")?.to_string(),
        calls: AtomicUsize::new(0),
        status: AtomicU16::new(0),
    });
    // This is the configured, CA-verified loopback port-forward used by the
    // existing live continuation; signed claims retain the actual cluster issuer.
    let target = login::session_target(Some(endpoint.as_str()), Some(audience))?;
    let credentials = login::credentials(pat.to_owned(), target, transport.clone()).await?;
    anyhow::ensure!(
        transport.calls.load(Ordering::SeqCst) == 1
            && transport.status.load(Ordering::SeqCst) == 200,
        "Receiving login must complete exactly one real session exchange"
    );
    Ok(Login {
        credentials,
        transport,
        issuer: endpoint.to_string(),
        audience: audience.to_owned(),
    })
}

/// Adapts client HTTP bytes to the existing production flow-http guest seam.
/// It does not manufacture route replies or select credentials.
pub(super) struct RouteTransport {
    engine: Arc<Engine>,
    flow_http: Component,
    routing: Arc<FlowHttpRouting>,
    bridge: Arc<RouterDeliveryBridge>,
    first_trace: u64,
    calls: AtomicUsize,
}

impl std::fmt::Debug for RouteTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteTransport")
            .finish_non_exhaustive()
    }
}

impl RouteTransport {
    pub(super) fn new(
        engine: Arc<Engine>,
        flow_http: Component,
        routing: Arc<FlowHttpRouting>,
        bridge: Arc<RouterDeliveryBridge>,
        first_trace: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            flow_http,
            routing,
            bridge,
            first_trace,
            calls: AtomicUsize::new(0),
        })
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Transport for RouteTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        let url = reqwest::Url::parse(&request.url).map_err(|_| transport_failure())?;
        let content_type = request.headers.get("content-type").map(String::as_str);
        // A read is a GET with its one item in the query and no body; a write
        // is a JSON POST.
        let method = match request.method.as_str() {
            "GET" if content_type.is_none() && request.body.is_empty() => Method::GET,
            "POST" if content_type == Some("application/json") && url.query().is_none() => {
                Method::POST
            }
            _ => return Err(transport_failure()),
        };
        if url.origin().ascii_serialization() != "http://client-test.test"
            || url.fragment().is_some()
        {
            return Err(transport_failure());
        }
        let target = url.query().map_or_else(
            || url.path().to_owned(),
            |query| format!("{}?{query}", url.path()),
        );
        let host = request.headers.get("host").ok_or_else(transport_failure)?;
        let bearer = request
            .headers
            .get("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(transport_failure)?;
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let (_, parent) = journey_trace(self.first_trace + index as u64);
        let response = invoke_journey_method(
            &JourneyRuntime {
                engine: &self.engine,
                flow_http: &self.flow_http,
                routing: &self.routing,
                bridge: &self.bridge,
            },
            method,
            host,
            &target,
            Some(bearer),
            &parent,
            request.body.into(),
        )
        .await
        .map_err(|_| transport_failure())?;
        Ok(HttpResponse {
            actor_labels: std::collections::BTreeMap::new(),
            status: response.status().as_u16(),
            body: String::from_utf8(response.into_body().to_vec())
                .map_err(|_| transport_failure())?,
        })
    }
}

pub(super) fn client(
    credentials: Arc<dyn CredentialProvider>,
    transport: Arc<RouteTransport>,
    host: &str,
) -> WamnClient {
    WamnClient::new(
        "http://client-test.test",
        Some(host.to_owned()),
        credentials,
        transport,
    )
}

pub(super) fn route(path: &str) -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: path.to_owned(),
    }
}

pub(super) fn read_route(path: &str) -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_owned(),
        template: path.to_owned(),
    }
}

/// A read expects no `request_id`; its outcome matches the item by position.
pub(super) fn assert_value(
    result: &Result<Vec<ItemOutcome>, ClientError>,
    request_id: Option<&str>,
    expected: &Value,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(result.as_deref(), Ok([item]) if item.request_id.as_deref() == request_id && item.value.as_ref() == Some(expected) && item.error.is_none()),
        "client response differs from the independent full operation result"
    );
    Ok(())
}

pub(super) fn assert_permission_refusal(
    result: Result<Vec<ItemOutcome>, ClientError>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        matches!(result, Err(ClientError::Operation { literal, detail }) if literal == "permission-denied" && detail == json!({"operation": BASE_RECORD_RECEIPT})),
        "client must expose the exact late permission-denied refusal"
    );
    Ok(())
}

pub(super) async fn assert_session_client(
    test: fresh_only::PriorCommitTest<'_>,
    login: &Login,
    transport: Arc<RouteTransport>,
    human: &Principal,
    base_digest: &str,
    direct_path: &str,
) -> anyhow::Result<()> {
    let client = client(
        login.credentials.clone(),
        transport.clone(),
        &test.inputs.route_host,
    );
    let before = nested_receipt_state(test.project).await?;
    let expected: Value = test.project.query_one(
        "SELECT jsonb_build_object('id', id::text, 'purchase_order_number', purchase_order_number, \
         'supplier_id', supplier_id::text, 'status', status, 'row_version', row_version::text, \
         'created_at', to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), \
         'updated_at', to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')) \
         FROM receiving.purchase_order WHERE id = '00000000-0000-0000-0000-000000000301'", &[]
    ).await.context("read independent client GET result")?.get(0);
    let get = [json!({"id": "00000000-0000-0000-0000-000000000301"})];
    for index in 0..2 {
        assert_value(
            &client
                .invoke(&read_route("/purchase_order/get"), &BTreeMap::new(), &get)
                .await,
            None,
            &expected,
        )?;
        anyhow::ensure!(
            transport.calls() == index + 1 && login.transport.calls.load(Ordering::SeqCst) == 1,
            "ordinary client call replayed or refreshed the warm session"
        );
        assert_kind(
            &test,
            61 + index as u64,
            "",
            OPERATION,
            base_digest,
            "session",
        )?;
    }
    let body: Vec<Value> = serde_json::from_slice(&test.body)?;
    assert_value(
        &client
            .invoke_fresh(&route(direct_path), &BTreeMap::new(), &body)
            .await,
        Some("session-nested-replay"),
        test.expected_base,
    )?;
    anyhow::ensure!(
        transport.calls() == 3 && login.transport.calls.load(Ordering::SeqCst) == 1,
        "legacy fresh client call must send one session request without exchange"
    );
    assert_kind(&test, 63, "", BASE_RECORD_RECEIPT, base_digest, "session")?;

    let expired = issue_pat(
        test.control,
        human.id(),
        "client expired login",
        Duration::from_secs(3600),
    )
    .await?;
    // The stamp trigger keeps created_at, so the token expires just after it.
    anyhow::ensure!(
        test.control
            .execute(
                "UPDATE identity.pats SET expires_at = created_at + interval '1 microsecond' \
         WHERE token_prefix = $1",
                &[&expired.record().prefix()],
            )
            .await?
            == 1,
        "expired client control must change one dedicated PAT"
    );
    refused_login(login, expired.token(), &transport).await?;
    let revoked = issue_pat(
        test.control,
        human.id(),
        "client revoked login",
        Duration::from_secs(3600),
    )
    .await?;
    revoke_pat(test.control, revoked.record().prefix()).await?;
    refused_login(login, revoked.token(), &transport).await?;

    anyhow::ensure!(
        nested_receipt_state(test.project).await? == before,
        "client GET, explicit replay, or refused login changed the existing business state"
    );
    fresh_only::test_prior_commit(test).await?;
    anyhow::ensure!(
        login.transport.calls.load(Ordering::SeqCst) == 1,
        "nested refusal or explicit retry refreshed the session"
    );
    Ok(())
}

fn assert_kind(
    test: &fresh_only::PriorCommitTest<'_>,
    index: u64,
    wiring: &str,
    operation: &str,
    digest: &str,
    kind: &str,
) -> anyhow::Result<()> {
    let (trace, _) = journey_trace(index);
    let spans = test.traces.spans();
    assert_direct_route_trace(&spans, &trace, wiring, operation, digest, test.human_id);
    let invocations = trace_component_invocations(&spans, &trace);
    anyhow::ensure!(
        invocations.len() == 1
            && span_attribute(invocations[0], "wamn.caller_credential_kind").as_deref()
                == Some(kind),
        "actual guest invocation lost the selected credential kind"
    );
    Ok(())
}

async fn refused_login(
    login: &Login,
    pat: &str,
    application: &RouteTransport,
) -> anyhow::Result<()> {
    let transport = Arc::new(ExchangeTransport {
        http: login.transport.http.clone(),
        endpoint: login.transport.endpoint.clone(),
        calls: AtomicUsize::new(0),
        status: AtomicU16::new(0),
    });
    let before = application.calls();
    let target = login::session_target(Some(&login.issuer), Some(&login.audience))?;
    let result = login::credentials(pat.to_owned(), target, transport.clone()).await;
    anyhow::ensure!(
        result.is_err()
            && transport.calls.load(Ordering::SeqCst) == 1
            && transport.status.load(Ordering::SeqCst) == 401
            && application.calls() == before,
        "invalid PAT login must get one real 401 without an application call or fallback"
    );
    Ok(())
}
