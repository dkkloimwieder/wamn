//! WASI HTTP shell over authoritative routing, auth, and router delivery.

#[allow(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.61 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:flow-http/flow-http@0.1.0",
        path: [
            "../../../../crates/platform/runtime/wit/deps/wamn-flow-http-routing",
            "../../../../crates/execution/host/wit/deps/wamn-router-delivery",
            "../../../../crates/execution/host/wit/deps/wamn-router-delivery-0.2",
            "../../execution/materializer/wit/deps/wasi-clocks",
            "wit",
        ],
        generate_all,
        async: [
            "export:wasi:http/handler@0.3.0#handle",
            "wamn:flow-http-routing/routing@0.1.0#authenticate",
            "wamn:router-delivery/delivery@0.2.0#deliver",
            "wamn:router-delivery/delivery@0.2.0#deliver-stream",
        ],
    });
}

use bindings::exports::wasi::http::handler::Guest;
use bindings::wasi::http::types::{ErrorCode, Fields, Method, Request, Response};
use wit_bindgen::rt::async_support::{
    FutureReader, FutureWriter, StreamReader, StreamResult, spawn_local,
};

use super::{
    AdapterLimits, AuthRejection, Backend, BodyReadError, BodyReader, Cardinality,
    DeadlineAdjustment, DeliveryError, DeliveryFailure, DeliveryFailureKind, DeliveryOutcome,
    DeliveryReport, DeliveryRequest, Emission, FailedOutcome, Header, HttpResponse, Mapping,
    MappingSource, PartialCompletion, ProviderError, RequestHead, RouteDefinition, SchemaInvalid,
    StreamedHead, handle_request,
};

struct Component;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let head = request_head(&request);
        let mut backend = GuestBackend::default();
        let mut body = WasiBody {
            request: Some(request),
            stream: None,
            trailers: None,
            result: None,
        };
        let response =
            handle_request(&mut backend, &mut body, &head, AdapterLimits::default()).await;
        drop(body);
        Ok(send_response(response, backend))
    }
}

bindings::export!(Component with_types_in bindings);

/// The guest's imports, and the reply lines and route permit of a streamed
/// read, which outlive `handle_request`.
#[derive(Default)]
struct GuestBackend {
    lines: Option<StreamReader<String>>,
    permit: Option<bindings::wamn::flow_http_routing::routing::RoutePermit>,
}

impl Backend for GuestBackend {
    type RoutePermit = bindings::wamn::flow_http_routing::routing::RoutePermit;
    type AuthenticatedCaller = bindings::wamn::flow_http_routing::routing::AuthenticatedCaller;

    fn routes(
        &mut self,
        method: &str,
        authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError> {
        let routes = bindings::wamn::flow_http_routing::routing::routes(method, authority)
            .map_err(|_| ProviderError)?;
        routes.into_iter().map(route_definition).collect()
    }

    async fn authenticate(
        &mut self,
        attachment_id: &str,
        headers: &[Header],
    ) -> Result<Option<Self::AuthenticatedCaller>, AuthRejection> {
        let headers = headers
            .iter()
            .map(
                |header| bindings::wamn::flow_http_routing::routing::Header {
                    name: header.name.clone(),
                    value: header.value.clone(),
                },
            )
            .collect::<Vec<_>>();
        bindings::wamn::flow_http_routing::routing::authenticate(attachment_id.to_owned(), headers)
            .await
            .map_err(|rejection| AuthRejection {
                status: rejection.status,
                code: rejection.code,
            })
    }

    fn validate_input(&mut self, attachment_id: &str, payload: &str) -> Result<(), SchemaInvalid> {
        bindings::wamn::flow_http_routing::routing::validate_input(attachment_id, payload)
            .map_err(SchemaInvalid::from_refusal)
    }

    fn try_acquire_route(
        &mut self,
        attachment_id: &str,
    ) -> Result<Option<Self::RoutePermit>, ProviderError> {
        bindings::wamn::flow_http_routing::routing::try_acquire(attachment_id)
            .map_err(|_| ProviderError)
    }

    fn new_delivery_id(&mut self) -> String {
        const RANDOM_BYTES: u64 = 16;
        hex(&bindings::wasi::random::random::get_random_bytes(
            RANDOM_BYTES,
        ))
    }

    async fn deliver(
        &mut self,
        request: DeliveryRequest<Self::AuthenticatedCaller>,
    ) -> DeliveryReport {
        delivery_report(
            bindings::wamn::router_delivery0_2_0::delivery::deliver(wire_request(request)).await,
        )
    }

    async fn deliver_stream(
        &mut self,
        request: DeliveryRequest<Self::AuthenticatedCaller>,
    ) -> Result<StreamedHead, DeliveryReport> {
        match bindings::wamn::router_delivery0_2_0::delivery::deliver_stream(wire_request(request))
            .await
        {
            Ok(reply) => {
                self.lines = Some(reply.lines);
                Ok(StreamedHead { etag: reply.etag })
            }
            Err(report) => Err(delivery_report(report)),
        }
    }

    fn keep_route_permit(&mut self, permit: Self::RoutePermit) {
        self.permit = Some(permit);
    }
}

fn wire_request(
    request: DeliveryRequest<bindings::wamn::flow_http_routing::routing::AuthenticatedCaller>,
) -> bindings::wamn::router_delivery0_1_0::delivery::DeliveryRequest {
    use bindings::wamn::router_delivery0_1_0::delivery;

    delivery::DeliveryRequest {
        source: delivery::Source::Attachment(request.attachment_id),
        delivery_id: request.delivery_id,
        payload: request.payload,
        caller: request.caller,
        trace: request.trace.map(|trace| delivery::TraceContext {
            traceparent: trace.traceparent,
            tracestate: trace.tracestate,
        }),
        parent_causation: None,
        if_none_match: request.if_none_match,
    }
}

fn delivery_report(
    report: bindings::wamn::router_delivery0_1_0::delivery::DeliveryReport,
) -> DeliveryReport {
    DeliveryReport {
        actor_labels: report.actor_labels,
        etag: report.etag,
        outcome: report
            .outcome
            .map(convert_delivery_outcome)
            .map_err(convert_delivery_error),
        deadline_adjustments: report
            .deadline_adjustments
            .into_iter()
            .map(|adjustment| DeadlineAdjustment {
                node: adjustment.node,
                requested_ms: adjustment.requested_ms,
                effective_ms: adjustment.effective_ms,
            })
            .collect(),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn convert_delivery_outcome(
    outcome: bindings::wamn::router_delivery0_1_0::delivery::DeliveryOutcome,
) -> DeliveryOutcome {
    use bindings::wamn::router_delivery0_1_0::delivery;

    match outcome {
        delivery::DeliveryOutcome::Respond(payload) => DeliveryOutcome::Respond(payload),
        delivery::DeliveryOutcome::Emit(emission) => DeliveryOutcome::Emit(Emission {
            event: emission.event,
            dedup_id: emission.dedup_id,
        }),
        delivery::DeliveryOutcome::Discard => DeliveryOutcome::Discard,
        delivery::DeliveryOutcome::Failed(failure) => {
            DeliveryOutcome::Failed(convert_delivery_failure(failure))
        }
        delivery::DeliveryOutcome::PartiallyCompleted(partial) => {
            DeliveryOutcome::PartiallyCompleted(PartialCompletion {
                committed_result: partial.committed_result,
                failed_outcome: match partial.failed_outcome {
                    delivery::FailedOutcome::Failed(failure) => {
                        FailedOutcome::Failed(convert_delivery_failure(failure))
                    }
                    delivery::FailedOutcome::Error(error) => {
                        FailedOutcome::Error(convert_delivery_error(error))
                    }
                    delivery::FailedOutcome::Cancelled => FailedOutcome::Cancelled,
                },
                effect_outcome: partial.effect_outcome.map(|outcome| match outcome {
                    delivery::EffectOutcome::RefusedBeforeDispatch => {
                        wamn_execution_contract::EffectOutcome::RefusedBeforeDispatch
                    }
                    delivery::EffectOutcome::Responded => {
                        wamn_execution_contract::EffectOutcome::Responded
                    }
                    delivery::EffectOutcome::Timeout => {
                        wamn_execution_contract::EffectOutcome::Timeout
                    }
                    delivery::EffectOutcome::Cancelled => {
                        wamn_execution_contract::EffectOutcome::Cancelled
                    }
                    delivery::EffectOutcome::EffectUncertain => {
                        wamn_execution_contract::EffectOutcome::EffectUncertain
                    }
                    delivery::EffectOutcome::ResponseLost => {
                        wamn_execution_contract::EffectOutcome::ResponseLost
                    }
                }),
            })
        }
        delivery::DeliveryOutcome::Cancelled => DeliveryOutcome::Cancelled,
        delivery::DeliveryOutcome::NotModified => DeliveryOutcome::NotModified,
    }
}

fn convert_delivery_failure(
    failure: bindings::wamn::router_delivery0_1_0::delivery::DeliveryFailure,
) -> DeliveryFailure {
    use bindings::wamn::router_delivery0_1_0::delivery;
    DeliveryFailure {
        kind: match failure.kind {
            delivery::FailureKind::Terminal => DeliveryFailureKind::Terminal,
            delivery::FailureKind::RetryExhausted => DeliveryFailureKind::RetryExhausted,
            delivery::FailureKind::InvalidInput => DeliveryFailureKind::InvalidInput,
            delivery::FailureKind::HopLimit => DeliveryFailureKind::HopLimit,
            delivery::FailureKind::UnreleasedCaller => DeliveryFailureKind::UnreleasedCaller,
            delivery::FailureKind::MissingDedupId => DeliveryFailureKind::MissingDedupId,
            delivery::FailureKind::RespondWithoutCaller => {
                DeliveryFailureKind::RespondWithoutCaller
            }
            delivery::FailureKind::SecondVerdict => DeliveryFailureKind::SecondVerdict,
        },
        code: failure.code,
        message: failure.message,
    }
}

fn convert_delivery_error(
    error: bindings::wamn::router_delivery0_1_0::delivery::DeliveryError,
) -> DeliveryError {
    use bindings::wamn::router_delivery0_1_0::delivery::DeliveryError as WireError;

    match error {
        WireError::SourceNotFound => DeliveryError::SourceNotFound,
        WireError::InvalidRequest => DeliveryError::InvalidRequest,
        WireError::InvalidPayload => DeliveryError::InvalidPayload,
        WireError::ExecutionFailed => DeliveryError::ExecutionFailed,
        WireError::PermissionDenied(denial) => DeliveryError::PermissionDenied {
            operation: denial.operation,
        },
        WireError::FreshCredentialRequired(denial) => DeliveryError::FreshCredentialRequired {
            operation: denial.operation,
        },
    }
}

fn route_definition(
    route: bindings::wamn::flow_http_routing::routing::RouteDefinition,
) -> Result<RouteDefinition, ProviderError> {
    use bindings::wamn::flow_http_routing::routing;

    let body_limit = usize::try_from(route.body_limit).map_err(|_| ProviderError)?;
    let mapped_limit = usize::try_from(route.mapped_limit).map_err(|_| ProviderError)?;
    Ok(RouteDefinition {
        attachment_id: route.attachment_id,
        host: route.host,
        path: route.path,
        method: route.method,
        mappings: route
            .mappings
            .into_iter()
            .map(|mapping| Mapping {
                from: match mapping.from {
                    routing::MappingSource::Body => MappingSource::Body,
                    routing::MappingSource::Path => MappingSource::Path,
                    routing::MappingSource::Query => MappingSource::Query,
                    routing::MappingSource::Header => MappingSource::Header,
                },
                name: mapping.name,
                to: mapping.to,
                optional: mapping.optional,
                cardinality: match mapping.cardinality {
                    routing::Cardinality::One => Cardinality::One,
                    routing::Cardinality::Many => Cardinality::Many,
                },
            })
            .collect(),
        body_limit,
        mapped_limit,
        cache_control: route.cache_control,
    })
}

struct WasiBody {
    request: Option<Request>,
    stream: Option<StreamReader<u8>>,
    trailers: Option<FutureReader<Result<Option<Fields>, ErrorCode>>>,
    result: Option<FutureWriter<Result<(), ErrorCode>>>,
}

impl BodyReader for WasiBody {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, BodyReadError> {
        // The adapter calls this only after route selection and authentication.
        if let Some(request) = self.request.take() {
            let (result, receiver) = bindings::wit_future::new(|| Ok(()));
            let (stream, trailers) = Request::consume_body(request, receiver);
            self.stream = Some(stream);
            self.trailers = Some(trailers);
            self.result = Some(result);
        }
        let Some(stream) = self.stream.as_mut() else {
            return Ok(None);
        };
        loop {
            let (status, bytes) = stream.read(Vec::with_capacity(8192)).await;
            match status {
                StreamResult::Complete(_) if !bytes.is_empty() => return Ok(Some(bytes)),
                StreamResult::Complete(_) => {}
                StreamResult::Cancelled => return Err(BodyReadError),
                StreamResult::Dropped => break,
            }
        }
        self.stream.take();
        // A closed stream alone does not establish successful body reception.
        let trailers = self
            .trailers
            .take()
            .expect("request body has trailers future");
        trailers.await.map_err(|_| BodyReadError)?;
        Ok(None)
    }
}

fn request_head(request: &Request) -> RequestHead {
    let method = match request.get_method() {
        Method::Get => "GET".to_string(),
        Method::Head => "HEAD".to_string(),
        Method::Post => "POST".to_string(),
        Method::Put => "PUT".to_string(),
        Method::Delete => "DELETE".to_string(),
        Method::Connect => "CONNECT".to_string(),
        Method::Options => "OPTIONS".to_string(),
        Method::Trace => "TRACE".to_string(),
        Method::Patch => "PATCH".to_string(),
        Method::Other(method) => method,
    };
    let headers: Vec<Header> = request
        .get_headers()
        .copy_all()
        .into_iter()
        .filter_map(|(name, value)| {
            String::from_utf8(value)
                .ok()
                .map(|value| Header { name, value })
        })
        .collect();
    // Origin-form HTTP requests carry their authority in the Host header.
    let authority = request.get_authority().unwrap_or_else(|| {
        headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("host"))
            .map(|header| header.value.clone())
            .unwrap_or_default()
    });
    RequestHead {
        method,
        authority,
        target: request
            .get_path_with_query()
            .unwrap_or_else(|| "/".to_string()),
        headers,
    }
}

fn send_response(response: HttpResponse, mut backend: GuestBackend) -> Response {
    let headers = Fields::new();
    if response.streamed
        && let Some(lines) = backend.lines.take()
    {
        return send_stream(&response, headers, lines, backend.permit.take());
    }
    for (name, value) in response.cache_headers() {
        let _ = headers.set(name, &[value.into_bytes()]);
    }
    if !response.actor_labels.is_empty() {
        let labels: std::collections::BTreeMap<_, _> = response.actor_labels.into_iter().collect();
        // Keep optional presentation metadata below common HTTP header limits.
        // A large label set falls back to the unchanged actor IDs.
        if let Ok(value) = serde_json::to_vec(&labels)
            && value.len() <= 4096
        {
            let _ = headers.set("wamn-actor-labels", &[value]);
        }
    }
    if !response.deadline_adjustments.is_empty() {
        let value = serde_json::to_vec(&response.deadline_adjustments)
            .expect("deadline adjustments contain serializable values");
        let _ = headers.set("wamn-deadline-adjustments", &[value]);
    }
    if !response.body.is_empty() {
        let _ = headers.set("content-type", &[response.content_type.as_bytes().to_vec()]);
    }
    let (mut writer, reader) = bindings::wit_stream::new::<u8>();
    let (trailers, trailer_reader) = bindings::wit_future::new(|| Ok(None));
    let (outgoing, _sent) = Response::new(headers, Some(reader), trailer_reader);
    let _ = outgoing.set_status_code(response.status);
    // The host consumes the stream after handle returns the response.
    spawn_local(async move {
        let _ = writer.write_all(response.body).await;
        drop(writer);
        let _ = trailers.write(Ok(None)).await;
    });
    outgoing
}

/// Lines of reply the body writer asks the host for at once.
const LINES_PER_READ: usize = 256;

/// Answer a streamed read: its head now, then its body as the host sends the
/// lines, one write per batch, so no line waits for the next. A client that
/// leaves drops the lines, which ends the host's read and its query.
fn send_stream(
    response: &HttpResponse,
    headers: Fields,
    mut lines: StreamReader<String>,
    permit: Option<bindings::wamn::flow_http_routing::routing::RoutePermit>,
) -> Response {
    for (name, value) in response.cache_headers() {
        let _ = headers.set(name, &[value.into_bytes()]);
    }
    let _ = headers.set("content-type", &[response.content_type.as_bytes().to_vec()]);
    // Proxies such as nginx hold a response body back unless told not to.
    let _ = headers.set("x-accel-buffering", &[b"no".to_vec()]);
    let (mut writer, reader) = bindings::wit_stream::new::<u8>();
    let (trailers, trailer_reader) = bindings::wit_future::new(|| Ok(None));
    let (outgoing, _sent) = Response::new(headers, Some(reader), trailer_reader);
    let _ = outgoing.set_status_code(response.status);
    spawn_local(async move {
        loop {
            let (status, batch) = lines.read(Vec::with_capacity(LINES_PER_READ)).await;
            if !batch.is_empty() {
                let mut bytes = Vec::new();
                for line in batch {
                    bytes.extend_from_slice(line.as_bytes());
                    bytes.push(b'\n');
                }
                if !writer.write_all(bytes).await.is_empty() {
                    break;
                }
            }
            if !matches!(status, StreamResult::Complete(_)) {
                break;
            }
        }
        drop(lines);
        drop(writer);
        drop(permit);
        let _ = trailers.write(Ok(None)).await;
    });
    outgoing
}
