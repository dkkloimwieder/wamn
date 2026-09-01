//! Live proof that the standard guest virtualization stage removes ambient
//! environment access without hiding guest traps from the serving boundary.
//!
//! Gate recipe: `[STD-GUEST-VIRTUALIZATION]` in `docs/operations/build-and-test.md`.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Context as _, ensure};
    use bytes::Bytes;
    use http_body_util::{BodyExt as _, Full};
    use hyper::{Method, Request, StatusCode};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use wamn_execution_host::{
        ROUTER_DELIVERY_ID, RouterDeliveryBridge, RouterDriverRequest, WiringResolution,
    };
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
    use wash_runtime::wasmtime::component::types::{ComponentInstance, ComponentItem, Type};
    use wash_runtime::wasmtime::component::{Component, Linker};
    use wasmtime_wasi_http::p2::WasiHttpView as _;
    use wasmtime_wasi_http::p2::bindings::Proxy;
    use wasmtime_wasi_http::p2::bindings::http::types::{ErrorCode, Scheme};

    use crate::trusted_http_route::{
        self, ENVIRONMENT, PACKAGE, PROJECT, ROUTE_AUTHORITY, ROUTE_PATH, RouteOptions, TENANT,
        WIRING_ID, WIRING_VERSION,
    };

    const SENTINEL_KEY: &str = "WAMN_STD_VIRTUALIZATION_SENTINEL";
    const RECEIVING_EXPORTS: [&str; 6] = [
        "wamn-receiving:purchase-order/get@1.0.0",
        "wamn-receiving:purchase-order/query@1.0.0",
        "wamn-receiving:purchase-order/update@1.0.0",
        "wamn-receiving:receipt/get@1.0.0",
        "wamn-receiving:receipt/query@1.0.0",
        "wamn-receiving:receiving/record-receipt@1.0.0",
    ];
    const HANDLER_RUN_SHAPE: &str = concat!(
        "run(ctx:record{wiring-id:string,wiring-version:u32,node-id:string,",
        "delivery-id:string,input-port:option<string>,occurrence:u32,",
        "traceparent:option<string>,tracestate:option<string>,",
        "deadline-ms:option<u64>,config:string},input:string)->(",
        "result<record{payload:string,port:option<string>},",
        "variant{retryable:record{message:string,code:option<string>},",
        "rate-limited:record{detail:record{message:string,code:option<string>},",
        "retry-after-ms:option<u64>},terminal:record{message:string,",
        "code:option<string>},invalid-input:record{message:string,",
        "code:option<string>},cancelled}>)"
    );

    fn required(key: &str) -> anyhow::Result<String> {
        std::env::var(key)
            .ok()
            .filter(|value| !value.is_empty())
            .with_context(|| format!("set {key} for the virtualized std guest proof"))
    }

    fn import_packages(
        engine: &wash_runtime::engine::Engine,
        component_bytes: &[u8],
        label: &str,
    ) -> anyhow::Result<BTreeSet<String>> {
        let imports = wamn_runtime::component_imports(engine, component_bytes, label)?;
        Ok(imports
            .iter()
            .map(wamn_component_policy::import_pkg)
            .map(str::to_owned)
            .collect())
    }

    fn type_shape(ty: &Type) -> String {
        match ty {
            Type::Bool => "bool".to_owned(),
            Type::S8 => "s8".to_owned(),
            Type::U8 => "u8".to_owned(),
            Type::S16 => "s16".to_owned(),
            Type::U16 => "u16".to_owned(),
            Type::S32 => "s32".to_owned(),
            Type::U32 => "u32".to_owned(),
            Type::S64 => "s64".to_owned(),
            Type::U64 => "u64".to_owned(),
            Type::Float32 => "float32".to_owned(),
            Type::Float64 => "float64".to_owned(),
            Type::Char => "char".to_owned(),
            Type::String => "string".to_owned(),
            Type::List(list) => format!("list<{}>", type_shape(&list.ty())),
            Type::Map(map) => format!(
                "map<{},{}>",
                type_shape(&map.key()),
                type_shape(&map.value())
            ),
            Type::Record(record) => format!(
                "record{{{}}}",
                record
                    .fields()
                    .map(|field| format!("{}:{}", field.name, type_shape(&field.ty)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Type::Tuple(tuple) => format!(
                "tuple<{}>",
                tuple
                    .types()
                    .map(|ty| type_shape(&ty))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Type::Variant(variant) => format!(
                "variant{{{}}}",
                variant
                    .cases()
                    .map(|case| match case.ty {
                        Some(ty) => format!("{}:{}", case.name, type_shape(&ty)),
                        None => case.name.to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Type::Enum(enumeration) => format!(
                "enum{{{}}}",
                enumeration.names().collect::<Vec<_>>().join(",")
            ),
            Type::Option(option) => format!("option<{}>", type_shape(&option.ty())),
            Type::Result(result) => format!(
                "result<{},{}>",
                result
                    .ok()
                    .as_ref()
                    .map_or_else(|| "_".to_owned(), type_shape),
                result
                    .err()
                    .as_ref()
                    .map_or_else(|| "_".to_owned(), type_shape)
            ),
            Type::Flags(flags) => {
                format!("flags{{{}}}", flags.names().collect::<Vec<_>>().join(","))
            }
            Type::Own(_) => "own<resource>".to_owned(),
            Type::Borrow(_) => "borrow<resource>".to_owned(),
            Type::Future(future) => format!(
                "future<{}>",
                future
                    .ty()
                    .as_ref()
                    .map_or_else(|| "_".to_owned(), type_shape)
            ),
            Type::Stream(stream) => format!(
                "stream<{}>",
                stream
                    .ty()
                    .as_ref()
                    .map_or_else(|| "_".to_owned(), type_shape)
            ),
            Type::ErrorContext => "error-context".to_owned(),
        }
    }

    fn run_shape(
        engine: &wash_runtime::wasmtime::Engine,
        instance: &ComponentInstance,
        operation: &str,
    ) -> anyhow::Result<String> {
        let exports = instance.exports(engine).collect::<Vec<_>>();
        let export_names = exports
            .iter()
            .map(|(name, _)| *name)
            .collect::<BTreeSet<_>>();
        ensure!(
            export_names
                == BTreeSet::from(["emission", "json", "node-context", "node-error", "run"]),
            "Receiving operation {operation:?} exports {export_names:?}, not the pinned handler members"
        );
        let (_, item) = exports
            .into_iter()
            .find(|(name, _)| *name == "run")
            .expect("the exact export-name check found run");
        let ComponentItem::ComponentFunc(run) = &item.ty else {
            anyhow::bail!("Receiving operation {operation:?} run is not a component function");
        };
        ensure!(
            !run.async_(),
            "Receiving operation {operation:?} run unexpectedly uses the async ABI"
        );
        let params = run
            .params()
            .map(|(name, ty)| format!("{name}:{}", type_shape(&ty)))
            .collect::<Vec<_>>()
            .join(",");
        let results = run
            .results()
            .map(|ty| type_shape(&ty))
            .collect::<Vec<_>>()
            .join(",");
        Ok(format!("run({params})->({results})"))
    }

    fn operation_exports(
        engine: &wash_runtime::engine::Engine,
        component_bytes: &[u8],
        label: &str,
    ) -> anyhow::Result<BTreeSet<String>> {
        let component = Component::new(engine.inner(), component_bytes)
            .map_err(|error| anyhow::anyhow!("compile {label}: {error}"))?;
        component
            .component_type()
            .exports(component.engine())
            .map(|(name, item)| {
                let ComponentItem::ComponentInstance(instance) = item.ty else {
                    anyhow::bail!("{label} export {name:?} is not an interface instance");
                };
                let shape = run_shape(component.engine(), &instance, name)?;
                ensure!(
                    shape == HANDLER_RUN_SHAPE,
                    "{label} export {name:?} has run shape {shape:?}, not the pinned handler shape {HANDLER_RUN_SHAPE:?}"
                );
                Ok(name.to_owned())
            })
            .collect()
    }

    async fn connection_origin() -> anyhow::Result<(String, tokio::task::JoinHandle<Vec<u8>>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind the connection-proof origin")?;
        let address = listener
            .local_addr()
            .context("read the connection-proof origin address")?;
        let served = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("the probe connects");
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                let read = socket.read(&mut byte).await.expect("read request head");
                assert_ne!(read, 0, "the probe closed before finishing its head");
                head.push(byte[0]);
            }
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\
                      connection: close\r\n\r\n",
                )
                .await
                .expect("answer the connection probe");
            socket.flush().await.expect("flush the connection answer");
            head
        });
        Ok((format!("http://{address}"), served))
    }

    #[test]
    #[ignore = "requires the built and virtualized std probe and Receiving package component"]
    fn virtualized_artifacts_have_exact_imports_and_receiving_exports() -> anyhow::Result<()> {
        let probe = PathBuf::from(required("WAMN_STD_VIRTUALIZATION_COMPONENT_WASM")?);
        let receiving_directory =
            PathBuf::from(required("WAMN_STD_VIRTUALIZATION_RECEIVING_DIRECTORY")?);
        let engine = build_engine(&[]).context("build the artifact-inspection engine")?;

        let probe_bytes =
            std::fs::read(&probe).with_context(|| format!("read {}", probe.display()))?;
        let probe_packages = import_packages(&engine, &probe_bytes, "virtualized std probe")?;
        ensure!(
            probe_packages
                == BTreeSet::from([
                    "wamn:connection".to_owned(),
                    "wamn:node".to_owned(),
                    "wasi:clocks".to_owned(),
                    "wasi:io".to_owned(),
                ]),
            "virtualized std probe imports {probe_packages:?}, not its exact four-package profile"
        );

        let expected_receiving = BTreeSet::from([
            "wamn:node".to_owned(),
            "wamn:postgres".to_owned(),
            "wasi:clocks".to_owned(),
            "wasi:io".to_owned(),
        ]);
        let path = receiving_directory.join("receiving.wasm");
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read Receiving artifact {}", path.display()))?;
        let packages = import_packages(&engine, &bytes, "receiving")?;
        ensure!(
            packages == expected_receiving,
            "virtualized Receiving artifact imports {packages:?}, not its exact four-package profile"
        );
        let exports = operation_exports(&engine, &bytes, "receiving")?;
        let expected_exports = RECEIVING_EXPORTS.map(str::to_owned).into_iter().collect();
        ensure!(
            exports == expected_exports,
            "virtualized Receiving artifact exports {exports:?}, not {expected_exports:?}"
        );
        println!(
            "receiving-component-bytes={} digest={}",
            bytes.len(),
            wamn_runtime::component_admission::component_digest(&bytes)
        );
        Ok(())
    }

    async fn invoke_flow_http(
        component_path: &Path,
        routing: Arc<FlowHttpRouting>,
        bridge: Arc<RouterDeliveryBridge>,
        body: Bytes,
    ) -> anyhow::Result<hyper::Response<Bytes>> {
        let engine = build_engine(&[]).context("build the flow-http engine")?;
        let raw = engine.inner();
        let component_bytes = std::fs::read(component_path)
            .with_context(|| format!("read {}", component_path.display()))?;
        let component = Component::new(raw, &component_bytes)
            .map_err(|error| anyhow::anyhow!("compile flow-http: {error}"))?;
        let mut linker = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|error| anyhow::anyhow!("link WASI into flow-http: {error}"))?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
            .map_err(|error| anyhow::anyhow!("link wasi:http into flow-http: {error}"))?;
        let loopback = Arc::new(std::sync::Mutex::new(
            wash_runtime::sockets::loopback::Network::default(),
        ));
        let mut workload = WorkloadComponent::new(
            "virtualized-std-guest",
            "virtualized-std-guest",
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
        let mut store = Store::new(raw, SharedCtx::new(ctx));
        store.set_epoch_deadline(u64::MAX / 2);
        let compiled = workload.component().clone();
        let proxy = Proxy::instantiate_async(&mut store, &compiled, workload.linker())
            .await
            .map_err(|error| anyhow::anyhow!("instantiate the shipped flow-http guest: {error}"))?;

        let body = Full::new(body).map_err(|never| -> ErrorCode { match never {} });
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("http://{ROUTE_AUTHORITY}{ROUTE_PATH}"))
            .header("content-type", "application/json")
            .body(body)
            .context("build the proof request")?;
        let incoming = store
            .data_mut()
            .http()
            .new_incoming_request(Scheme::Http, request)
            .map_err(|error| anyhow::anyhow!("lower the proof request: {error}"))?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let out = store
            .data_mut()
            .http()
            .new_response_outparam(sender)
            .map_err(|error| anyhow::anyhow!("allocate the response outparam: {error}"))?;
        let call = wasmtime_wasi::runtime::spawn(async move {
            proxy
                .wasi_http_incoming_handler()
                .call_handle(&mut store, incoming, out)
                .await
                .map_err(|error| anyhow::anyhow!("call flow-http: {error}"))
        });
        let response = receiver
            .await
            .context("flow-http did not set its response")?
            .map_err(|error| anyhow::anyhow!("flow-http returned {error:?}"))?;
        let (parts, body) = response.into_parts();
        let body = body.collect().await.context("collect flow-http response")?;
        call.await.context("join flow-http")?;
        Ok(hyper::Response::from_parts(parts, body.to_bytes()))
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL and OCI, plus built virtualized probe and flow-http artifacts"]
    async fn virtualized_std_guest_hides_the_sentinel_and_maps_a_panic_to_a_typed_refusal()
    -> anyhow::Result<()> {
        required(SENTINEL_KEY)?;
        let database_url = required("WAMN_STD_VIRTUALIZATION_PG_URL")?;
        let artifact_base = required("WAMN_STD_VIRTUALIZATION_ARTIFACT_BASE")?;
        let component_wasm = PathBuf::from(required("WAMN_STD_VIRTUALIZATION_COMPONENT_WASM")?);
        let flow_http_wasm = PathBuf::from(required("WAMN_STD_VIRTUALIZATION_FLOW_HTTP_WASM")?);

        let engine = build_engine(&[]).context("build the artifact-inspection engine")?;
        let component_bytes = std::fs::read(&component_wasm)
            .with_context(|| format!("read {}", component_wasm.display()))?;
        let packages = import_packages(&engine, &component_bytes, "virtualized std probe")?;
        ensure!(
            packages
                == BTreeSet::from([
                    "wamn:connection".to_owned(),
                    "wamn:node".to_owned(),
                    "wasi:clocks".to_owned(),
                    "wasi:io".to_owned(),
                ]),
            "virtualized std probe imports {packages:?}, not the exact four-package profile"
        );

        let (upstream_base_url, served) = connection_origin().await?;
        let route = trusted_http_route::build(&RouteOptions {
            database_url,
            artifact_base,
            component_wasm,
            upstream_base_url,
            path_and_query: "/connection-proof".to_owned(),
        })
        .await
        .context("build the released virtualized probe route")?;

        let delivery = route
            .driver
            .execute(RouterDriverRequest {
                tenant_id: TENANT.to_owned(),
                package_id: PACKAGE.to_owned(),
                environment: ENVIRONMENT.to_owned(),
                wiring_id: WIRING_ID.to_owned(),
                wiring_version: WIRING_VERSION,
                delivery_id: "virtualized-environment-proof".to_owned(),
                payload: serde_json::json!({"proof": "environment"}),
                caller_attached: true,
                resolution: WiringResolution::Active,
                caller: None,
                traceparent: None,
                tracestate: None,
            })
            .await
            .context("run the virtualized probe through RouterDriver")?;
        ensure!(
            delivery.outcome.result["sentinel-key"] == SENTINEL_KEY,
            "the guest and native proof disagree on the sentinel identity"
        );
        ensure!(
            delivery.outcome.result["sentinel-visible"] == false,
            "a virtualized std guest observed {SENTINEL_KEY} from its host process"
        );

        let connection = route
            .driver
            .execute(RouterDriverRequest {
                tenant_id: TENANT.to_owned(),
                package_id: PACKAGE.to_owned(),
                environment: ENVIRONMENT.to_owned(),
                wiring_id: WIRING_ID.to_owned(),
                wiring_version: WIRING_VERSION,
                delivery_id: "virtualized-connection-proof".to_owned(),
                payload: serde_json::json!({"proof": "connection"}),
                caller_attached: true,
                resolution: WiringResolution::Active,
                caller: None,
                traceparent: None,
                tracestate: None,
            })
            .await
            .context("exercise the probe's admitted connection import")?;
        ensure!(
            connection.outcome.result["connection-status"] == 204,
            "the virtualized probe did not observe the loopback origin's 204 response"
        );
        let request_head = tokio::time::timeout(Duration::from_secs(10), served)
            .await
            .context("the virtualized probe did not reach its loopback origin")?
            .context("the loopback origin task failed")?;
        ensure!(
            request_head.starts_with(b"POST /connection-proof HTTP/1.1\r\n"),
            "the connection proof reached the wrong operation: {}",
            String::from_utf8_lossy(&request_head)
        );

        let jetstream = Arc::new(WamnJetstream::new(WamnJetstreamConfig { nats_url: None }));
        let bridge = Arc::new(
            RouterDeliveryBridge::new(
                Arc::clone(&route.driver),
                Arc::clone(&route.release),
                jetstream,
                PROJECT,
            )
            .context("bind the production delivery bridge")?,
        );
        let routing = Arc::new(
            FlowHttpRouting::from_env(Some(Arc::clone(&route.release)))
                .context("build release-backed HTTP routing")?,
        );
        let response = invoke_flow_http(
            &flow_http_wasm,
            routing,
            bridge,
            Bytes::from_static(br#"{"proof":"panic"}"#),
        )
        .await
        .context("drive the deliberate panic through released ingress")?;
        ensure!(
            response.status() == StatusCode::SERVICE_UNAVAILABLE,
            "a trapped guest mapped to HTTP {} instead of 503",
            response.status()
        );
        let body: serde_json::Value =
            serde_json::from_slice(response.body()).context("decode the typed refusal")?;
        ensure!(
            body["error"]["code"] == "execution-failed",
            "a trapped guest did not map to the execution-failed refusal: {body}"
        );
        Ok(())
    }
}
