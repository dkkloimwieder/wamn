//! The edge serves the route of the `apps/edge_device` bundle through the
//! http-route guest, and logs one intent for each request element of a write.
//!
//! The support module builds the bundle when the tests start.

mod support;

use std::net::SocketAddr;

use serde_json::{Value, json};
use support::{HOST, PATH, ROLE, bundle, config, key, session, start};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// The capture time of every request element.
const CAPTURED_AT: &str = "2026-09-25T12:00:00.000000Z";

/// POST `body` to the route and return the status and the response body.
async fn post(addr: SocketAddr, token: Option<&str>, body: &str) -> (u16, String) {
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST {PATH} HTTP/1.1\r\nHost: {HOST}\r\n{authorization}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("send the request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read the response");
    let response = String::from_utf8(response).expect("the response is UTF-8");
    let (head, body) = response
        .split_once("\r\n\r\n")
        .expect("the response has a head");
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .expect("the response has a status");
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    (
        status,
        if chunked {
            dechunk(body)
        } else {
            body.to_owned()
        },
    )
}

fn dechunk(mut body: &str) -> String {
    let mut output = String::new();
    loop {
        let (size, rest) = body.split_once("\r\n").expect("a chunk size line");
        let size = usize::from_str_radix(size.trim(), 16).expect("a hexadecimal chunk size");
        if size == 0 {
            return output;
        }
        output.push_str(&rest[..size]);
        body = &rest[size + 2..];
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_edge_serves_a_route_to_a_session_whose_role_grants_it() {
    let (known, public) = key("key-one", 1);
    let (unknown, _) = key("key-other", 2);
    let (directory, digest) = bundle("route", &public);
    let host = start(config(&directory, digest)).await;
    let addr = host.addr();
    let body = json!([request_element("r-1", " 12.5 kg ")]).to_string();
    let body = body.as_str();

    let (status, answer) = post(addr, Some(&session(&known, "key-one", &[ROLE])), body).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        serde_json::from_str::<Value>(&answer).expect("a JSON body"),
        json!([outcome("r-1", "12.5 kg")]),
        "the device component trims the frame"
    );
    assert_eq!(post(addr, None, body).await.0, 401, "no credential");
    assert_eq!(
        post(addr, Some(&session(&unknown, "key-other", &[ROLE])), body)
            .await
            .0,
        401,
        "a key that the file does not hold"
    );
    assert_eq!(
        post(addr, Some(&session(&known, "key-one", &["viewer"])), body)
            .await
            .0,
        403,
        "a role without the permission"
    );
    host.stop().await.expect("the edge stops");
}

/// A request element of the route's input, keyed by its request id.
fn request_element(request_id: &str, frame: &str) -> Value {
    json!({"request_id": request_id, "value": {"frame": frame, "captured_at": CAPTURED_AT}})
}

/// The answer of the device component to one request element: the trimmed frame.
fn outcome(request_id: &str, frame: &str) -> Value {
    request_element(request_id, frame)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_replays_after_reopen_and_refuses_a_changed_request() {
    let (pair, public) = key("key-one", 1);
    let (directory, digest) = bundle("claims", &public);
    let config = config(&directory, digest);
    let host = start(config.clone()).await;
    let token = session(&pair, "key-one", &[ROLE]);
    let input = json!([request_element("durable", " 4 kg")]).to_string();
    let (status, answer) = post(host.addr(), Some(&token), &input).await;
    assert_eq!(status, 200);
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert!(answer[0].get("value").is_some());
    host.stop().await.unwrap();
    let reopened = start(config).await;
    let (status, replay) = post(reopened.addr(), Some(&token), &input).await;
    assert_eq!(status, 200);
    let replay: Value = serde_json::from_str(&replay).unwrap();
    assert_eq!(answer[0]["value"]["frame"], replay[0]["value"]["frame"]);
    let changed = json!([request_element("durable", "5 kg")]).to_string();
    let (status, conflict) = post(reopened.addr(), Some(&token), &changed).await;
    assert_eq!(status, 200);
    let conflict: Value = serde_json::from_str(&conflict).unwrap();
    assert!(conflict[0].get("error").is_some());
    reopened.stop().await.unwrap();
}
