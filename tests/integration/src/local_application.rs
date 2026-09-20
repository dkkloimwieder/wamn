//! Loopback HTTP shell for locally assembled application releases.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use wamn_execution_host::{ROUTER_DELIVERY_ID, RouterDeliveryBridge};
use wamn_runtime::plugins::flow_http_routing::{FLOW_HTTP_ROUTING_ID, FlowHttpRouting};
use wash_runtime::engine::InstancePolicy;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::engine::workload::{WorkloadComponent, WorkloadItem};
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::types::LocalResources;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, Linker};
use wasmtime_wasi_http::p3::bindings::Service;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;

/// Production adapters required by the shipped flow-http component.
#[derive(Clone)]
pub struct LocalApplicationRuntime {
    pub engine: Arc<wash_runtime::engine::Engine>,
    pub flow_http: Component,
    pub routing: Arc<FlowHttpRouting>,
    pub bridge: Arc<RouterDeliveryBridge>,
}

/// Published identities returned with the local HTTP endpoint.
pub struct LocalApplicationConfig {
    pub runtime: LocalApplicationRuntime,
    pub route_host: String,
    pub bearer: String,
    pub caller_secret_path: PathBuf,
    pub tenant: String,
    pub caller_role: String,
    pub component_digests: HashMap<String, String>,
}

/// A loopback endpoint backed by a fresh flow-http store for every request.
pub struct LocalApplication {
    endpoint: String,
    pub route_host: String,
    pub bearer: String,
    pub caller_secret_path: PathBuf,
    pub tenant: String,
    pub caller_role: String,
    pub component_digests: HashMap<String, String>,
    task: JoinHandle<()>,
}

/// Response and guest-memory observation from one flow-http invocation.
pub struct LocalInvocation {
    pub response: Response<Bytes>,
    pub shell_bytes: u64,
    pub peak_bytes: u64,
}

impl LocalApplication {
    pub async fn start(config: LocalApplicationConfig) -> anyhow::Result<Self> {
        let LocalApplicationConfig {
            runtime,
            route_host,
            bearer,
            caller_secret_path,
            tenant,
            caller_role,
            component_digests,
        } = config;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind the local application HTTP endpoint")?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let runtime = runtime.clone();
                        async move {
                            let invocation = invoke_request(runtime, request).await?;
                            Ok::<_, anyhow::Error>(invocation.response.map(Full::new))
                        }
                    });
                    if let Err(error) = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await
                    {
                        tracing::debug!(%error, "local application HTTP connection ended");
                    }
                });
            }
        });
        Ok(Self {
            endpoint: endpoint(address),
            route_host,
            bearer,
            caller_secret_path,
            tenant,
            caller_role,
            component_digests,
            task,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for LocalApplication {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn endpoint(address: SocketAddr) -> String {
    format!("http://{address}")
}

pub async fn invoke_request<B>(
    runtime: LocalApplicationRuntime,
    request: Request<B>,
) -> anyhow::Result<LocalInvocation>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<ErrorCode>,
{
    let raw = runtime.engine.inner();
    let mut linker = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|error| anyhow::anyhow!("link WASI into flow-http: {error}"))?;
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)
        .map_err(|error| anyhow::anyhow!("link wasi:http into flow-http: {error}"))?;
    let loopback = Arc::new(std::sync::Mutex::new(
        wash_runtime::sockets::loopback::Network::default(),
    ));
    let mut workload = WorkloadComponent::new(
        "local-application",
        "local-application",
        "wamn",
        "flow-http",
        runtime.flow_http,
        linker,
        Vec::new(),
        LocalResources::default(),
        loopback,
        InstancePolicy::Ephemeral,
    );
    let imports = workload.world().imports;
    {
        let mut item = WorkloadItem::Component(&mut workload);
        runtime
            .routing
            .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
            .await
            .context("bind released HTTP routing")?;
        runtime
            .bridge
            .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
            .await
            .context("bind router delivery")?;
    }
    let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
    plugins.insert(FLOW_HTTP_ROUTING_ID, runtime.routing);
    plugins.insert(ROUTER_DELIVERY_ID, runtime.bridge);
    let ctx = Ctx::builder(workload.workload_id().to_owned(), workload.id().to_owned())
        .with_plugins(plugins)
        .build();
    let mut store = Store::new(
        raw,
        SharedCtx::new(ctx).with_guest_memory(runtime.engine.guest_memory()),
    );
    wash_runtime::engine::guest_memory::install_memory_limiter(&mut store);
    store.set_epoch_deadline(u64::MAX / 2);
    let compiled = workload.component().clone();
    let service = Service::instantiate_async(&mut store, &compiled, workload.linker())
        .await
        .map_err(|error| anyhow::anyhow!("instantiate shipped flow-http: {error}"))?;
    let (request, request_io) = wasmtime_wasi_http::p3::Request::from_http(request);
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
                let body =
                    body.map_err(|error| anyhow::anyhow!("collect flow-http response: {error:?}"))?;
                Ok::<_, anyhow::Error>(Response::from_parts(parts, body.to_bytes()))
            };
            let io = async {
                if let Err(error) = request_io.await {
                    tracing::debug!(?error, "flow-http request body processing ended");
                }
                Ok::<_, anyhow::Error>(())
            };
            let (response, ()) = tokio::try_join!(handle, io)?;
            Ok::<_, anyhow::Error>(response)
        })
        .await
        .map_err(|error| anyhow::anyhow!("drive flow-http P3 request: {error}"))??;
    let shell_bytes = store.data().memory_limiter.charged();
    let peak_bytes = store.data().memory_limiter.peak();
    Ok(LocalInvocation {
        response,
        shell_bytes,
        peak_bytes,
    })
}
