//! NetSuite routes for the kind fixture. Authentication is enforced by the server.
use std::collections::HashMap;

use serde_json::{Value, json};

pub(super) struct Response {
    pub status: &'static str,
    pub body: Value,
    pub location: Option<String>,
}

impl Response {
    pub fn error(status: &'static str) -> Self {
        Self {
            status,
            body: json!({"error":status}),
            location: None,
        }
    }
}

pub(super) fn respond(
    method: &str,
    url: &url::Url,
    headers: &HashMap<String, &str>,
    body: &[u8],
    limit: usize,
    customers: &mut HashMap<String, Value>,
) -> Response {
    let input = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let ok = |body| Response {
        status: "200 OK",
        body,
        location: None,
    };
    match (method, url.path()) {
        ("POST", "/services/rest/query/v1/suiteql") => {
            let offset = query.get("offset").and_then(|v| v.parse::<usize>().ok());
            let requested = query.get("limit").and_then(|v| v.parse::<usize>().ok());
            if requested != Some(limit)
                || limit == 0
                || offset.is_none()
                || headers.get("prefer") != Some(&"transient")
                || !input["q"]
                    .as_str()
                    .is_some_and(|q| q.contains("ORDER BY id"))
            {
                return Response::error("400 Bad Request");
            }
            let offset = offset.unwrap();
            let all = json!([
                {"id":"1","name":"First"}, {"id":"2","name":"Second"},
                {"id":"3","name":"Third"}, {"id":"4","name":"Fourth"}
            ]);
            let items: Vec<_> = all
                .as_array()
                .unwrap()
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect();
            ok(
                json!({"items":items,"count":items.len(),"offset":offset,"hasMore":offset.saturating_add(limit)<4}),
            )
        }
        ("POST", "/app/site/hosting/restlet.nl") => {
            if !input.is_object()
                || !["script", "deploy"]
                    .iter()
                    .all(|k| query.get(*k).is_some_and(|v| !v.is_empty()))
            {
                return Response::error("400 Bad Request");
            }
            ok(input)
        }
        (method, path) if path.starts_with("/services/rest/record/v1/customer") => {
            let root = "/services/rest/record/v1/customer";
            let id = path.strip_prefix(root).unwrap();
            if method == "GET" {
                return customers
                    .get(id)
                    .cloned()
                    .map(ok)
                    .unwrap_or_else(|| Response::error("404 Not Found"));
            }
            if !input.is_object() {
                return Response::error("400 Bad Request");
            }
            let id = match method {
                "POST" if id.is_empty() => format!("/{}", customers.len() + 100),
                "PUT" if id.starts_with("/eid:") && id.len() > 5 => id.to_owned(),
                "PATCH" if customers.contains_key(id) => id.to_owned(),
                _ => return Response::error("404 Not Found"),
            };
            if method == "PATCH" {
                customers
                    .get_mut(&id)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .extend(input.as_object().unwrap().clone());
            } else {
                customers.insert(id.clone(), input);
            }
            Response {
                status: "204 No Content",
                body: Value::Null,
                location: Some(format!("{}{root}{id}", url.origin().ascii_serialization())),
            }
        }
        _ => Response::error("404 Not Found"),
    }
}
