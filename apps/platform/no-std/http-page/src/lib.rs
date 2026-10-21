//! Paginated HTTP GET palette component.

#![no_std]

#[cfg(any(test, feature = "std"))]
extern crate std;

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;

#[cfg(not(any(test, feature = "std")))]
#[path = "../../guest_runtime.rs"]
mod guest_runtime;

use bindings::exports::wamn::node::async_handler::{Emission, Guest, NodeContext, NodeError};
use bindings::wamn::connection::http::{ConnectionError, Header, Request, Response};
use bindings::wamn::node::types::{ErrorDetail, RateLimitDetail};

#[expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.61 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:http-page/http-page@0.1.0",
        path: [
            "../../../../crates/platform/runtime/wit/deps/wamn-connection",
            "../../../../crates/execution/workflow/router/wit",
            "wit",
        ],
        generate_all,
        async: [
            "import:wamn:connection/http@0.1.0#send",
            "export:wamn:node/async-handler@0.1.0#run",
        ],
        std_feature,
    });
}

struct Component;

impl Guest for Component {
    async fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        serde_json::from_str::<serde_json::Value>(&input)
            .map_err(|error| invalid_input(format!("input is not JSON: {error}")))?;
        let config = serde_json::from_str::<serde_json::Value>(&context.config)
            .map_err(|error| terminal("invalid-config", format!("config is not JSON: {error}")))?;
        let mut headers = configured_headers(&config)?;
        if let Some(traceparent) = context.traceparent {
            push_header_unless_present(&mut headers, "traceparent", traceparent.into_bytes());
        }
        if let Some(tracestate) = context.tracestate {
            push_header_unless_present(&mut headers, "tracestate", tracestate.into_bytes());
        }
        let items = collect_pages(&config, headers, async |request| {
            bindings::wamn::connection::http::send(request)
                .await
                .map_err(classify_connection_error)
        })
        .await?;
        Ok(Emission {
            payload: serde_json::Value::Array(items).to_string(),
            port: None,
        })
    }
}

fn required_string(config: &serde_json::Value, field: &str) -> Result<String, NodeError> {
    config
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(alloc::borrow::ToOwned::to_owned)
        .ok_or_else(|| terminal("invalid-config", format!("{field} must be a string")))
}

fn configured_headers(config: &serde_json::Value) -> Result<Vec<Header>, NodeError> {
    let Some(headers) = config.get("headers") else {
        return Ok(Vec::new());
    };
    let headers = headers
        .as_object()
        .ok_or_else(|| terminal("invalid-config", "headers must be an object of strings"))?;
    headers
        .iter()
        .map(|(name, value)| {
            let value = value.as_str().ok_or_else(|| {
                terminal(
                    "invalid-config",
                    format!("header {name:?} must be a string"),
                )
            })?;
            Ok(Header {
                name: name.to_ascii_lowercase(),
                value: value.as_bytes().to_vec(),
            })
        })
        .collect()
}

fn push_header_unless_present(headers: &mut Vec<Header>, name: &str, value: Vec<u8>) {
    if !headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case(name))
    {
        headers.push(Header {
            name: name.to_string(),
            value,
        });
    }
}

fn response_error(response: &Response) -> ErrorDetail {
    ErrorDetail {
        message: format!("upstream answered HTTP {}", response.status),
        code: Some(format!("HTTP_{}", response.status)),
    }
}

fn retry_after_ms(headers: &[Header]) -> Option<u64> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("retry-after"))
        .and_then(|header| core::str::from_utf8(&header.value).ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1_000))
}

fn classify_connection_error(error: ConnectionError) -> NodeError {
    match error {
        ConnectionError::CredentialUnavailable | ConnectionError::Timeout => retryable(
            "connection-unavailable",
            format!("connection failed: {error:?}"),
        ),
        ConnectionError::Transport(detail) => retryable("http-transport", detail),
        ConnectionError::Unbound
        | ConnectionError::Incompatible
        | ConnectionError::AuthorityDenied
        | ConnectionError::AttestationInvalid => terminal(
            "connection-denied",
            format!("connection refused: {error:?}"),
        ),
    }
}

fn invalid_input(message: String) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message,
        code: Some("invalid-json".to_string()),
    })
}

fn retryable(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::Retryable(ErrorDetail {
        message: message.into(),
        code: Some(code.to_string()),
    })
}

fn terminal(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: message.into(),
        code: Some(code.to_string()),
    })
}

bindings::export!(Component with_types_in bindings);

/// Collect pages without retaining any transport or run state between calls.
async fn collect_pages<F, Fut>(
    config: &serde_json::Value,
    headers: Vec<Header>,
    mut send: F,
) -> Result<Vec<serde_json::Value>, NodeError>
where
    F: FnMut(Request) -> Fut,
    Fut: core::future::Future<Output = Result<Response, NodeError>>,
{
    let requirement = required_string(config, "requirement")?;
    let mut path = required_string(config, "path-and-query")?;
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(terminal(
            "invalid-config",
            "path-and-query must be a relative absolute-path",
        ));
    }
    let items_pointer = required_string(config, "items")?;
    let page = &config["page"];
    let style = required_string(page, "style")?;
    let mut offset = 0_u64;
    let mut limit = 0_usize;
    let mut offset_param = String::new();
    match style.as_str() {
        "offset" => {
            let limit_param = required_string(page, "limit")?;
            offset_param = required_string(page, "offset")?;
            limit = query_value(&path, &limit_param)
                .and_then(|value| value.parse().ok())
                .filter(|limit| *limit > 0)
                .ok_or_else(|| {
                    terminal(
                        "invalid-config",
                        "path-and-query must carry a positive limit",
                    )
                })?;
            if let Some(value) = query_value(&path, &offset_param) {
                offset = value.parse().map_err(|_| {
                    terminal("invalid-config", "offset must be an unsigned integer")
                })?;
            }
        }
        "cursor" => {
            required_string(page, "pointer")?;
            required_string(page, "param")?;
        }
        "link-header" => {}
        _ => return Err(terminal("invalid-config", "unknown page style")),
    }
    let mut collected = Vec::new();
    loop {
        let response = send(Request {
            requirement: requirement.clone(),
            method: "GET".into(),
            path_and_query: path.clone(),
            headers: headers.clone(),
            body: None,
            idempotency_key: None,
        })
        .await?;
        match response.status {
            429 => {
                return Err(NodeError::RateLimited(RateLimitDetail {
                    detail: response_error(&response),
                    retry_after_ms: retry_after_ms(&response.headers),
                }));
            }
            500..=599 => return Err(NodeError::Retryable(response_error(&response))),
            400..=499 => return Err(NodeError::Terminal(response_error(&response))),
            _ => {}
        }
        let body: serde_json::Value = serde_json::from_slice(&response.body)
            .map_err(|error| terminal("invalid-page", format!("page is not JSON: {error}")))?;
        let items = body
            .pointer(&items_pointer)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| terminal("invalid-page", "items pointer must select an array"))?;
        let count = items.len();
        collected.extend(items.iter().cloned());
        match style.as_str() {
            "offset" => {
                if count < limit {
                    break;
                }
                offset = offset
                    .checked_add(limit as u64)
                    .ok_or_else(|| terminal("invalid-page", "offset overflow"))?;
                path = set_query(&path, &offset_param, &offset.to_string());
            }
            "cursor" => {
                let pointer = required_string(page, "pointer")?;
                let Some(cursor) = body.pointer(&pointer).filter(|value| !value.is_null()) else {
                    break;
                };
                let cursor = cursor
                    .as_str()
                    .ok_or_else(|| terminal("invalid-page", "next cursor must be a string"))?;
                if cursor.is_empty() {
                    break;
                }
                path = set_query(&path, &required_string(page, "param")?, cursor);
            }
            "link-header" => {
                let Some(next) = next_link(&response.headers)? else {
                    break;
                };
                path = next;
            }
            _ => unreachable!(),
        }
    }
    Ok(collected)
}

fn query_value<'a>(path: &'a str, name: &str) -> Option<&'a str> {
    path.split_once('?')?
        .1
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

fn encode_query(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use core::fmt::Write as _;
            write!(encoded, "%{byte:02X}").expect("write to a string");
        }
    }
    encoded
}

fn set_query(path: &str, name: &str, value: &str) -> String {
    let (base, query) = path.split_once('?').unwrap_or((path, ""));
    let name = encode_query(name);
    let mut parts: Vec<_> = query
        .split('&')
        .filter(|part| !part.is_empty())
        .filter(|part| part.split('=').next() != Some(name.as_str()))
        .map(String::from)
        .collect();
    parts.push(format!("{name}={}", encode_query(value)));
    format!("{base}?{}", parts.join("&"))
}

fn next_link(headers: &[Header]) -> Result<Option<String>, NodeError> {
    for header in headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("link"))
    {
        let value = core::str::from_utf8(&header.value)
            .map_err(|_| terminal("invalid-page", "Link header is not UTF-8"))?;
        for link in value.split(',') {
            let mut parts = link.trim().split(';');
            let target = parts.next().unwrap_or_default().trim();
            let is_next =
                parts
                    .filter_map(|part| part.trim().split_once('='))
                    .any(|(key, value)| {
                        key == "rel"
                            && value
                                .trim_matches('"')
                                .split_whitespace()
                                .any(|rel| rel == "next")
                    });
            if !is_next {
                continue;
            }
            let target = target
                .strip_prefix('<')
                .and_then(|target| target.strip_suffix('>'))
                .ok_or_else(|| {
                    terminal(
                        "invalid-page",
                        "next link must be enclosed in angle brackets",
                    )
                })?;
            if target.contains("://") || target.starts_with("//") {
                return absolute_next(target).map(Some);
            }
            return Ok(Some(target.to_string()));
        }
    }
    Ok(None)
}

// The guest cannot see the binding's origin or base path. The owner must decide
// how an absolute next URL proves that boundary without changing the contract.
fn absolute_next(_url: &str) -> Result<String, NodeError> {
    Err(terminal(
        "invalid-page",
        "absolute next URLs await the binding policy",
    ))
}

#[cfg(test)]
mod tests;
