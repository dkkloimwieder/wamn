//! HTTP responses for session exchanges.

use crate::response;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};

pub(super) fn unauthorized() -> Response<Full<Bytes>> {
    let mut response = response(
        StatusCode::UNAUTHORIZED,
        "application/json",
        b"{\"error\":\"unauthorized\"}".to_vec(),
    );
    response.headers_mut().insert(
        hyper::header::WWW_AUTHENTICATE,
        hyper::header::HeaderValue::from_static("Bearer"),
    );
    response
}
