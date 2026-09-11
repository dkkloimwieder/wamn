//! Receiving runtime tests and helpers.

use super::*;


pub(super) struct TraceHarness {
    pub(super) exporter: InMemorySpanExporter,
    pub(super) provider: SdkTracerProvider,
    pub(super) _guard: tracing::subscriber::DefaultGuard,
}

impl TraceHarness {
    pub(super) fn install() -> Self {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
        let exporter = InMemorySpanExporterBuilder::new().build();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer("receiving-route-live")),
        );
        let guard = tracing::subscriber::set_default(subscriber);
        Self {
            exporter,
            provider,
            _guard: guard,
        }
    }

    pub(super) fn spans(&self) -> Vec<SpanData> {
        self.provider
            .force_flush()
            .expect("Receiving route spans must flush");
        self.exporter
            .get_finished_spans()
            .expect("Receiving route span exporter must remain readable")
    }
}

pub(super) fn journey_trace(index: u64) -> (String, String) {
    let trace_id = format!("{index:032x}");
    let span_id = format!("{index:016x}");
    (trace_id.clone(), format!("00-{trace_id}-{span_id}-01"))
}

pub(super) fn span_attribute(span: &SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| attribute.value.to_string())
}

pub(super) fn span_descends_from(spans: &[SpanData], span: &SpanData, ancestor: &SpanData) -> bool {
    let trace_id = span.span_context.trace_id();
    let ancestor_id = ancestor.span_context.span_id();
    let mut parent_id = span.parent_span_id;
    for _ in 0..=spans.len() {
        if parent_id == ancestor_id {
            return true;
        }
        let Some(parent) = spans.iter().find(|candidate| {
            candidate.span_context.trace_id() == trace_id
                && candidate.span_context.span_id() == parent_id
        }) else {
            return false;
        };
        parent_id = parent.parent_span_id;
    }
    false
}

pub(super) fn trace_component_invocations<'a>(spans: &'a [SpanData], trace_id: &str) -> Vec<&'a SpanData> {
    spans
        .iter()
        .filter(|span| {
            span.name == "wamn.component.invoke"
                && span.span_context.trace_id().to_string() == trace_id
        })
        .collect()
}

pub(super) fn assert_invocation_identity(
    component: &SpanData,
    trace_id: &str,
    wiring_id: &str,
    operation: &str,
    component_digest: &str,
    caller_principal_id: &str,
) {
    assert_eq!(
        span_attribute(component, "wamn.wiring_id").as_deref(),
        Some(wiring_id),
        "trace {trace_id} reached a different released wiring"
    );
    assert_eq!(
        span_attribute(component, "wamn.project").as_deref(),
        Some(PROJECT),
        "trace {trace_id} escaped the Receiving project"
    );
    assert_eq!(
        span_attribute(component, "wamn.component_digest").as_deref(),
        Some(component_digest),
        "trace {trace_id} invoked a different released component"
    );
    assert_eq!(
        span_attribute(component, "wamn.operation").as_deref(),
        Some(operation),
        "trace {trace_id} invoked a different operation"
    );
    assert_eq!(
        span_attribute(component, "wamn.caller_principal_id").as_deref(),
        Some(caller_principal_id),
        "trace {trace_id} did not preserve the originating caller"
    );
}

pub(super) fn assert_postgres_descendants<'a>(
    spans: &'a [SpanData],
    trace_id: &str,
    ancestor: &SpanData,
) -> Vec<&'a SpanData> {
    let postgres = spans
        .iter()
        .filter(|span| {
            span.name == "wamn.postgres"
                && span.span_context.trace_id().to_string() == trace_id
                && span_descends_from(spans, span, ancestor)
        })
        .collect::<Vec<_>>();
    assert!(
        !postgres.is_empty(),
        "trace {trace_id} contains no PostgreSQL effect below the expected component invocation"
    );
    postgres
}

pub(super) fn assert_direct_route_trace(
    spans: &[SpanData],
    trace_id: &str,
    wiring_id: &str,
    operation: &str,
    component_digest: &str,
    caller_principal_id: &str,
) {
    let components = trace_component_invocations(spans, trace_id);
    assert_eq!(
        components.len(),
        1,
        "trace {trace_id} must contain one released component invocation"
    );
    let component = components[0];
    assert_invocation_identity(
        component,
        trace_id,
        wiring_id,
        operation,
        component_digest,
        caller_principal_id,
    );
    assert_eq!(
        span_attribute(component, "wamn.node_id").as_deref(),
        Some("operation"),
        "trace {trace_id} invoked a different wiring node"
    );
    let postgres = assert_postgres_descendants(spans, trace_id, component);
    assert_eq!(
        postgres.len(),
        spans
            .iter()
            .filter(|span| {
                span.name == "wamn.postgres" && span.span_context.trace_id().to_string() == trace_id
            })
            .count(),
        "trace {trace_id} contains a PostgreSQL effect outside its component invocation"
    );
}

pub(super) fn assert_nested_record_receipt_trace(
    spans: &[SpanData],
    trace_id: &str,
    overlay_digest: &str,
    base_digest: &str,
    caller_principal_id: &str,
    credential_kind: &str,
) {
    let components = trace_component_invocations(spans, trace_id);
    assert_eq!(
        components.len(),
        2,
        "trace {trace_id} must contain overlay and pinned-base invocations"
    );
    let overlay = components
        .iter()
        .find(|span| {
            span_attribute(span, "wamn.operation").as_deref() == Some(OVERLAY_RECORD_RECEIPT)
        })
        .copied()
        .expect("overlay record_receipt invocation is present");
    let base = components
        .iter()
        .find(|span| span_attribute(span, "wamn.operation").as_deref() == Some(BASE_RECORD_RECEIPT))
        .copied()
        .expect("pinned base record_receipt invocation is present");
    assert_invocation_identity(
        overlay,
        trace_id,
        "receiving_record_receipt",
        OVERLAY_RECORD_RECEIPT,
        overlay_digest,
        caller_principal_id,
    );
    assert_invocation_identity(
        base,
        trace_id,
        "receiving_record_receipt",
        BASE_RECORD_RECEIPT,
        base_digest,
        caller_principal_id,
    );
    for invocation in [overlay, base] {
        assert_eq!(
            span_attribute(invocation, "wamn.caller_credential_kind").as_deref(),
            Some(credential_kind),
            "trace {trace_id} did not preserve the originating credential kind"
        );
    }
    assert!(
        span_descends_from(spans, base, overlay),
        "trace {trace_id} did not parent the pinned-base invocation under the overlay invocation"
    );
    assert_postgres_descendants(spans, trace_id, base);
    assert_postgres_descendants(spans, trace_id, overlay);
}

pub(super) fn assert_native_nested_acquisition(
    spans: &[SpanData],
    trace_id: &str,
    overlay_digest: &str,
    base_digest: &str,
) {
    // The native workload loads the full release once. Other admitted nodes can
    // appear in that preload, while this invocation executes exactly two nodes.
    let loaded = spans
        .iter()
        .filter(|span| span.name == "wamn.component.pull")
        .filter_map(|span| span_attribute(span, "wamn.component_digest"))
        .collect::<BTreeSet<_>>();
    for digest in [overlay_digest, base_digest] {
        assert!(
            loaded.contains(digest),
            "native release never loaded {digest}"
        );
    }
    let invoked = trace_component_invocations(spans, trace_id);
    assert_eq!(
        invoked.len(),
        2,
        "native nested call must execute two nodes"
    );
    let digests = invoked
        .iter()
        .filter_map(|span| span_attribute(span, "wamn.component_digest"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        digests,
        BTreeSet::from([overlay_digest.to_owned(), base_digest.to_owned()]),
        "native nested call executed the wrong admitted components"
    );
}

pub(super) fn assert_nested_permission_denial_trace(
    spans: &[SpanData],
    trace_id: &str,
    overlay_digest: &str,
    base_digest: &str,
    caller_principal_id: &str,
) {
    let components = trace_component_invocations(spans, trace_id);
    assert_eq!(
        components.len(),
        1,
        "trace {trace_id} reached a component after the nested permission refusal"
    );
    let overlay = components[0];
    assert_invocation_identity(
        overlay,
        trace_id,
        "receiving_record_receipt",
        OVERLAY_RECORD_RECEIPT,
        overlay_digest,
        caller_principal_id,
    );
    assert!(
        components.iter().all(|span| {
            span_attribute(span, "wamn.operation").as_deref() != Some(BASE_RECORD_RECEIPT)
                && span_attribute(span, "wamn.component_digest").as_deref() != Some(base_digest)
        }),
        "trace {trace_id} invoked the denied pinned-base operation"
    );
}

pub(super) fn assert_no_component_trace(spans: &[SpanData], trace_id: &str) {
    assert!(
        spans.iter().all(|span| {
            span.name != "wamn.component.invoke"
                || span.span_context.trace_id().to_string() != trace_id
        }),
        "refused trace {trace_id} reached a released component"
    );
}

pub(super) fn journey_postgres(credentials: &JourneyCredentials) -> anyhow::Result<Arc<WamnPostgres>> {
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 4,
        platform_pool_max_size: 4,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
        row_limit: 10_000,
    };
    let configuration = serde_json::json!({
        PROJECT: {
            "credentials": {
                (AuthorityClass::GuestSql.as_str()): credentials.guest_sql,
                (AuthorityClass::ExecutorPlatform.as_str()): credentials.executor_platform,
                (AuthorityClass::EventMaterializer.as_str()): credentials.event_materializer,
                (AuthorityClass::CallableHttp.as_str()): credentials.http_admitter,
            }
        }
    });
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    let provider: Arc<dyn CredentialProvider> =
        Arc::new(StaticCredentialProvider::new(projects, None));
    Ok(Arc::new(WamnPostgres::with_provider(provider)))
}

pub(super) async fn build_journey_runtime(
    inputs: &JourneyDocument,
    credentials: &JourneyCredentials,
    release: Arc<ReleaseManifestWeld>,
    session_verifier: Option<SessionVerifier>,
) -> anyhow::Result<(
    Arc<wash_runtime::engine::Engine>,
    Component,
    Arc<FlowHttpRouting>,
    Arc<RouterDeliveryBridge>,
    tokio::task::JoinHandle<()>,
)> {
    anyhow::ensure!(
        std::fs::read_dir(&inputs.compilation_cache_directory)
            .with_context(|| {
                format!(
                    "read cold compilation cache {}",
                    inputs.compilation_cache_directory.display()
                )
            })?
            .next()
            .is_none(),
        "Receiving journey compilation cache must be empty before runtime construction"
    );
    let postgres = journey_postgres(credentials)?;
    let source = ComponentArtifactSource::new(
        ComponentArtifactSourceConfig::new(
            &inputs.component_artifact_base,
            true,
            REGISTRY_IO_TIMEOUT,
        )?
        .with_registry_auth_file(&inputs.registry_auth_file)?,
    );
    let engine = Arc::new(
        build_engine_with_host_memory_and_compilation_cache(
            &[],
            default_host_memory_budgets(),
            &inputs.compilation_cache_directory,
        )
        .context("build the cached Receiving router engine")?,
    );
    let driver = Arc::new(RouterDriver::new(
        Arc::clone(&engine),
        Arc::clone(&postgres),
        Arc::new(wamn_runtime::plugins::connection_http::transport::HttpTransport::new()?),
        Arc::new(WamnCredentials::empty()),
        Arc::new(WamnLogging::new(WamnLoggingConfig::default())?),
        Arc::from(Vec::<AllowedHost>::new()),
        Arc::clone(&release),
        source,
        RouterDriverConfig {
            owner_prefix: "receiving-route-live".to_owned(),
            project: PROJECT.to_owned(),
            schema: Some("receiving".to_owned()),
            cache_capacity: WiringCacheCapacity::default(),
        },
    )?);
    let jetstream = Arc::new(
        WamnJetstream::new(WamnJetstreamConfig { nats_url: None, ..Default::default() })
            .with_release(Some(Arc::clone(&release))),
    );
    let bridge = Arc::new(RouterDeliveryBridge::new(
        driver,
        Arc::clone(&release),
        jetstream,
        PROJECT,
    )?);
    let (identity_reader, identity_task) = connect(&credentials.identity_reader).await?;
    let mut routing = FlowHttpRouting::new(Some(release), RouteInFlightLimit::default())
        .with_authentication(Arc::new(
            RouteAuthentication::new(
                identity_reader,
                Arc::clone(&postgres),
                ORG,
                PROJECT,
                route_caller_subject(ORG, PROJECT, ENVIRONMENT)?,
            )
            .await?,
        ));
    if let Some(verifier) = session_verifier {
        routing = routing.with_session_authentication(Arc::new(SessionRouteAuthentication::new(
            verifier, postgres, PROJECT,
        )));
    }
    let routing = Arc::new(routing);
    let raw = engine.inner();
    let flow_http_bytes = std::fs::read(&inputs.flow_http_wasm)
        .with_context(|| format!("read {}", inputs.flow_http_wasm.display()))?;
    let flow_http = Component::new(raw, &flow_http_bytes)
        .map_err(|error| anyhow::anyhow!("compile flow-http: {error}"))?;
    Ok((engine, flow_http, routing, bridge, identity_task))
}

#[derive(Clone, Debug)]
pub(super) struct JourneyGuestMemory {
    pub(super) shell_bytes: u64,
    pub(super) peak_bytes: u64,
}

pub(super) async fn invoke_journey_route(
    engine: &wash_runtime::engine::Engine,
    flow_http: &Component,
    routing: Arc<FlowHttpRouting>,
    bridge: Arc<RouterDeliveryBridge>,
    route_host: &str,
    path: &str,
    bearer: Option<&str>,
    traceparent: &str,
    body: Bytes,
) -> anyhow::Result<hyper::Response<Bytes>> {
    let body = Full::new(body).map_err(|never| -> ErrorCode { match never {} });
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("http://{route_host}{path}"))
        .header("content-type", "application/json")
        .header("traceparent", traceparent);
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    let request = request
        .body(body)
        .context("build the Receiving HTTP request")?;
    invoke_journey_request(engine, flow_http, routing, bridge, request).await
}

pub(super) async fn invoke_journey_request<B>(
    engine: &wash_runtime::engine::Engine,
    flow_http: &Component,
    routing: Arc<FlowHttpRouting>,
    bridge: Arc<RouterDeliveryBridge>,
    request: Request<B>,
) -> anyhow::Result<hyper::Response<Bytes>>
where
    B: hyper::body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<ErrorCode>,
{
    let raw = engine.inner();
    let mut linker = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|error| anyhow::anyhow!("link WASI into flow-http: {error}"))?;
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)
        .map_err(|error| anyhow::anyhow!("link wasi:http into flow-http: {error}"))?;
    let loopback = Arc::new(std::sync::Mutex::new(
        wash_runtime::sockets::loopback::Network::default(),
    ));
    let mut workload = WorkloadComponent::new(
        "receiving-route-live",
        "receiving-route-live",
        "wamn",
        "flow-http",
        flow_http.clone(),
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
            .context("bind the released HTTP routing plugin")?;
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
        .map_err(|error| anyhow::anyhow!("instantiate shipped flow-http: {error}"))?;

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
                let body =
                    body.map_err(|error| anyhow::anyhow!("collect flow-http response: {error:?}"))?;
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
    let shell_bytes = store.data().memory_limiter.charged();
    anyhow::ensure!(
        engine.guest_memory().in_use() == shell_bytes,
        "Receiving invocation retained memory outside its flow-http store"
    );
    let memory = JourneyGuestMemory {
        shell_bytes,
        peak_bytes: engine.guest_memory().high_water(),
    };
    drop(store);
    anyhow::ensure!(
        engine.guest_memory().in_use() == 0,
        "Receiving invocation retained guest memory after its stores dropped"
    );
    let mut response = response;
    response.extensions_mut().insert(memory);
    Ok(response)
}

pub(super) fn successful_value(response: &hyper::Response<Bytes>, request_id: &str) -> anyhow::Result<Value> {
    anyhow::ensure!(
        response.status() == StatusCode::OK,
        "request {request_id} returned {}: {}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
    let body: Value = serde_json::from_slice(response.body())
        .with_context(|| format!("decode response for {request_id}"))?;
    let item = body
        .as_array()
        .filter(|items| items.len() == 1)
        .and_then(|items| items.first())
        .with_context(|| format!("request {request_id} returned a non-unit envelope: {body}"))?;
    anyhow::ensure!(
        item["request_id"] == request_id && item.get("error").is_none(),
        "request {request_id} returned a refusal or lost correlation: {item}"
    );
    item.get("value")
        .cloned()
        .with_context(|| format!("request {request_id} returned no value: {item}"))
}
