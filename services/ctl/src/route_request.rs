//! Build requests for the CLI’s control routes.

#[path = "read_query.rs"]
mod read_query;

pub(crate) use read_query::encode_read_query;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_catalog::HostAttachment;

pub(crate) fn request(
    client: &reqwest::Client,
    host_url: &str,
    token: &str,
    attachment: &HostAttachment,
    value: &Value,
) -> anyhow::Result<reqwest::Request> {
    let mut url = format!(
        "{}{}",
        host_url.trim_end_matches('/'),
        attachment.definition["route"]["path"]
            .as_str()
            .context("route path")?
    );
    let request = if attachment.route.type_.is_read() {
        let query = encode_read_query(value.as_object().context("read request object")?);
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query);
        }
        client.get(url)
    } else {
        let request_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
            .to_string();
        client
            .post(url)
            .json(&json!([{"request_id":request_id,"value":value}]))
    };
    request
        .bearer_auth(token)
        .build()
        .context("build control request")
}
