//! Body handling for native control HTTP endpoints.

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full, combinators::UnsyncBoxBody};
use hyper::{Request, Response, StatusCode, body::Incoming, header};
use serde_json::Value;

pub type HttpBody = UnsyncBoxBody<Bytes, hyper::Error>;

pub fn incoming(request: Request<Incoming>) -> Request<HttpBody> {
    request.map(BodyExt::boxed_unsync)
}

#[derive(Debug)]
pub enum ReadError {
    Invalid(hyper::Error),
    TooLarge,
}

pub async fn read_limited(mut body: HttpBody, maximum: u64) -> Result<Vec<u8>, ReadError> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(ReadError::Invalid)?;
        if let Ok(data) = frame.into_data() {
            if bytes.len().saturating_add(data.len()) as u64 > maximum {
                return Err(ReadError::TooLarge);
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes)
}

fn full(bytes: Bytes) -> HttpBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}

pub fn empty_response(status: StatusCode) -> Response<HttpBody> {
    let body = Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response
}

pub fn json_response(status: u16, body: &Value) -> Response<HttpBody> {
    let mut response = Response::new(full(body.to_string().into()));
    *response.status_mut() = StatusCode::from_u16(status).expect("native HTTP status");
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

pub fn oauth_callback_page(status: u16, message: &str) -> hyper::Response<HttpBody> {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>OAuth authorization</title></head><body><p>{escaped}</p></body></html>"
    );
    let mut response = hyper::Response::new(full(html.into()));
    *response.status_mut() = hyper::StatusCode::from_u16(status).expect("fixed callback status");
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        "cache-control",
        hyper::header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "content-security-policy",
        hyper::header::HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    response.headers_mut().insert(
        "referrer-policy",
        hyper::header::HeaderValue::from_static("no-referrer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn callback_pages_escape_provider_values() {
        let response = oauth_callback_page(400, "provider error: <script>\"&'");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("&lt;script&gt;&quot;&amp;&#39;"));
        assert!(!html.contains("<script>"));
    }
}
