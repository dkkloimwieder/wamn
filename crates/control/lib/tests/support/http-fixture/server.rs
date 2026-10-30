//! HTTP fixture for declared authentication and business request boundaries.
use base64::Engine as _;
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
mod netsuite;
#[derive(Default)]
struct Counts {
    pages: HashMap<usize, usize>,
    recorded: Vec<serde_json::Value>,
    customers: HashMap<String, serde_json::Value>,
}
struct Fixture {
    limit: usize,
}
fn basic_header() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("fixture:fixture-password")
    )
}
fn verify_basic(headers: &HashMap<String, &str>) -> bool {
    headers.get("authorization").copied() == Some(basic_header().as_str())
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}
fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
        .replace('*', "%2A")
        .replace("%7E", "~")
}
fn verify_aws(method: &str, url: &url::Url, headers: &HashMap<String, &str>, body: &[u8]) -> bool {
    let Some(auth) = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("AWS4-HMAC-SHA256 "))
    else {
        return false;
    };
    let fields: HashMap<_, _> = auth
        .split(", ")
        .filter_map(|pair| pair.split_once('='))
        .collect();
    let (Some(credential), Some(signed), Some(signature), Some(date)) = (
        fields.get("Credential"),
        fields.get("SignedHeaders"),
        fields.get("Signature"),
        headers.get("x-amz-date"),
    ) else {
        return false;
    };
    let scope: Vec<_> = credential.split('/').collect();
    if date.len() < 8
        || scope.len() != 5
        || scope[0] != "fixture-access"
        || scope[2] != "us-east-1"
        || scope[3] != "execute-api"
        || scope[4] != "aws4_request"
        || !date.starts_with(scope[1])
    {
        return false;
    }
    if headers.get("x-amz-content-sha256").copied() != Some(digest(body).as_str()) {
        return false;
    }
    let mut canonical = String::new();
    for name in signed.split(';') {
        let Some(value) = headers.get(name) else {
            return false;
        };
        canonical.push_str(&format!(
            "{name}:{}\n",
            value.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    let mut query = url
        .query_pairs()
        .map(|(key, value)| (encode(&key), encode(&value)))
        .collect::<Vec<_>>();
    query.sort();
    let query = query
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let canonical = format!(
        "{method}\n{}\n{query}\n{canonical}\n{signed}\n{}",
        url.path(),
        digest(body)
    );
    let scope = scope[1..].join("/");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
        digest(canonical.as_bytes())
    );
    let key = mac(b"AWS4fixture-private", &date.as_bytes()[..8]);
    let key = mac(&key, b"us-east-1");
    let key = mac(&key, b"execute-api");
    let key = mac(&key, b"aws4_request");
    hex::decode(signature)
        .ok()
        .is_some_and(|value| value == mac(&key, to_sign.as_bytes()))
}
fn serve_socket(
    mut socket: TcpStream,
    fixture: &Fixture,
    counts: &Mutex<Counts>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if socket.read(&mut byte)? == 0 {
                assert!(head.is_empty(), "HTTP peer truncated its request head");
                return Ok(());
            }
            head.push(byte[0]);
            assert!(head.len() <= 16_384, "fixture request head exceeds 16 KiB");
        }
        let head = std::str::from_utf8(&head)?;
        let headers: HashMap<_, _> = head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.to_ascii_lowercase(), value.trim()))
            .collect();
        let length: usize = headers
            .get("content-length")
            .copied()
            .unwrap_or("0")
            .parse()?;
        assert!(length <= 65_536, "fixture body exceeds 64 KiB");
        let mut body = vec![0; length];
        socket.read_exact(&mut body)?;
        let mut request = head
            .lines()
            .next()
            .ok_or("request line")?
            .split_whitespace();
        let method = request.next().ok_or("method")?;
        let target = request.next().ok_or("target")?;
        let url = url::Url::parse(&format!(
            "http://{}{}",
            headers.get("host").ok_or("host")?,
            target
        ))?;
        let basic_verified = verify_basic(&headers);
        let key_verified = headers.get("x-wamn-key").copied() == Some("fixture-outbound-key")
            || url
                .query_pairs()
                .any(|(key, value)| key == "key" && value == "fixture-api-key");
        let aws_verified = verify_aws(method, &url, &headers, &body);
        let mut counts = counts.lock().unwrap();
        let mut status = "200 OK";
        let mut extra = String::new();
        let mut response_body = body.clone();
        let mut page = None;
        let mut page_request_count = None;
        let mut items = serde_json::Value::Null;
        let netsuite = url.path().starts_with("/services/rest/")
            || url.path() == "/app/site/hosting/restlet.nl";
        let mut netsuite_response = serde_json::Value::Null;
        let mut location = None;
        match url.path() {
            _ if netsuite => {
                let response = if basic_verified {
                    netsuite::respond(
                        method,
                        &url,
                        &headers,
                        &body,
                        fixture.limit,
                        &mut counts.customers,
                    )
                } else {
                    netsuite::Response::error("401 Unauthorized")
                };
                status = response.status;
                location = response.location;
                if let Some(value) = &location {
                    extra = format!("location: {value}\r\n");
                }
                netsuite_response = response.body.clone();
                response_body = if status.starts_with("204") {
                    Vec::new()
                } else {
                    serde_json::to_vec(&response.body)?
                };
            }
            "/recorded" if method == "GET" => {
                response_body = serde_json::to_vec(&counts.recorded)?;
            }
            "/api-key" if !key_verified => {
                status = "401 Unauthorized";
            }
            "/aws" if !aws_verified => {
                status = "401 Unauthorized";
            }
            "/pages" => {
                let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
                let limit: usize = query.get("limit").ok_or("page limit")?.parse()?;
                assert_eq!(limit, fixture.limit);
                let offset: usize = query.get("offset").map_or(Ok(0), |value| value.parse())?;
                let number = offset / limit + 1;
                page = Some(number);
                let count = counts.pages.entry(number).or_default();
                *count += 1;
                page_request_count = Some(*count);
                let all = json!([
                    {"id":"one","name":"First"}, {"id":"two","name":"Second"},
                    {"id":"three","name":"Third"}, {"id":"four","name":"Fourth"},
                    {"id":"five","name":"Fifth"}
                ]);
                items = all
                    .as_array()
                    .unwrap()
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .cloned()
                    .collect();
                if number == 2 && *count == 1 {
                    status = "429 Too Many Requests";
                    extra = "retry-after: 1\r\n".into();
                    response_body = b"{}".to_vec();
                } else {
                    response_body = serde_json::to_vec(&json!({"items":items}))?;
                }
            }

            _ => {}
        }
        if !(url.path() == "/recorded" && method == "GET") {
            counts.recorded.push(json!({"method":method,"target":target,"headers":headers,"json":serde_json::from_slice::<serde_json::Value>(&body).ok(),"response":netsuite_response,"location":location,"netsuite":netsuite,"head":head,"body":body,"basic_verified":basic_verified,"key_verified":key_verified,"aws_verified":aws_verified,"page":page,"page_request_count":page_request_count,"items":items,"status":status}));
        }
        drop(counts);
        write!(
            socket,
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\n\r\n",
            response_body.len()
        )?;
        socket.write_all(&response_body)?;
        socket.flush()?;
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Arc::new(Fixture {
        limit: std::env::var("WAMN_HTTP_FIXTURE_LIMIT")?.parse()?,
    });
    let counts = Arc::new(Mutex::new(Counts::default()));
    let listener = TcpListener::bind("0.0.0.0:8080")?;
    for socket in listener.incoming() {
        let socket = socket?;
        let fixture = Arc::clone(&fixture);
        let counts = Arc::clone(&counts);
        std::thread::spawn(move || {
            serve_socket(socket, &fixture, &counts).unwrap();
        });
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_returns_requests_without_recording_its_own_reads() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let fixture = Fixture { limit: 3 };
            let counts = Mutex::new(Counts::default());
            for _ in 0..3 {
                let (socket, _) = listener.accept().unwrap();
                serve_socket(socket, &fixture, &counts).unwrap();
            }
        });
        let request = |path: &str| {
            let mut socket = TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            write!(socket, "GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic Zml4dHVyZTpmaXh0dXJlLXBhc3N3b3Jk\r\n\r\n").unwrap();
            socket.shutdown(std::net::Shutdown::Write).unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            response.split_once("\r\n\r\n").unwrap().1.to_owned()
        };
        request("/echo");
        for _ in 0..2 {
            let records: Vec<serde_json::Value> =
                serde_json::from_str(&request("/recorded")).unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0]["basic_verified"], true);
            assert!(
                records[0]["head"]
                    .as_str()
                    .unwrap()
                    .starts_with("GET /echo ")
            );
        }
        worker.join().unwrap();
    }
}

#[cfg(test)]
mod netsuite_tests {
    use super::*;

    fn fixture() -> Fixture {
        Fixture { limit: 2 }
    }

    #[test]
    fn missing_or_foreign_authentication_refuses() {
        assert!(verify_basic(&HashMap::from([(
            "authorization".into(),
            basic_header().as_str()
        )])));
        for value in ["", "Basic wrong", "Bearer private"] {
            assert!(!verify_basic(&HashMap::from([(
                "authorization".into(),
                value
            )])));
        }
    }

    #[test]
    fn authenticated_pages_records_upsert_restlet_and_refusals_are_recorded() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let counts = Arc::new(Mutex::new(Counts::default()));
        let observed = Arc::clone(&counts);
        let worker = std::thread::spawn(move || {
            for _ in 0..11 {
                let (socket, _) = listener.accept().unwrap();
                serve_socket(socket, &fixture(), &counts).unwrap();
            }
        });
        let request = |method: &str, path: &str, body: &str, signed: bool| {
            let mut socket = TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let auth = if signed {
                basic_header()
            } else {
                String::new()
            };
            write!(socket, "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {auth}\r\nPrefer: transient\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            socket.shutdown(std::net::Shutdown::Write).unwrap();
            let mut result = String::new();
            socket.read_to_string(&mut result).unwrap();
            result
        };
        for (offset, more) in [(0, true), (2, false)] {
            let result = request(
                "POST",
                &format!("/services/rest/query/v1/suiteql?offset={offset}&limit=2"),
                r#"{"q":"SELECT id FROM customer ORDER BY id"}"#,
                true,
            );
            assert!(result.starts_with("HTTP/1.1 200"));
            let body: serde_json::Value =
                serde_json::from_str(result.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["hasMore"], more);
            assert_eq!(body["items"].as_array().unwrap().len(), 2);
        }
        let created = request(
            "POST",
            "/services/rest/record/v1/customer",
            r#"{"entityId":"Created"}"#,
            true,
        );
        assert!(created.starts_with("HTTP/1.1 204"));
        assert!(created.contains("/services/rest/record/v1/customer/100\r\n"));
        assert!(created.ends_with("\r\n\r\n"));
        assert!(
            request("GET", "/services/rest/record/v1/customer/100", "", true).contains("Created")
        );
        for name in ["First", "Changed"] {
            let result = request(
                "PUT",
                "/services/rest/record/v1/customer/eid:stable",
                &format!(r#"{{"entityId":"{name}"}}"#),
                true,
            );
            assert!(result.starts_with("HTTP/1.1 204"));
            assert!(result.contains("/services/rest/record/v1/customer/eid:stable\r\n"));
        }
        assert!(
            request(
                "GET",
                "/services/rest/record/v1/customer/eid:stable",
                "",
                true
            )
            .contains("Changed")
        );
        let echo = request(
            "POST",
            "/app/site/hosting/restlet.nl?script=1&deploy=2",
            r#"{"message":"echo"}"#,
            true,
        );
        assert!(echo.ends_with(r#"{"message":"echo"}"#));
        assert!(
            request("GET", "/services/rest/record/v1/customer/100", "", false)
                .starts_with("HTTP/1.1 401")
        );
        for _ in 0..2 {
            let log = request("GET", "/recorded", "", false);
            let records: Vec<serde_json::Value> =
                serde_json::from_str(log.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(records.len(), 9);
            assert!(records[..8].iter().all(|r| r["basic_verified"] == true));
            assert_eq!(records[8]["status"], "401 Unauthorized");
            assert_eq!(records[8]["basic_verified"], false);
        }
        worker.join().unwrap();
        assert_eq!(observed.lock().unwrap().customers.len(), 2);
    }
}
