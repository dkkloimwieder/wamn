//! WASI HTTP shell over authoritative routing, auth, and router delivery.

wit_bindgen::generate!({
    world: "flow-http",
    path: "wit",
    generate_all,
    async: ["export:wasi:http/handler@0.3.0#handle"],
});

use exports::wasi::http::handler::Guest;
use wasi::http::types::{ErrorCode, Fields, Method, Request, Response};
use wit_bindgen::rt::async_support::{
    FutureReader, FutureWriter, StreamReader, StreamResult, spawn_local,
};

use super::{
    AdapterLimits, AuthRejection, Backend, BodyReadError, BodyReader, Cardinality, DeliveryError,
    DeliveryFailure, DeliveryFailureKind, DeliveryOutcome, DeliveryRequest, Emission,
    FailedOutcome, Header, HttpResponse, Mapping, MappingSource, PartialCompletion, ProviderError,
    RequestHead, RouteDefinition, handle_request,
};

struct Component;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let head = request_head(&request);
        let mut backend = GuestBackend;
        let mut body = WasiBody {
            request: Some(request),
            stream: None,
            trailers: None,
            result: None,
        };
        let response =
            handle_request(&mut backend, &mut body, &head, AdapterLimits::default()).await;
        drop(body);
        Ok(send_response(response))
    }
}

export!(Component);

struct GuestBackend;

impl Backend for GuestBackend {
    type RoutePermit = wamn::flow_http_routing::routing::RoutePermit;
    type AuthenticatedCaller = wamn::flow_http_routing::routing::AuthenticatedCaller;

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
        attachment_id: &str,
        headers: &[Header],
    ) -> Result<Option<Self::AuthenticatedCaller>, AuthRejection> {
        let headers = headers
            .iter()
            .map(|header| wamn::flow_http_routing::routing::Header {
                name: header.name.clone(),
                value: header.value.clone(),
            })
            .collect::<Vec<_>>();
        wamn::flow_http_routing::routing::authenticate(attachment_id, &headers).map_err(
            |rejection| AuthRejection {
                status: rejection.status,
                code: rejection.code,
            },
        )
    }

    fn validate_input(&mut self, attachment_id: &str, payload: &str) -> Result<(), ProviderError> {
        wamn::flow_http_routing::routing::validate_input(attachment_id, payload)
            .map_err(|_| ProviderError)
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

    fn deliver(
        &mut self,
        request: DeliveryRequest<Self::AuthenticatedCaller>,
    ) -> Result<DeliveryOutcome, DeliveryError> {
        use wamn::router_delivery::delivery;

        let request = delivery::DeliveryRequest {
            source: delivery::Source::Attachment(request.attachment_id),
            delivery_id: request.delivery_id,
            payload: request.payload,
            caller: request.caller,
            trace: request.trace.map(|trace| delivery::TraceContext {
                traceparent: trace.traceparent,
                tracestate: trace.tracestate,
            }),
            parent_causation: None,
        };
        delivery::deliver(request)
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
    }
}

fn convert_delivery_failure(
    failure: wamn::router_delivery::delivery::DeliveryFailure,
) -> DeliveryFailure {
    use wamn::router_delivery::delivery;
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

fn convert_delivery_error(error: wamn::router_delivery::delivery::DeliveryError) -> DeliveryError {
    use wamn::router_delivery::delivery::DeliveryError as WireError;

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
    route: wamn::flow_http_routing::routing::RouteDefinition,
) -> Result<RouteDefinition, ProviderError> {
    use wamn::flow_http_routing::routing;

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
            let (result, receiver) = wit_future::new(|| Ok(()));
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

fn send_response(response: HttpResponse) -> Response {
    let headers = Fields::new();
    if !response.body.is_empty() {
        let _ = headers.set("content-type", &[response.content_type.as_bytes().to_vec()]);
    }
    let (mut writer, reader) = wit_stream::new::<u8>();
    let (trailers, trailer_reader) = wit_future::new(|| Ok(None));
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
