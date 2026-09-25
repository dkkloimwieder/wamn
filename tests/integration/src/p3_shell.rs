//! P3 protocol cases over a supplied application read route.

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
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_execution_host::RouterDeliveryBridge;
use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;

use crate::local_application::{LocalInvocation, invoke_request};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Application-owned request with a body, used to exercise the HTTP protocol
/// shell. It is a write, because a read is a GET and carries no body.
#[derive(Debug)]
pub struct BodyProbe<'a> {
    pub path: &'a str,
    pub payload: &'a [u8],
    pub body_limit: usize,
    pub validate_response: fn(&hyper::Response<Bytes>) -> anyhow::Result<()>,
}

pub async fn assert_p3_route(
    engine: &Engine,
    flow_http: &Component,
    routing: Arc<FlowHttpRouting>,
    bridge: Arc<RouterDeliveryBridge>,
    route_host: &str,
    bearer: &str,
    probe: &BodyProbe<'_>,
) -> anyhow::Result<()> {
    let invoke = |request| {
        invoke_checked(
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
            probe.path,
            Some(bearer),
            buffered(Bytes::copy_from_slice(probe.payload)),
        )?),
    )
    .await
    .context("P3 protocol case origin-form-host timed out")??;
    (probe.validate_response)(&response)
        .context("P3 protocol case origin-form-host returned another record")?;

    for (case, path, token, status, code) in [
        (
            "stalled-body-missing-auth",
            probe.path,
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

    let mut padded = probe.payload.to_vec();
    padded.resize(probe.body_limit, b' ');
    let response = tokio::time::timeout(
        TIMEOUT,
        invoke(request(
            route_host,
            probe.path,
            Some(bearer),
            buffered(Bytes::from(padded.clone())),
        )?),
    )
    .await
    .context("P3 protocol case exact-1mib timed out")??;
    (probe.validate_response)(&response)
        .context("P3 protocol case exact-1mib did not execute the valid request")?;

    padded.push(b' ');
    let response = tokio::time::timeout(
        TIMEOUT,
        invoke(request(
            route_host,
            probe.path,
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
            && response.body().as_ref()
                == format!("request body exceeds {}-byte limit\n", probe.body_limit).as_bytes(),
        "P3 protocol case 1mib-plus-one lost the typed 413 contract"
    );

    let body = StreamBody::new(stream::iter([
        Ok(Frame::data(Bytes::copy_from_slice(probe.payload))),
        Err(ErrorCode::ConnectionTerminated),
    ]))
    .boxed_unsync();
    let response = tokio::time::timeout(
        TIMEOUT,
        invoke(request(route_host, probe.path, Some(bearer), body)?),
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
        stream::iter([Ok(Frame::data(Bytes::copy_from_slice(probe.payload)))]).chain(
            stream::poll_fn(move |_| {
                observed.notify_one();
                Poll::<Option<Result<Frame<Bytes>, ErrorCode>>>::Pending
            }),
        ),
    )
    .boxed_unsync();
    {
        let invocation = invoke_request(
            engine,
            flow_http,
            Arc::clone(&routing),
            Arc::clone(&bridge),
            request(route_host, probe.path, Some(bearer), body)?,
        );
        tokio::pin!(invocation);
        tokio::select! {
            biased;
            response = &mut invocation => {
                let response = response.context("P3 protocol case pending-body-cancellation failed before cancellation")?;
                anyhow::bail!(
                    "P3 protocol case pending-body-cancellation completed early with {}",
                    response.response.status()
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
            probe.path,
            Some(bearer),
            buffered(Bytes::copy_from_slice(probe.payload)),
        )?),
    )
    .await
    .context("P3 protocol case recovery-after-cancellation timed out")??;
    (probe.validate_response)(&response).context(
        "P3 protocol case recovery-after-cancellation did not execute the next valid request",
    )?;
    println!(
        "P3 protocol cases passed: origin-form-host, stalled-body-missing-auth, \
         stalled-body-unknown-route, exact-1mib, 1mib-plus-one, transport-error-after-json, \
         pending-body-cancellation, recovery-after-cancellation"
    );
    Ok(())
}

async fn invoke_checked<B>(
    engine: &Engine,
    flow_http: &Component,
    routing: Arc<FlowHttpRouting>,
    bridge: Arc<RouterDeliveryBridge>,
    request: Request<B>,
) -> anyhow::Result<hyper::Response<Bytes>>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<wasmtime_wasi_http::Error>,
{
    let LocalInvocation {
        response,
        shell_bytes,
        in_use_before_drop,
        ..
    } = invoke_request(engine, flow_http, routing, bridge, request).await?;
    anyhow::ensure!(
        in_use_before_drop == shell_bytes,
        "P3 protocol invocation retained memory outside its flow-http store"
    );
    anyhow::ensure!(
        engine.guest_memory().in_use() == 0,
        "P3 protocol invocation retained guest memory after its stores dropped"
    );
    Ok(response)
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
