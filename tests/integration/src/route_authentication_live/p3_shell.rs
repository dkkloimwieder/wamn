//! P3 protocol cases over the shipped Receiving HTTP component.

use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use futures_util::{StreamExt as _, stream};
use http_body_util::{BodyExt as _, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::body::Frame;
use hyper::{Method, Request, StatusCode};
use tokio::sync::Notify;
use wamn_execution_host::RouterDeliveryBridge;
use wamn_runtime::plugins::flow_http_routing::FlowHttpRouting;
use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;

use super::{RAW_BODY_LIMIT, invoke_journey_request, successful_value};

const TIMEOUT: Duration = Duration::from_secs(10);
const PATH: &str = "/purchase_order/get";
const REQUEST_ID: &str = "p3-protocol";
const PURCHASE_ORDER_ID: &str = "00000000-0000-0000-0000-000000000301";
const PAYLOAD: &[u8] =
    br#"[{"request_id":"p3-protocol","id":"00000000-0000-0000-0000-000000000301"}]"#;

pub(super) async fn assert_p3_route(
    engine: &Engine,
    flow_http: &Component,
    routing: Arc<FlowHttpRouting>,
    bridge: Arc<RouterDeliveryBridge>,
    route_host: &str,
    bearer: &str,
) -> anyhow::Result<()> {
    let invoke = |request| {
        invoke_journey_request(
            engine,
            flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            request,
        )
    };
    let response = tokio::time::timeout(
        TIMEOUT,
        invoke(request(
            route_host,
            PATH,
            Some(bearer),
            buffered(Bytes::from_static(PAYLOAD)),
        )?),
    )
    .await
    .context("P3 protocol case origin-form-host timed out")??;
    anyhow::ensure!(
        successful_value(&response, REQUEST_ID)?["id"] == PURCHASE_ORDER_ID,
        "P3 protocol case origin-form-host returned another purchase order"
    );

    for (case, path, token, status, code) in [
        (
            "stalled-body-missing-auth",
            PATH,
            None,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "stalled-body-unknown-route",
            "/p3-protocol-no-such-route",
            Some(bearer),
            StatusCode::NOT_FOUND,
            "route-not-found",
        ),
    ] {
        let body =
            StreamBody::new(stream::pending::<Result<Frame<Bytes>, ErrorCode>>()).boxed_unsync();
        let response =
            tokio::time::timeout(TIMEOUT, invoke(request(route_host, path, token, body)?))
                .await
                .with_context(|| {
                    format!("P3 protocol case {case} waited for the stalled body")
                })??;
        let expected = serde_json::json!({"error": {"code": code}});
        anyhow::ensure!(
            response.status() == status
                && serde_json::from_slice::<serde_json::Value>(response.body())? == expected,
            "P3 protocol case {case} did not preserve its HTTP refusal"
        );
    }

    let mut padded = PAYLOAD.to_vec();
    padded.resize(RAW_BODY_LIMIT, b' ');
    let response = tokio::time::timeout(
        TIMEOUT,
        invoke(request(
            route_host,
            PATH,
            Some(bearer),
            buffered(Bytes::from(padded.clone())),
        )?),
    )
    .await
    .context("P3 protocol case exact-1mib timed out")??;
    anyhow::ensure!(
        successful_value(&response, REQUEST_ID)?["id"] == PURCHASE_ORDER_ID,
        "P3 protocol case exact-1mib did not execute the valid read"
    );

    padded.push(b' ');
    let response = tokio::time::timeout(
        TIMEOUT,
        invoke(request(
            route_host,
            PATH,
            Some(bearer),
            buffered(Bytes::from(padded)),
        )?),
    )
    .await
    .context("P3 protocol case 1mib-plus-one timed out")??;
    anyhow::ensure!(
        response.status() == StatusCode::PAYLOAD_TOO_LARGE
            && response
                .headers()
                .get(hyper::header::CONTENT_TYPE)
                .is_some_and(|value| value == "text/plain; charset=utf-8")
            && response.body().as_ref() == b"request body exceeds 1048576-byte limit\n",
        "P3 protocol case 1mib-plus-one lost the typed 413 contract"
    );

    let body = StreamBody::new(stream::iter([
        Ok(Frame::data(Bytes::from_static(PAYLOAD))),
        Err(ErrorCode::ConnectionTerminated),
    ]))
    .boxed_unsync();
    let response = tokio::time::timeout(
        TIMEOUT,
        invoke(request(route_host, PATH, Some(bearer), body)?),
    )
    .await
    .context("P3 protocol case transport-error-after-json timed out")??;
    anyhow::ensure!(
        response.status() == StatusCode::BAD_REQUEST
            && response.body().as_ref() == br#"{"error":{"code":"body-read-failed"}}"#,
        "P3 protocol case transport-error-after-json did not refuse the incomplete transport"
    );

    let stalled = Arc::new(Notify::new());
    let observed = Arc::clone(&stalled);
    let body = StreamBody::new(
        stream::iter([Ok(Frame::data(Bytes::from_static(PAYLOAD)))]).chain(stream::poll_fn(
            move |_| {
                observed.notify_one();
                Poll::<Option<Result<Frame<Bytes>, ErrorCode>>>::Pending
            },
        )),
    )
    .boxed_unsync();
    {
        let invocation = invoke(request(route_host, PATH, Some(bearer), body)?);
        tokio::pin!(invocation);
        tokio::select! {
            biased;
            response = &mut invocation => {
                let response = response.context("P3 protocol case pending-body-cancellation failed before cancellation")?;
                anyhow::bail!(
                    "P3 protocol case pending-body-cancellation completed early with {}",
                    response.status()
                );
            }
            () = stalled.notified() => {}
            () = tokio::time::sleep(TIMEOUT) => {
                anyhow::bail!("P3 protocol case pending-body-cancellation never reached the stalled body");
            }
        }
        anyhow::ensure!(
            engine.guest_memory().in_use() > 0,
            "P3 protocol case pending-body-cancellation did not hold a live guest store"
        );
    }
    anyhow::ensure!(
        engine.guest_memory().in_use() == 0,
        "P3 protocol case pending-body-cancellation retained guest memory after drop"
    );
    let response = tokio::time::timeout(
        TIMEOUT,
        invoke(request(
            route_host,
            PATH,
            Some(bearer),
            buffered(Bytes::from_static(PAYLOAD)),
        )?),
    )
    .await
    .context("P3 protocol case recovery-after-cancellation timed out")??;
    anyhow::ensure!(
        successful_value(&response, REQUEST_ID)?["id"] == PURCHASE_ORDER_ID,
        "P3 protocol case recovery-after-cancellation did not execute the next valid request"
    );
    println!(
        "P3 protocol cases passed: origin-form-host, stalled-body-missing-auth, \
         stalled-body-unknown-route, exact-1mib, 1mib-plus-one, transport-error-after-json, \
         pending-body-cancellation, recovery-after-cancellation"
    );
    Ok(())
}

fn buffered(bytes: Bytes) -> UnsyncBoxBody<Bytes, ErrorCode> {
    Full::new(bytes)
        .map_err(|never| -> ErrorCode { match never {} })
        .boxed_unsync()
}

fn request(
    route_host: &str,
    path: &str,
    bearer: Option<&str>,
    body: UnsyncBoxBody<Bytes, ErrorCode>,
) -> anyhow::Result<Request<UnsyncBoxBody<Bytes, ErrorCode>>> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(hyper::header::HOST, route_host)
        .header(hyper::header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        request = request.header(hyper::header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    request
        .body(body)
        .context("build a P3 protocol origin-form request")
}
