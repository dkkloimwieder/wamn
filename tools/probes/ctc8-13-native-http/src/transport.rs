//! Public native HTTP entry points, with no replacement connector or policy.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::Request;
use hyper_util::client::legacy::connect::HttpInfo;
use serde_json::{Value, json};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{DefaultOutgoingHandler, DynamicRouter, HostHandler as _, Ingress};
use wash_runtime::host::quota::{QuotaLimits, QuotaRegistry};
use wasmtime_wasi::p2::Pollable as _;
use wasmtime_wasi_http::p2::types::OutgoingRequestConfig;

pub(crate) type Host = Ingress<DynamicRouter>;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Abi {
    P2,
    P3,
}

#[derive(Clone)]
pub(crate) struct Call {
    pub abi: Abi,
    pub url: String,
    pub workload: String,
    pub marker: String,
    pub grpc: bool,
    pub body_bytes: usize,
    pub padding: usize,
    pub timeout: Duration,
    pub allow: bool,
}

#[derive(Debug)]
pub(crate) struct Reply {
    pub connection: usize,
    pub peer: Option<SocketAddr>,
    pub version: String,
    pub body_bytes: usize,
    pub header_bytes: usize,
    pub elapsed_ms: u128,
}

pub(crate) fn call(
    abi: Abi,
    address: SocketAddr,
    tls: bool,
    grpc: bool,
    workload: &str,
    path: &str,
) -> Call {
    Call {
        abi,
        url: format!("{}://{address}{path}", if tls { "https" } else { "http" }),
        workload: workload.to_owned(),
        marker: "synthetic-g1".to_owned(),
        grpc,
        body_bytes: 8,
        padding: 0,
        timeout: Duration::from_secs(2),
        allow: true,
    }
}

pub(crate) async fn host(
    tls: Arc<rustls::ClientConfig>,
    per_scope: usize,
    total: usize,
) -> Result<(Arc<Host>, Arc<QuotaRegistry>)> {
    let quotas = QuotaRegistry::new(
        QuotaLimits {
            outbound_http: per_scope,
            ..QuotaLimits::default()
        },
        Some(total),
    );
    let handler = DefaultOutgoingHandler::with_tls_config(tls).with_quotas(Arc::clone(&quotas));
    let host = Ingress::builder(DynamicRouter::default(), "127.0.0.1:0".parse()?)
        .outgoing_handler(handler)
        .build()
        .await?;
    Ok((Arc::new(host), quotas))
}

fn request(call: &Call) -> hyper::http::request::Builder {
    Request::builder()
        .method("POST")
        .uri(&call.url)
        .header("authorization", &call.marker)
        .header("x-probe-padding", "x".repeat(call.padding))
        .header(
            "content-type",
            if call.grpc {
                "application/grpc+proto"
            } else {
                "application/octet-stream"
            },
        )
}

pub(crate) async fn send(host: &Host, call: &Call) -> Result<Reply> {
    let started = Instant::now();
    let allowed: Vec<AllowedHost> = if call.allow {
        vec!["*".parse()?]
    } else {
        Vec::new()
    };
    let bytes = Bytes::from(vec![b'p'; call.body_bytes]);
    let (parts, body_bytes) = match call.abi {
        Abi::P2 => {
            let request = request(call).body(
                Full::new(bytes)
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )?;
            let mut future = host
                .outgoing_request(
                    &call.workload,
                    request,
                    OutgoingRequestConfig {
                        use_tls: call.url.starts_with("https:"),
                        connect_timeout: call.timeout,
                        first_byte_timeout: call.timeout,
                        between_bytes_timeout: call.timeout,
                    },
                    &allowed,
                )
                .map_err(|error| anyhow::anyhow!("P2 dispatch: {error:?}"))?;
            future.ready().await;
            let incoming = future
                .unwrap_ready()
                .map_err(|error| anyhow::anyhow!("P2 future: {error:?}"))?
                .map_err(|error| anyhow::anyhow!("P2 transport: {error:?}"))?;
            let (parts, body) = incoming.resp.into_parts();
            // This is the native transport body, not the guest HostIncomingBody
            // wrapper. Do not infer P2 guest between-byte enforcement from it.
            let body = body
                .collect()
                .await
                .map_err(|error| anyhow::anyhow!("P2 body: {error:?}"))?;
            (parts, body.to_bytes().len())
        }
        Abi::P3 => {
            let request = request(call).body(
                Full::new(bytes)
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )?;
            let future = host.outgoing_request_p3(
                &call.workload,
                request,
                Some(wasmtime_wasi_http::p3::RequestOptions {
                    connect_timeout: Some(call.timeout),
                    first_byte_timeout: Some(call.timeout),
                    between_bytes_timeout: Some(call.timeout),
                }),
                Box::new(async { Ok(()) }),
                &allowed,
            );
            let (response, upload) = Box::into_pin(future)
                .await
                .map_err(|error| anyhow::anyhow!("P3 transport: {error:?}"))?;
            let (parts, body) = response.into_parts();
            let body = body
                .collect()
                .await
                .map_err(|error| anyhow::anyhow!("P3 body: {error:?}"))?;
            Box::into_pin(upload)
                .await
                .map_err(|error| anyhow::anyhow!("P3 request outcome: {error:?}"))?;
            (parts, body.to_bytes().len())
        }
    };
    anyhow::ensure!(parts.status == 200, "fixture returned {}", parts.status);
    let connection = parts
        .headers
        .get("x-probe-connection")
        .context("fixture connection header is missing")?
        .to_str()?
        .parse()?;
    Ok(Reply {
        connection,
        peer: parts
            .extensions
            .get::<HttpInfo>()
            .map(HttpInfo::remote_addr),
        version: format!("{:?}", parts.version),
        body_bytes,
        header_bytes: parts
            .headers
            .iter()
            .map(|(key, value)| key.as_str().len() + value.as_bytes().len())
            .sum(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub(crate) fn observation(reply: &Reply) -> Value {
    json!({"connection":reply.connection,"connected_peer":reply.peer.map(|peer| peer.to_string()),
        "version":reply.version,"response_bytes":reply.body_bytes,"header_bytes":reply.header_bytes,
        "elapsed_ms":reply.elapsed_ms})
}
