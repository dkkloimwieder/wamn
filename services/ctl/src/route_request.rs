//! Build requests for the CLI’s control routes.

#[path = "read_query.rs"]
mod read_query;

pub(crate) use read_query::encode_read_query;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_catalog::HostAttachment;

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

/// The shared HTTP transport for every CLI route.
pub(crate) fn client() -> hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Full<bytes::Bytes>,
> {
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .build();
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(connector)
}
