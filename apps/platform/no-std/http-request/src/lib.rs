//! Ordinary HTTP request palette component.

#![no_std]

extern crate alloc;

use alloc::borrow::ToOwned as _;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;

#[path = "../../guest_runtime.rs"]
mod guest_runtime;

use exports::wamn::node::handler::{Emission, Guest, NodeContext, NodeError};
use wamn::connection::http::{ConnectionError, Header, Request, Response};
use wamn::node::types::{ErrorDetail, RateLimitDetail};

wit_bindgen::generate!({
    world: "http-request",
    path: "wit",
    generate_all,
    std_feature,
});

struct Component;

impl Guest for Component {
    fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        serde_json::from_str::<serde_json::Value>(&input)
            .map_err(|error| invalid_input(format!("input is not JSON: {error}")))?;
        let config = serde_json::from_str::<serde_json::Value>(&context.config)
            .map_err(|error| terminal("invalid-config", format!("config is not JSON: {error}")))?;
        let requirement = required_string(&config, "requirement")?;
        let method = required_string(&config, "method")?.to_ascii_uppercase();
        let path_and_query = required_string(&config, "path-and-query")?;
        if !path_and_query.starts_with('/') || path_and_query.starts_with("//") {
            return Err(terminal(
                "invalid-config",
                "path-and-query must be a relative absolute-path",
            ));
        }

        let mut headers = configured_headers(&config)?;
        if let Some(traceparent) = context.traceparent {
            push_header_unless_present(&mut headers, "traceparent", traceparent.into_bytes());
        }
        if let Some(tracestate) = context.tracestate {
            push_header_unless_present(&mut headers, "tracestate", tracestate.into_bytes());
        }

        let request = Request {
            requirement,
            method,
            path_and_query,
            headers,
            body: Some(input.into_bytes()),
            idempotency_key: Some(format!(
                "{}:{}:{}",
                context.delivery_id, context.node_id, context.occurrence
            )),
        };
        match wamn::connection::http::send(&request) {
            Ok(response) => classify_response(response),
            Err(error) => Err(classify_connection_error(error)),
        }
    }
}

fn required_string(config: &serde_json::Value, field: &str) -> Result<String, NodeError> {
    config
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(|value| value.to_owned())
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

fn classify_response(response: Response) -> Result<Emission, NodeError> {
    if response.status == 429 {
        return Err(NodeError::RateLimited(RateLimitDetail {
            detail: response_error(&response),
            retry_after_ms: retry_after_ms(&response.headers),
        }));
    }
    match response.status {
        408 | 500..=599 => Err(NodeError::Retryable(response_error(&response))),
        400..=499 => Err(NodeError::Terminal(response_error(&response))),
        _ => {
            let headers: BTreeMap<_, _> = response
                .headers
                .into_iter()
                .map(|header| {
                    (
                        header.name.to_ascii_lowercase(),
                        String::from_utf8_lossy(&header.value).into_owned(),
                    )
                })
                .collect();
            let body =
                serde_json::from_slice::<serde_json::Value>(&response.body).unwrap_or_else(|_| {
                    serde_json::Value::String(String::from_utf8_lossy(&response.body).into_owned())
                });
            let payload = serde_json::json!({
                "status": response.status,
                "headers": headers,
                "body": body,
            });
            Ok(Emission {
                payload: payload.to_string(),
                port: None,
            })
        }
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

export!(Component);
