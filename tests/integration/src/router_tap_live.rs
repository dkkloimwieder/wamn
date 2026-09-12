//! Live test for the release-owned router tap (`wamn-0h0g.24.5`).
//!
//! This is deliberately not an in-memory bridge test. The checked-in
//! `WAMN_TAP` provisioning must already have run on a disposable NATS, the
//! shipped flow-http guest supplies the request through the production
//! release-backed routing plugin, and the bridge executes the real OCI-backed
//! `RouterDriver` closure before this test reads the two stored previews.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Context as _, ensure};
    use async_nats::jetstream::consumer::{
        AckPolicy, Consumer, DeliverPolicy, pull::Config as PullConfig,
    };
    use async_nats::jetstream::stream::{DiscardPolicy, RetentionPolicy, StorageType};
    use bytes::Bytes;
    use futures_util::StreamExt as _;
    use http_body_util::{BodyExt as _, Full};
    use hyper::{Method, Request, StatusCode};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use wamn_execution_host::{ROUTER_DELIVERY_ID, RouterDeliveryBridge};
    use wamn_runtime::engine::build_engine;
    use wamn_runtime::plugins::flow_http_routing::FLOW_HTTP_ROUTING_ID;
    use wamn_runtime::plugins::wamn_jetstream::WamnJetstreamConfig;
    use wamn_runtime::plugins::{FlowHttpRouting, WamnJetstream};
    use wash_runtime::engine::InstancePolicy;
    use wash_runtime::engine::ctx::{Ctx, SharedCtx};
    use wash_runtime::engine::workload::{WorkloadComponent, WorkloadItem};
    use wash_runtime::plugin::{HostPlugin, WitInterfaces};
    use wash_runtime::types::LocalResources;
    use wash_runtime::wasmtime::Store;
    use wash_runtime::wasmtime::component::{Component, Linker};
    use wasmtime_wasi_http::p3::bindings::Service;
    use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;

    use crate::trusted_http_route::{
        self, ATTACHMENT_ID, ENVIRONMENT, PROJECT, ROUTE_AUTHORITY, ROUTE_PATH, RouteOptions,
        TENANT, WIRING_ID,
    };

    const STREAM: &str = "WAMN_TAP";

    fn required(key: &str) -> anyhow::Result<String> {
        std::env::var(key)
            .ok()
            .filter(|value| !value.is_empty())
            .with_context(|| format!("set {key} for the disposable router-tap proof"))
    }

    async fn upstream_origin() -> anyhow::Result<(u16, tokio::task::JoinHandle<Vec<u8>>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind the disposable upstream")?;
        let port = listener.local_addr()?.port();
        let served = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("the released node connects");
            let mut received = Vec::new();
            let mut byte = [0_u8; 1];
            while !received.ends_with(b"\r\n\r\n") {
                let read = socket.read(&mut byte).await.expect("read request head");
                assert_ne!(read, 0, "request ended before its headers");
                received.push(byte[0]);
            }
            let head = String::from_utf8(received).expect("request head is utf-8");
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map_or(0, |(_, value)| {
                    value
                        .trim()
                        .parse::<usize>()
                        .expect("numeric content length")
                });
            let mut body = vec![0_u8; length];
            socket
                .read_exact(&mut body)
                .await
                .expect("read request body");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\
                      connection: close\r\n\r\n{}",
                )
                .await
                .expect("answer the released node");
            socket.flush().await.expect("flush upstream response");
            body
        });
        Ok((port, served))
    }

    async fn invoke_flow_http(
        component_path: PathBuf,
        routing: Arc<FlowHttpRouting>,
        bridge: Arc<RouterDeliveryBridge>,
        body: Bytes,
    ) -> anyhow::Result<hyper::Response<Bytes>> {
        let engine = build_engine(&[]).context("build the flow-http engine")?;
        let raw = engine.inner();
        let component_bytes = std::fs::read(&component_path)
            .with_context(|| format!("read {}", component_path.display()))?;
        let component = Component::new(raw, &component_bytes)
            .map_err(|error| anyhow::anyhow!("compile flow-http: {error}"))?;
        let mut linker = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|error| anyhow::anyhow!("link WASI into flow-http: {error}"))?;
        wasmtime_wasi_http::p3::add_to_linker(&mut linker)
            .map_err(|error| anyhow::anyhow!("link wasi:http into flow-http: {error}"))?;
        let loopback = Arc::new(std::sync::Mutex::new(
            wash_runtime::sockets::loopback::Network::default(),
        ));
        let mut workload = WorkloadComponent::new(
            "router-tap-live",
            "router-tap-live",
            "wamn",
            "flow-http",
            component,
            linker,
            Vec::new(),
            LocalResources::default(),
            loopback,
            InstancePolicy::Ephemeral,
        );
        let imports = workload.world().imports;
        {
            let mut item = WorkloadItem::Component(&mut workload);
            routing
                .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
                .await
                .context("bind release-backed HTTP routing")?;
            bridge
                .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
                .await
                .context("bind the production router-delivery bridge")?;
        }

        let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
        plugins.insert(FLOW_HTTP_ROUTING_ID, routing);
        plugins.insert(ROUTER_DELIVERY_ID, bridge);
        let workload_id = workload.workload_id().to_owned();
        let component_id = workload.id().to_owned();
        let ctx = Ctx::builder(workload_id, component_id)
            .with_plugins(plugins)
            .build();
        let mut store = Store::new(
            raw,
            SharedCtx::new(ctx).with_guest_memory(engine.guest_memory()),
        );
        wash_runtime::engine::guest_memory::install_memory_limiter(&mut store);
        store.set_epoch_deadline(u64::MAX / 2);
        let compiled = workload.component().clone();
        let service = Service::instantiate_async(&mut store, &compiled, workload.linker())
            .await
            .map_err(|error| anyhow::anyhow!("instantiate the shipped flow-http guest: {error}"))?;

        let body = Full::new(body).map_err(|never| -> ErrorCode { match never {} });
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("http://{ROUTE_AUTHORITY}{ROUTE_PATH}"))
            .header("content-type", "application/json")
            .body(body)
            .context("build the HTTP request")?;
        let (request, request_io) = wasmtime_wasi_http::p3::Request::from_http(request);
        // Keep the fresh store driving P3 streams until the response body is collected.
        let response = store
            .run_concurrent(async |accessor| {
                let handle = async {
                    let response = service
                        .handle(accessor, request)
                        .await
                        .map_err(|error| anyhow::anyhow!("call flow-http: {error}"))?
                        .map_err(|error| anyhow::anyhow!("flow-http returned {error:?}"))?;
                    let (finish_tx, finish_rx) =
                        tokio::sync::oneshot::channel::<Result<(), ErrorCode>>();
                    let response = accessor
                        .with(|store| {
                            response.into_http(store, async move {
                                finish_rx
                                    .await
                                    .unwrap_or(Err(ErrorCode::ConnectionTerminated))
                            })
                        })
                        .map_err(|error| anyhow::anyhow!("convert flow-http response: {error}"))?;
                    let (parts, body) = response.into_parts();
                    let body = body.collect().await;
                    let _ = finish_tx.send(body.as_ref().map(|_| ()).map_err(Clone::clone));
                    let body = body.map_err(|error| {
                        anyhow::anyhow!("collect flow-http response: {error:?}")
                    })?;
                    Ok::<_, anyhow::Error>(hyper::Response::from_parts(parts, body.to_bytes()))
                };
                let io = async {
                    // An early typed refusal may abandon its request body.
                    if let Err(error) = request_io.await {
                        tracing::debug!(
                            ?error,
                            "flow-http request body processing ended with an error"
                        );
                    }
                    Ok::<_, anyhow::Error>(())
                };
                let (response, ()) = tokio::try_join!(handle, io)?;
                Ok::<_, anyhow::Error>(response)
            })
            .await
            .map_err(|error| anyhow::anyhow!("drive flow-http P3 request: {error}"))??;
        Ok(response)
    }

    async fn read_tap_records(
        consumer: &Consumer<PullConfig>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let mut messages = consumer
            .fetch()
            .max_messages(2)
            .expires(Duration::from_secs(5))
            .messages()
            .await
            .context("fetch the bridge previews")?;
        let mut records = Vec::new();
        while let Some(message) = messages.next().await {
            let message =
                message.map_err(|error| anyhow::anyhow!("read a bridge preview: {error}"))?;
            let record = serde_json::from_slice(&message.payload)
                .context("decode a bridge preview record")?;
            records.push((message.subject.to_string(), record));
            message
                .ack()
                .await
                .map_err(|error| anyhow::anyhow!("ack a bridge preview: {error}"))?;
        }
        Ok(records)
    }

    #[tokio::test]
    #[ignore = "requires disposable WAMN_TAP NATS, PostgreSQL, OCI registry, and built production guests"]
    async fn the_released_router_bridge_emits_accepted_and_settled_previews() -> anyhow::Result<()>
    {
        let nats_url = required("WAMN_ROUTER_TAP_NATS_URL")?;
        let database_url = required("WAMN_ROUTER_TAP_PG_URL")?;
        let artifact_base = required("WAMN_ROUTER_TAP_ARTIFACT_BASE")?;
        let node_wasm = PathBuf::from(required("WAMN_ROUTER_TAP_NODE_WASM")?);
        let flow_http_wasm = PathBuf::from(required("WAMN_ROUTER_TAP_FLOW_HTTP_WASM")?);

        let client = async_nats::connect(&nats_url)
            .await
            .context("connect to the disposable tap NATS")?;
        let jetstream_context = async_nats::jetstream::new(client);
        let mut stream = jetstream_context
            .get_stream(STREAM)
            .await
            .context("the checked-in WAMN_TAP provisioning must have run")?;
        let info = stream.info().await.context("read WAMN_TAP configuration")?;
        let messages_before = info.state.messages;
        ensure!(
            info.config.subjects.len() == 1 && info.config.subjects[0] == "tap.>",
            "WAMN_TAP subject drifted"
        );
        ensure!(
            info.config.retention == RetentionPolicy::Limits,
            "WAMN_TAP retention drifted"
        );
        ensure!(
            info.config.storage == StorageType::Memory,
            "WAMN_TAP storage drifted"
        );
        ensure!(info.config.num_replicas == 1, "WAMN_TAP replicas drifted");
        ensure!(
            info.config.discard == DiscardPolicy::Old,
            "WAMN_TAP discard policy drifted"
        );
        ensure!(
            info.config.max_age == Duration::from_secs(300),
            "WAMN_TAP max age drifted"
        );
        ensure!(
            info.config.max_messages == 200_000,
            "WAMN_TAP max messages drifted"
        );
        ensure!(
            info.config.max_messages_per_subject == 16,
            "WAMN_TAP per-subject bound drifted"
        );
        ensure!(
            info.config.max_bytes == 64 * 1024 * 1024,
            "WAMN_TAP byte bound drifted"
        );
        ensure!(
            info.config.max_message_size == 128 * 1024,
            "WAMN_TAP message bound drifted"
        );
        ensure!(
            info.config.duplicate_window == Duration::from_secs(120),
            "WAMN_TAP dedup drifted"
        );
        // The route fixture declares its organization as TENANT in WorkloadRoleScope.
        let event_scope = wamn_control_registry::Triple::new(TENANT, PROJECT, ENVIRONMENT);
        let event_stream = wamn_event_wire::stream_name(TENANT, PROJECT, ENVIRONMENT);
        let advisory_stream = wamn_event_wire::delivery_advisory_stream(&event_stream);
        let result: anyhow::Result<()> = async {
            wamn_ctl::event_streams::provision(
                &jetstream_context,
                &event_scope,
                1,
                Duration::from_secs(120),
                &[],
            )
            .await?;
            let (port, served) = upstream_origin().await?;
            let route = trusted_http_route::build(&RouteOptions {
                database_url,
                artifact_base,
                component_wasm: node_wasm,
                upstream_base_url: format!("http://127.0.0.1:{port}"),
                path_and_query: "/effect".to_owned(),
            })
            .await
            .context("build the real released RouterDriver closure")?;
            let jetstream = Arc::new(
                WamnJetstream::new(WamnJetstreamConfig {
                    nats_url: Some(nats_url),
                    event_scope: Some(event_scope),
                    stream_replicas: Some(1),
                    dup_window_secs: Some(120),
                    ..Default::default()
                })
                .with_release(Some(Arc::clone(&route.release))),
            );
            jetstream
                .activate_events()
                .await
                .map_err(|error| anyhow::anyhow!("activate test event streams: {error:?}"))?;
            let bridge = Arc::new(
                RouterDeliveryBridge::new(
                    Arc::clone(&route.driver),
                    Arc::clone(&route.release),
                    Arc::clone(&jetstream),
                    PROJECT,
                )
                .context("bind the bridge's release-derived tap identity")?,
            );
            let routing = Arc::new(
                FlowHttpRouting::from_env(Some(Arc::clone(&route.release)))
                    .context("build release-backed flow-http routing")?,
            );
            let consumer = stream
                .create_consumer(PullConfig {
                    deliver_policy: DeliverPolicy::New,
                    ack_policy: AckPolicy::Explicit,
                    filter_subject: format!("tap.{TENANT}.{PROJECT}.{ENVIRONMENT}.{WIRING_ID}.>"),
                    inactive_threshold: Duration::from_secs(120),
                    ..Default::default()
                })
                .await
                .context("create a new-only exact-scope tap consumer")?;
            let input =
                Bytes::from_static(br#"{"api_key":"must-not-reach-the-tap","plain":"visible"}"#);
            let response = invoke_flow_http(flow_http_wasm, routing, bridge, input.clone())
                .await
                .context("drive the production ingress-to-router path")?;
            ensure!(
                response.status() == StatusCode::OK,
                "released route returned {}",
                response.status()
            );
            let response_body: serde_json::Value = serde_json::from_slice(response.body())
                .context("decode released route response")?;
            ensure!(
                response_body["status"] == 200,
                "the real HTTP node did not settle successfully"
            );

            let upstream_body = tokio::time::timeout(Duration::from_secs(10), served)
                .await
                .context("the released node never reached its upstream")?
                .context("the disposable upstream task failed")?;
            ensure!(
                upstream_body.as_slice() == input.as_ref(),
                "the real driver did not execute the released payload"
            );

            let records = read_tap_records(&consumer).await?;
            ensure!(
                stream.info().await?.state.messages == messages_before + 2,
                "one delivered request must store exactly two previews"
            );
            ensure!(
                records.len() == 2,
                "one delivered request must emit exactly two previews, got {}",
                records.len()
            );
            ensure!(
                records[0].0 == records[1].0,
                "accepted and settled must share one delivery subject"
            );
            ensure!(
                records[0].0.starts_with(&format!(
                    "tap.{TENANT}.{PROJECT}.{ENVIRONMENT}.{WIRING_ID}."
                )),
                "the bridge must mint scope from the release: {}",
                records[0].0
            );
            ensure!(
                records[0].1["phase"] == "accepted",
                "first preview is not accepted"
            );
            ensure!(
                records[1].1["phase"] == "settled",
                "second preview is not settled"
            );
            ensure!(
                records[1].1["outcome"] == "respond",
                "settled preview lost the real outcome"
            );
            ensure!(
                records[0].1["payload"]["api_key"] == "[redacted]",
                "accepted preview leaked a secret"
            );
            ensure!(
                records[0].1["payload"]["plain"] == "visible",
                "accepted preview lost safe payload"
            );
            ensure!(
                records[0].1["delivery-id"] == records[1].1["delivery-id"],
                "the two boundaries disagree on delivery identity"
            );
            ensure!(
                records[0].1["source-id"] == ATTACHMENT_ID,
                "tap did not name the released attachment"
            );
            Ok(())
        }
        .await;
        let source_cleanup = jetstream_context.delete_stream(&event_stream).await;
        let advisory_cleanup = jetstream_context.delete_stream(&advisory_stream).await;
        result?;
        source_cleanup.context("remove the test source stream")?;
        advisory_cleanup.context("remove the test advisory stream")?;
        Ok(())
    }
}
