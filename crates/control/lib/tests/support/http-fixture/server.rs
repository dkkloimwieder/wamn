//! Header-recording kind fixture, adapted from trusted_http_route::tests::serve_socket.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use base64::Engine as _;

mod netsuite;

use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::json;
use sha2::Sha256;

#[derive(Default)]
struct Counts {
    token_fetches: usize,
    pages: HashMap<usize, usize>,
    recorded: Vec<serde_json::Value>,
    customers: HashMap<String, serde_json::Value>,
}

struct Fixture {
    hmac_secret: String,
    client_id: String,
    client_secret: String,
    consumer_key: String,
    consumer_secret: String,
    token: String,
    token_secret: String,
    realm: String,
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
        let mut extra = String::new();
        let mut response_body = body.clone();
        let bearer_verified = headers.get("authorization").copied()
            == Some(format!("Bearer {}", fixture.access_token).as_str());
        let oauth1_verified = verify_oauth1(fixture, method, &url, &headers, &body);
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
        let netsuite = url.path().starts_with("/services/rest/")
            || url.path() == "/app/site/hosting/restlet.nl";
        let mut netsuite_response = serde_json::Value::Null;
        let mut location = None;
        match url.path() {
            _ if netsuite => {
                let response = if oauth1_verified {
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
                    extra = "retry-after: 1\r\n".into();
                    response_body = b"{}".to_vec();
                } else {
                    response_body = serde_json::to_vec(&json!({"items":items}))?;
                }
            }
            _ => {}
        }
        if !(url.path() == "/recorded" && method == "GET") {
            let record = json!({
                "method": method, "target": target, "headers": headers,
                "json": serde_json::from_slice::<serde_json::Value>(&body).ok(),
                "response": netsuite_response, "location": location, "netsuite": netsuite,
                "realm": if netsuite && oauth1_verified { Some(&fixture.realm) } else { None },
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
        realm: std::env::var("WAMN_HTTP_FIXTURE_REALM")?,
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

// Independent verifier: query plus signs remain literal; only form bodies use form decoding.
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
    headers: &HashMap<String, &str>,
    body: &[u8],
) -> bool {
    let Some(header) = headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("OAuth "))
    else {
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
        let decoded = if key == "realm" {
            value.replace("\\\"", "\"").replace("\\\\", "\\")
        } else {
            percent_encoding::percent_decode_str(value)
                .decode_utf8_lossy()
                .into_owned()
        };
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
    let realm = oauth.remove("realm");
    if (url.path().starts_with("/services/rest/") || url.path() == "/app/site/hosting/restlet.nl")
        && realm.as_deref() != Some(fixture.realm.as_str())
    {
        return false;
    }
    let mut params: Vec<_> = url
        .query()
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                oauth_encode(&percent_encoding::percent_decode_str(key).decode_utf8_lossy()),
                oauth_encode(&percent_encoding::percent_decode_str(value).decode_utf8_lossy()),
            )
        })
        .chain(
            oauth
                .iter()
                .map(|(key, value)| (oauth_encode(key), oauth_encode(value))),
        )
        .collect();
    if headers
        .get("content-type")
        .is_some_and(|value| value.split(';').next() == Some("application/x-www-form-urlencoded"))
    {
        params.extend(
            url::form_urlencoded::parse(body)
                .map(|(key, value)| (oauth_encode(&key), oauth_encode(&value))),
        );
    }
    params.retain(|(key, _)| key != "oauth_signature");
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
                realm: "test-realm".into(),
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

#[cfg(test)]
mod netsuite_tests {
    use super::*;

    fn fixture() -> Fixture {
        Fixture {
            hmac_secret: "unused".into(),
            client_id: String::new(),
            client_secret: String::new(),
            consumer_key: "consumer".into(),
            consumer_secret: "secret&".into(),
            token: "token".into(),
            token_secret: "token+secret".into(),
            realm: "123_SB1".into(),
            access_token: String::new(),
            limit: 2,
        }
    }

    // This signer builds the base independently with the URL form encoder,
    // correcting its space/star rules. It calls no production signing helper.
    fn encode(value: &str) -> String {
        url::form_urlencoded::byte_serialize(value.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
            .replace('*', "%2A")
            .replace("%7E", "~")
    }

    fn sign(method: &str, authority: &str, target: &str) -> String {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let mut pairs = vec![
            ("oauth_consumer_key".to_owned(), "consumer".to_owned()),
            ("oauth_token".into(), "token".into()),
            ("oauth_nonce".into(), "nonce".into()),
            ("oauth_timestamp".into(), "1234567890".into()),
            ("oauth_signature_method".into(), "HMAC-SHA256".into()),
            ("oauth_version".into(), "1.0".into()),
        ];
        let mut header: Vec<_> = pairs
            .iter()
            .map(|(k, v)| format!("{k}=\"{}\"", encode(v)))
            .collect();
        pairs.extend(query.split('&').filter(|p| !p.is_empty()).map(|p| {
            let (k, v) = p.split_once('=').unwrap();
            (k.to_owned(), v.to_owned())
        }));
        pairs.sort();
        let normalized = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let base = format!(
            "{method}&{}&{}",
            encode(&format!("http://{authority}{path}")),
            encode(&normalized)
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret%26&token%2Bsecret").unwrap();
        mac.update(base.as_bytes());
        let signature =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        header.push(format!("oauth_signature=\"{}\"", encode(&signature)));
        format!("OAuth realm=\"123_SB1\", {}", header.join(", "))
    }

    #[test]
    fn independent_sha256_vector_requires_signature_and_realm() {
        let fixture = fixture();
        let target = "/services/rest/record/v1/customer/42?x=a+b";
        let url = url::Url::parse(&format!("http://example.test{target}")).unwrap();
        let header = sign("GET", "example.test", target);
        let verify = |h: &str| {
            verify_oauth1(
                &fixture,
                "GET",
                &url,
                &HashMap::from([("authorization".into(), h)]),
                b"",
            )
        };
        assert!(verify(&header));
        let mut wrong_secret = super::netsuite_tests::fixture();
        wrong_secret.consumer_secret.push('x');
        assert!(!verify_oauth1(
            &wrong_secret,
            "GET",
            &url,
            &HashMap::from([("authorization".into(), header.as_str())]),
            b""
        ));
        assert!(!verify(&header.replace("123_SB1", "other")));
        assert!(!verify(&header.replace("realm=\"123_SB1\", ", "")));
        assert!(!verify(
            &header.replace("oauth_signature=", "bad_signature=")
        ));
        assert!(!verify(""));
        assert!(!verify(&header.replace("consumer\"", "wrong\"")));
    }

    #[test]
    fn signed_pages_records_upsert_restlet_and_refusals_are_recorded() {
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
                sign(method, &address.to_string(), path)
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
            assert!(
                records[..8]
                    .iter()
                    .all(|r| r["oauth1_verified"] == true && r["realm"] == "123_SB1")
            );
            assert_eq!(records[8]["status"], "401 Unauthorized");
            assert_eq!(records[8]["oauth1_verified"], false);
        }
        worker.join().unwrap();
        assert_eq!(observed.lock().unwrap().customers.len(), 2);
    }
}
