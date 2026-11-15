//! Public control callback pages. The pending state supplies all authority.

use http_body_util::{BodyExt as _, Full};
use wamn_http_capability::oauth_authorization::Authorization;
use wasmtime_wasi_http::WasiBody;

/// Answer only the exact callback path and GET method, without a session.
pub async fn response(
    oauth: &Authorization,
    database_url: &str,
    org: &str,
    request: hyper::Request<WasiBody>,
) -> hyper::Response<WasiBody> {
    if request.uri().path() != "/oauth/callback" {
        return page(404, "OAuth callback not found");
    }
    if request.method() != hyper::Method::GET {
        return page(405, "OAuth callback requires GET");
    }
    let Ok(mut database) = wamn_postgres::connect(database_url).await else {
        return page(503, "callback database unavailable");
    };
    let result = oauth
        .callback(
            &mut database,
            org,
            request.uri().query().unwrap_or_default(),
        )
        .await;
    drop(database);
    match result {
        Ok(()) => page(200, "Authorization succeeded. You can close this page."),
        Err(error) => page(400, &error.reason),
    }
}

fn page(status: u16, message: &str) -> hyper::Response<WasiBody> {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>OAuth authorization</title></head><body><p>{escaped}</p></body></html>"
    );
    let body = Full::new(bytes::Bytes::from(html))
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut response = hyper::Response::new(body);
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
        let response = page(400, "provider error: <script>\"&'");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("&lt;script&gt;&quot;&amp;&#39;"));
        assert!(!html.contains("<script>"));
    }
}
