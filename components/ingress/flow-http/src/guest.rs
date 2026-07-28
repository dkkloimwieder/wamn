//! WASI HTTP shell over authoritative routing/auth and invocation providers.

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
    AdapterLimits, AdapterOutcome, Backend, BodyReadError, BodyReader, Cardinality, ClientLiveness,
    Header, HttpResponse, Mapping, MappingSource, ProviderError, RequestHead, RouteDefinition,
    handle_request,
};
use wamn_flow_invocation::{
    Admitted, BeginResult, CancelAck, Failure, FlowError, InvokeRequest, InvokeResult, Rejection,
    Response,
};

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let head = request_head(&request);
        let body = request.consume().ok();
        let mut backend = GuestBackend;
        let mut body = WasiBody::new(body);
        let mut liveness = WasiLiveness;
        match handle_request(
            &mut backend,
            &mut body,
            &mut liveness,
            &head,
            AdapterLimits::default(),
        ) {
            AdapterOutcome::Response(response) => send_response(response_out, response),
            AdapterOutcome::Disconnected { .. } => {
                send_response(
                    response_out,
                    HttpResponse {
                        status: 499,
                        body: Vec::new(),
                    },
                );
            }
        }
    }
}

export!(Component);

struct GuestBackend;

impl Backend for GuestBackend {
    fn routes(
        &mut self,
        method: &str,
        authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError> {
        let routes = wamn::flow_http_routing::routing::routes(method, authority)
            .map_err(|_| ProviderError)?;
        routes.into_iter().map(route_definition).collect()
    }

    fn authenticate(&mut self, policy: &str, headers: &[Header]) -> Result<String, Rejection> {
        let headers = headers
            .iter()
            .map(|header| wamn::flow_http_routing::routing::Header {
                name: header.name.clone(),
                value: header.value.clone(),
            })
            .collect::<Vec<_>>();
        wamn::flow_http_routing::routing::authenticate(policy, &headers).map_err(|rejection| {
            Rejection {
                status: rejection.status,
                code: rejection.code,
            }
        })
    }

    fn begin(&mut self, request: InvokeRequest) -> Result<BeginResult, ProviderError> {
        use wamn::flow_invocation::invocation;

        let request = invocation::InvokeRequest {
            attachment_id: request.attachment_id,
            expected_catalog_version: request.expected_catalog_version,
            expected_definition_hash: request.expected_definition_hash,
            client_request_fingerprint: request.client_request_fingerprint,
            payload: request.payload,
            idempotency_key: request.idempotency_key,
            principal: request.principal,
            deadline_override: request.deadline_override,
            trace: request.trace.map(|trace| invocation::TraceContext {
                traceparent: trace.traceparent,
                tracestate: trace.tracestate,
            }),
        };
        Ok(match invocation::begin(&request) {
            invocation::BeginResult::Admitted(admitted) => BeginResult::Admitted(Admitted {
                run_id: admitted.run_id,
            }),
            invocation::BeginResult::Rejected(rejection) => BeginResult::Rejected(Rejection {
                status: rejection.status,
                code: rejection.code,
            }),
        })
    }

    fn wait(
        &mut self,
        run_id: &str,
        timeout_ms: u32,
    ) -> Result<Option<InvokeResult>, ProviderError> {
        use wamn::flow_invocation::invocation;

        Ok(
            invocation::wait(run_id, timeout_ms).map(|result| match result {
                invocation::InvokeResult::Responded(response) => {
                    InvokeResult::Responded(Response {
                        run_id: response.run_id,
                        body: response.body,
                        status_hint: response.status_hint,
                    })
                }
                invocation::InvokeResult::Failed(failure) => {
                    InvokeResult::Failed(convert_failure(failure))
                }
                invocation::InvokeResult::Cancelled(failure) => {
                    InvokeResult::Cancelled(convert_failure(failure))
                }
            }),
        )
    }

    fn cancel(&mut self, run_id: &str) -> Result<CancelAck, ProviderError> {
        let ack = wamn::flow_invocation::invocation::cancel(run_id);
        Ok(CancelAck { run_id: ack.run_id })
    }
}

fn convert_failure(failure: wamn::flow_invocation::invocation::Failure) -> Failure {
    Failure {
        status: failure.status,
        error: FlowError {
            code: failure.error.code,
            message: failure.error.message,
            run_id: failure.error.run_id,
            flow_id: failure.error.flow_id,
            flow_version: failure.error.flow_version,
        },
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
        catalog_version: route.catalog_version,
        definition_hash: route.definition_hash,
        host: route.host,
        path: route.path,
        method: route.method,
        enabled: route.enabled,
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
        idempotency_required: route.idempotency_required,
        body_limit,
        mapped_limit,
        deadline_override: route.deadline_override,
    })
}

struct WasiBody {
    _body: Option<IncomingBody>,
    stream: Option<InputStream>,
    failed: bool,
}

impl WasiBody {
    fn new(body: Option<IncomingBody>) -> Self {
        let stream = body.as_ref().and_then(|body| body.stream().ok());
        Self {
            _body: body,
            stream,
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

struct WasiLiveness;

impl ClientLiveness for WasiLiveness {
    fn connected(&mut self) -> bool {
        wamn::flow_http_routing::routing::caller_connected()
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
