//! Build requests for the CLI’s control and application routes.

#[path = "read_query.rs"]
mod read_query;

pub(crate) use read_query::encode_read_query;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_routes::HostAttachment;

pub(crate) fn request(
    host_url: &str,
    token: &str,
    attachment: &HostAttachment,
    value: &Value,
) -> anyhow::Result<hyper::Request<http_body_util::Full<bytes::Bytes>>> {
    let mut url = format!(
        "{}{}",
        host_url.trim_end_matches('/'),
        attachment.definition["route"]["path"]
            .as_str()
            .context("route path")?
    );
    let (method, body) = if attachment.route.type_.is_read() {
        let query = encode_read_query(value.as_object().context("read request object")?);
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query);
        }
        (hyper::Method::GET, Vec::new())
    } else {
        let request_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
            .to_string();
        (
            hyper::Method::POST,
            serde_json::to_vec(&json!([{"request_id":request_id,"value":value}]))?,
        )
    };
    hyper::Request::builder()
        .method(method)
        .uri(url)
        .header(hyper::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(http_body_util::Full::new(bytes::Bytes::from(body)))
        .context("build control request")
}

/// Send CLI requests through the platform HTTP transport.
pub(crate) async fn send(
    request: hyper::Request<http_body_util::Full<bytes::Bytes>>,
) -> anyhow::Result<hyper::Response<bytes::Bytes>> {
    use http_body_util::BodyExt as _;
    let (parts, body) = request.into_parts();
    let body = body.collect().await?.to_bytes();
    wamn_http_client::HttpClient::shared()?
        .send(
            hyper::Request::from_parts(parts, body),
            std::time::Duration::from_secs(60),
            16 * 1024 * 1024,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_request_keeps_the_mounted_api_base() {
        let (_, attachment) = wamn_routes::HostRouteSet::App
            .attachments()
            .find(|(_, attachment)| attachment.route.operation == "role.create")
            .expect("the application declares role.create");
        let request = request(
            "https://dev.acme.example.test/shop/stock/api/",
            "token",
            attachment,
            &json!({"role":"clerk"}),
        )
        .expect("build a request for the mounted app");
        assert_eq!(request.method(), hyper::Method::POST);
        assert_eq!(
            request.uri().path(),
            "/shop/stock/api/wamn_control/role/create"
        );
    }
}
