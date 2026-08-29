//! WASI HTTP shell over authoritative routing, auth, and router delivery.

wit_bindgen::generate!({
    world: "flow-http",
    path: "wit",
    generate_all,
});

use exports::wasi::http::incoming_handler::Guest;
use wasi::http::types::{
    Fields, IncomingBody, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use wasi::io::streams::{InputStream, StreamError};

use super::{
    AdapterLimits, AuthRejection, Backend, BodyReadError, BodyReader, Cardinality, DeliveryError,
    DeliveryFailure, DeliveryFailureKind, DeliveryOutcome, DeliveryRequest, Emission, Header,
    HttpResponse, Mapping, MappingSource, ProviderError, RequestHead, RouteDefinition,
    handle_request,
};

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let head = request_head(&request);
        let body = request.consume().ok();
        let mut backend = GuestBackend;
        let mut body = WasiBody::new(body);
        let response = handle_request(&mut backend, &mut body, &head, AdapterLimits::default());
        send_response(response_out, response);
    }
}

export!(Component);

struct GuestBackend;

impl Backend for GuestBackend {
    type RoutePermit = wamn::flow_http_routing::routing::RoutePermit;

    fn routes(
        &mut self,
        method: &str,
        authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError> {
        let routes = wamn::flow_http_routing::routing::routes(method, authority)
            .map_err(|_| ProviderError)?;
        routes.into_iter().map(route_definition).collect()
    }

    fn authenticate(
        &mut self,
        policy: &str,
        headers: &[Header],
    ) -> Result<Option<String>, AuthRejection> {
        let headers = headers
            .iter()
            .map(|header| wamn::flow_http_routing::routing::Header {
                name: header.name.clone(),
                value: header.value.clone(),
            })
            .collect::<Vec<_>>();
        wamn::flow_http_routing::routing::authenticate(policy, &headers).map_err(|rejection| {
            AuthRejection {
                status: rejection.status,
                code: rejection.code,
            }
        })
    }

    fn try_acquire_route(
        &mut self,
        attachment_id: &str,
    ) -> Result<Option<Self::RoutePermit>, ProviderError> {
        wamn::flow_http_routing::routing::try_acquire(attachment_id).map_err(|_| ProviderError)
    }

    fn new_delivery_id(&mut self) -> String {
        const RANDOM_BYTES: u64 = 16;
        hex(&wasi::random::random::get_random_bytes(RANDOM_BYTES))
    }

    fn deliver(&mut self, request: DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError> {
        use wamn::router_delivery::delivery;

        let request = delivery::DeliveryRequest {
            source: delivery::Source::Attachment(request.attachment_id),
            delivery_id: request.delivery_id,
            payload: request.payload,
            caller: request.caller.map(|caller| delivery::CallerContext {
                role: caller.role,
                user_id: caller.user_id,
            }),
            trace: request.trace.map(|trace| delivery::TraceContext {
                traceparent: trace.traceparent,
                tracestate: trace.tracestate,
            }),
            parent_causation: None,
        };
        delivery::deliver(&request)
            .map(convert_delivery_outcome)
            .map_err(convert_delivery_error)
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
    outcome: wamn::router_delivery::delivery::DeliveryOutcome,
) -> DeliveryOutcome {
    use wamn::router_delivery::delivery;

    match outcome {
        delivery::DeliveryOutcome::Respond(payload) => DeliveryOutcome::Respond(payload),
        delivery::DeliveryOutcome::Emit(emission) => DeliveryOutcome::Emit(Emission {
            event: emission.event,
            dedup_id: emission.dedup_id,
        }),
        delivery::DeliveryOutcome::Discard => DeliveryOutcome::Discard,
        delivery::DeliveryOutcome::Failed(failure) => DeliveryOutcome::Failed(DeliveryFailure {
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
        }),
        delivery::DeliveryOutcome::Cancelled => DeliveryOutcome::Cancelled,
    }
}

fn convert_delivery_error(error: wamn::router_delivery::delivery::DeliveryError) -> DeliveryError {
    use wamn::router_delivery::delivery::DeliveryError as WireError;

    match error {
        WireError::SourceNotFound => DeliveryError::SourceNotFound,
        WireError::InvalidRequest => DeliveryError::InvalidRequest,
        WireError::InvalidPayload => DeliveryError::InvalidPayload,
        WireError::WiringNotPreloaded => DeliveryError::WiringNotPreloaded,
        WireError::ExecutionFailed => DeliveryError::ExecutionFailed,
    }
}

fn route_definition(
    route: wamn::flow_http_routing::routing::RouteDefinition,
) -> Result<RouteDefinition, ProviderError> {
    use wamn::flow_http_routing::routing;

    let input_schema = serde_json::from_str(&route.input_schema).map_err(|_| ProviderError)?;
    let body_limit = usize::try_from(route.body_limit).map_err(|_| ProviderError)?;
    let mapped_limit = usize::try_from(route.mapped_limit).map_err(|_| ProviderError)?;
    Ok(RouteDefinition {
        attachment_id: route.attachment_id,
        host: route.host,
        path: route.path,
        method: route.method,
        auth_policy: route.auth_policy,
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
        input_schema,
        body_limit,
        mapped_limit,
    })
}

struct WasiBody {
    stream: Option<InputStream>,
    _body: Option<IncomingBody>,
    failed: bool,
}

impl WasiBody {
    fn new(body: Option<IncomingBody>) -> Self {
        let stream = body.as_ref().and_then(|body| body.stream().ok());
        Self {
            stream,
            _body: body,
            failed: false,
        }
    }
}

impl BodyReader for WasiBody {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, BodyReadError> {
        if self.failed {
            return Err(BodyReadError);
        }
        let Some(stream) = &self.stream else {
            return Ok(None);
        };
        match stream.blocking_read(8192) {
            Ok(bytes) if bytes.is_empty() => Ok(None),
            Ok(bytes) => Ok(Some(bytes)),
            Err(StreamError::Closed) => Ok(None),
            Err(_) => {
                self.failed = true;
                Err(BodyReadError)
            }
        }
    }
}

fn request_head(request: &IncomingRequest) -> RequestHead {
    let method = match request.method() {
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
    let headers = request
        .headers()
        .entries()
        .into_iter()
        .filter_map(|(name, value)| {
            String::from_utf8(value)
                .ok()
                .map(|value| Header { name, value })
        })
        .collect();
    RequestHead {
        method,
        authority: request.authority().unwrap_or_default(),
        target: request.path_with_query().unwrap_or_else(|| "/".to_string()),
        headers,
    }
}

fn send_response(response_out: ResponseOutparam, response: HttpResponse) {
    let headers = Fields::new();
    if !response.body.is_empty() {
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    }
    let outgoing = OutgoingResponse::new(headers);
    let _ = outgoing.set_status_code(response.status);
    let body = outgoing.body().expect("flow-http response body");
    ResponseOutparam::set(response_out, Ok(outgoing));
    if let Ok(stream) = body.write() {
        for chunk in response.body.chunks(4096) {
            if stream.blocking_write_and_flush(chunk).is_err() {
                break;
            }
        }
    }
    let _ = OutgoingBody::finish(body, None);
}
