//! Header-recording kind fixture, adapted from trusted_http_route::tests::serve_socket.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use base64::Engine as _;

use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::json;
use sha2::Sha256;

fn serve_socket(
    mut socket: TcpStream,
    secret: &[u8],
    counts: &Mutex<HashMap<String, usize>>,
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
        let mut mac = Hmac::<Sha256>::new_from_slice(secret)?;
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
        let count = counts.entry(target.to_owned()).or_default();
        *count += 1;
        let request_count = *count;
        let mut status = "200 OK";
        let mut extra = "";
        let mut response_body = body.clone();
        let bearer_verified = headers.get("authorization") == Some(&"Bearer fixture-issued-token");
        let oauth1_verified = verify_oauth1(method, &url, headers.get("authorization").copied());
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
        let mut items = serde_json::Value::Null;
        match url.path() {
            "/oauth/token" => {
                let basic = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode("fixture-client:fixture-client-secret")
                );
                let form: HashMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
                token_verified = headers.get("authorization") == Some(&basic.as_str())
                    && form.get("grant_type").map(String::as_str) == Some("client_credentials");
                if token_verified {
                    response_body = serde_json::to_vec(
                        &json!({"access_token":"fixture-issued-token", "token_type":"Bearer", "expires_in":3600}),
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
                assert_eq!(limit, 2);
                let offset: usize = query.get("offset").map_or(Ok(0), |value| value.parse())?;
                page = Some(offset / limit + 1);
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
                if offset == 2 && request_count == 1 {
                    status = "429 Too Many Requests";
                    extra = "retry-after: 1\r\n";
                    response_body = b"{}".to_vec();
                } else {
                    response_body = serde_json::to_vec(&json!({"items":items}))?;
                }
            }
            _ => {}
        }
        // Record every request before answering. TCP readiness opens no request.
        {
            let mut log = std::io::stdout().lock();
            writeln!(
                log,
                "{}",
                json!({
                    "head": head, "body": body, "signature_verified": signature_verified,
                    "bearer_verified": bearer_verified, "token_verified": token_verified,
                    "oauth1_verified": oauth1_verified, "oauth_nonce": oauth_nonce, "request_count": request_count,
                    "page": page, "items": items, "status": status,
                    "token_fetches": counts.get("/oauth/token").copied().unwrap_or(0),
                })
            )?;
            log.flush()?;
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
    let secret: Arc<str> = std::env::var("WAMN_HTTP_FIXTURE_HMAC_SECRET")?.into();
    assert!(!secret.is_empty(), "fixture HMAC secret is empty");
    let counts = Arc::new(Mutex::new(HashMap::new()));
    for socket in TcpListener::bind("0.0.0.0:8080")?.incoming() {
        let socket = socket?;
        let secret = Arc::clone(&secret);
        let counts = Arc::clone(&counts);
        std::thread::spawn(move || {
            serve_socket(socket, secret.as_bytes(), &counts).expect("serve the fixture request")
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

fn verify_oauth1(method: &str, url: &url::Url, authorization: Option<&str>) -> bool {
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
        ("oauth_consumer_key", "fixture-consumer"),
        ("oauth_token", "fixture-token"),
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
    let mut mac =
        Hmac::<Sha256>::new_from_slice(b"fixture-consumer-secret&fixture-token-secret").unwrap();
    mac.update(base.as_bytes());
    base64::engine::general_purpose::STANDARD
        .decode(signature)
        .ok()
        .is_some_and(|signature| mac.verify_slice(&signature).is_ok())
}
