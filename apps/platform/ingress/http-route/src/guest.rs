//! WASI HTTP shell over release route data and typed host invocation.

use crate::bindings;

use bindings::exports::wasi::http::handler::Guest;
use bindings::wasi::http::types::{ErrorCode, Fields, Method, Request, Response};
use wit_bindgen::rt::async_support::{
    FutureReader, FutureWriter, StreamReader, StreamResult, spawn_local,
};

use super::{
    AdapterLimits, AuthRejection, Backend, BodyReadError, BodyReader, Header, HttpResponse,
    OperationRequest, ProviderError, RequestHead, RouteDefinition, SchemaInvalid, handle_request,
};

#[path = "entry.rs"]
mod entry;

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

bindings::export!(Component with_types_in bindings);

#[derive(Default)]
struct GuestBackend;
impl Backend for GuestBackend {
    type AuthenticatedCaller = bindings::wamn::flow_http_routing::routing::AuthenticatedCaller;
    fn routes(
        &mut self,
        method: &str,
        _authority: &str,
    ) -> Result<Vec<RouteDefinition>, ProviderError> {
        let data = bindings::wasi::config::store::get("wamn.http.routes")
            .map_err(|_| ProviderError)?
            .ok_or(ProviderError)?;
        let routes: Vec<RouteDefinition> =
            serde_json::from_str(&data).map_err(|_| ProviderError)?;
        Ok(routes.into_iter().filter(|r| r.method == method).collect())
    }
    async fn authenticate(
        &mut self,
        route: &RouteDefinition,
        headers: &[Header],
        body: &[u8],
    ) -> Result<Option<Self::AuthenticatedCaller>, AuthRejection> {
        let policy = route.auth_policy.to_string();
        bindings::wamn::flow_http_routing::routing::authenticate(
            route.attachment_id.clone(),
            policy,
            headers.to_vec(),
            body.to_vec(),
        )
        .await
    }
    fn validate_input(
        &mut self,
        route: &RouteDefinition,
        payload: &str,
    ) -> Result<(), SchemaInvalid> {
        entry::validate_read_limits(route, payload)
    }
    async fn invoke(
        &mut self,
        route: &RouteDefinition,
        request: OperationRequest<Self::AuthenticatedCaller>,
    ) -> HttpResponse {
        entry::invoke(route, request).await
    }
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

fn send_response(response: HttpResponse) -> Response {
    let headers = Fields::new();
    for (name, value) in response.cache_headers() {
        let _ = headers.set(name, &[value.into_bytes()]);
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
