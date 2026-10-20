//! Header-recording kind fixture, adapted from trusted_http_route::tests::serve_socket.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::json;
use sha2::Sha256;

fn serve_socket(mut socket: TcpStream, secret: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
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
            .ok_or("fixture request has no content length")?
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
        // Record every request before answering. TCP readiness opens no request.
        {
            let mut log = std::io::stdout().lock();
            writeln!(
                log,
                "{}",
                json!({
                    "head": head, "body": body, "signature_verified": signature_verified
                })
            )?;
            log.flush()?;
        }
        write!(
            socket,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        )?;
        socket.write_all(&body)?;
        socket.flush()?;
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret: Arc<str> = std::env::var("WAMN_HTTP_FIXTURE_HMAC_SECRET")?.into();
    assert!(!secret.is_empty(), "fixture HMAC secret is empty");
    for socket in TcpListener::bind("0.0.0.0:8080")?.incoming() {
        let socket = socket?;
        let secret = Arc::clone(&secret);
        std::thread::spawn(move || {
            serve_socket(socket, secret.as_bytes()).expect("serve the fixture request")
        });
    }
    Ok(())
}
