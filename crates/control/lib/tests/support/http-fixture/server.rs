//! Header-recording kind fixture, adapted from trusted_http_route::tests::serve_socket.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use base64::Engine as _;

use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::json;
use sha2::Sha256;

#[derive(Default)]
struct Counts {
    token_fetches: usize,
    pages: HashMap<usize, usize>,
    recorded: Vec<serde_json::Value>,
}

struct Fixture {
    hmac_secret: String,
    client_id: String,
    client_secret: String,
    consumer_key: String,
    consumer_secret: String,
    token: String,
    token_secret: String,
    access_token: String,
    limit: usize,
}

fn serve_socket(
    mut socket: TcpStream,
    fixture: &Fixture,
    counts: &Mutex<Counts>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
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
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim()))
            .collect();
        let length: usize = headers
            .get("content-length")
            .copied()
            .unwrap_or("0")
            .parse()?;
        assert!(length <= 65_536, "fixture body exceeds 64 KiB");
        let mut body = vec![0_u8; length];
        socket.read_exact(&mut body)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(fixture.hmac_secret.as_bytes())?;
        mac.update(&body);
        let signature_verified = headers
            .get("x-wamn-signature")
            .and_then(|value| value.strip_prefix("sha256="))
            .and_then(|value| hex::decode(value).ok())
            .is_some_and(|signature| mac.verify_slice(&signature).is_ok());
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
        let mut counts = counts.lock().unwrap();
        let mut status = "200 OK";
        let mut extra = "";
        let mut response_body = body.clone();
        let bearer_verified = headers.get("authorization").copied()
            == Some(format!("Bearer {}", fixture.access_token).as_str());
        let oauth1_verified =
            verify_oauth1(fixture, method, &url, headers.get("authorization").copied());
        let oauth_nonce = headers
            .get("authorization")
            .and_then(|header| {
                header
                    .split(',')
                    .find_map(|field| field.trim().strip_prefix("oauth_nonce="))
            })
            .map(|value| value.trim_matches('"'));
        let mut token_verified = false;
        let mut page = None;
        let mut page_request_count = None;
        let mut items = serde_json::Value::Null;
        match url.path() {
            "/recorded" if method == "GET" => {
                response_body = serde_json::to_vec(&counts.recorded)?;
            }
            "/oauth/token" => {
                counts.token_fetches += 1;
                let basic = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{}:{}", fixture.client_id, fixture.client_secret))
                );
                let form: HashMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
                token_verified = headers.get("authorization") == Some(&basic.as_str())
                    && form.get("grant_type").map(String::as_str) == Some("client_credentials");
                if token_verified {
                    response_body = serde_json::to_vec(
                        &json!({"access_token":fixture.access_token, "token_type":"Bearer", "expires_in":3600}),
                    )?;
                } else {
                    status = "401 Unauthorized";
                }
            }
            "/oauth/items" => {
                if !bearer_verified {
                    status = "401 Unauthorized";
                }
            }
            "/oauth1" => {
                if !oauth1_verified {
                    status = "401 Unauthorized";
                }
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
                    extra = "retry-after: 1\r\n";
                    response_body = b"{}".to_vec();
                } else {
                    response_body = serde_json::to_vec(&json!({"items":items}))?;
                }
            }
            _ => {}
        }
        if url.path() != "/recorded" {
            let record = json!({
                "head": head, "body": body, "signature_verified": signature_verified,
                "bearer_verified": bearer_verified, "token_verified": token_verified,
                "oauth1_verified": oauth1_verified, "oauth_nonce": oauth_nonce,
                "page": page, "page_request_count": page_request_count,
                "items": items, "status": status, "token_fetches": counts.token_fetches,
            });
            counts.recorded.push(record);
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
        hmac_secret: std::env::var("WAMN_HTTP_FIXTURE_HMAC_SECRET")?,
        client_id: std::env::var("WAMN_HTTP_FIXTURE_CLIENT_ID")?,
        client_secret: std::env::var("WAMN_HTTP_FIXTURE_CLIENT_SECRET")?,
        consumer_key: std::env::var("WAMN_HTTP_FIXTURE_CONSUMER_KEY")?,
        consumer_secret: std::env::var("WAMN_HTTP_FIXTURE_CONSUMER_SECRET")?,
        token: std::env::var("WAMN_HTTP_FIXTURE_TOKEN")?,
        token_secret: std::env::var("WAMN_HTTP_FIXTURE_TOKEN_SECRET")?,
        access_token: std::env::var("WAMN_HTTP_FIXTURE_ACCESS_TOKEN")?,
        limit: std::env::var("WAMN_HTTP_FIXTURE_LIMIT")?.parse()?,
    });
    let counts = Arc::new(Mutex::new(Counts::default()));
    for socket in TcpListener::bind("0.0.0.0:8080")?.incoming() {
        let socket = socket?;
        let fixture = Arc::clone(&fixture);
        let counts = Arc::clone(&counts);
        std::thread::spawn(move || {
            serve_socket(socket, &fixture, &counts).expect("serve the fixture request")
        });
    }
    Ok(())
}

// Independent RFC 5849 verifier. Form decoding happens before OAuth encoding.
fn oauth_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(encoded, "%{byte:02X}").unwrap();
        }
    }
    encoded
}

fn verify_oauth1(
    fixture: &Fixture,
    method: &str,
    url: &url::Url,
    authorization: Option<&str>,
) -> bool {
    let Some(header) = authorization.and_then(|value| value.strip_prefix("OAuth ")) else {
        return false;
    };
    let mut oauth = HashMap::new();
    for field in header.split(',') {
        let Some((key, value)) = field.trim().split_once('=') else {
            return false;
        };
        let Some(value) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            return false;
        };
        let decoded = url::form_urlencoded::parse(format!("v={value}").as_bytes())
            .next()
            .unwrap()
            .1
            .into_owned();
        if oauth.insert(key.to_owned(), decoded).is_some() {
            return false;
        }
    }
    for (key, expected) in [
        ("oauth_consumer_key", fixture.consumer_key.as_str()),
        ("oauth_token", fixture.token.as_str()),
        ("oauth_signature_method", "HMAC-SHA256"),
        ("oauth_version", "1.0"),
    ] {
        if oauth.get(key).map(String::as_str) != Some(expected) {
            return false;
        }
    }
    if !oauth
        .get("oauth_nonce")
        .is_some_and(|value| !value.is_empty())
        || !oauth
            .get("oauth_timestamp")
            .is_some_and(|value| value.parse::<u64>().is_ok())
    {
        return false;
    }
    let Some(signature) = oauth.remove("oauth_signature") else {
        return false;
    };
    oauth.remove("realm");
    let mut params: Vec<_> = url
        .query_pairs()
        .map(|(key, value)| (oauth_encode(&key), oauth_encode(&value)))
        .chain(
            oauth
                .iter()
                .map(|(key, value)| (oauth_encode(key), oauth_encode(value))),
        )
        .collect();
    params.sort();
    let normalized = params
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let mut base_url = url.clone();
    base_url.set_query(None);
    base_url.set_fragment(None);
    let base = format!(
        "{}&{}&{}",
        method.to_ascii_uppercase(),
        oauth_encode(base_url.as_str()),
        oauth_encode(&normalized)
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(
        format!(
            "{}&{}",
            oauth_encode(&fixture.consumer_secret),
            oauth_encode(&fixture.token_secret)
        )
        .as_bytes(),
    )
    .unwrap();
    mac.update(base.as_bytes());
    base64::engine::general_purpose::STANDARD
        .decode(signature)
        .ok()
        .is_some_and(|signature| mac.verify_slice(&signature).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_returns_requests_without_recording_its_own_reads() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let fixture = Fixture {
                hmac_secret: "recorded-test".into(),
                client_id: String::new(),
                client_secret: String::new(),
                consumer_key: String::new(),
                consumer_secret: String::new(),
                token: String::new(),
                token_secret: String::new(),
                access_token: "recorded-token".into(),
                limit: 3,
            };
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
            write!(socket, "GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer recorded-token\r\n\r\n").unwrap();
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
            assert_eq!(records[0]["bearer_verified"], true);
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
