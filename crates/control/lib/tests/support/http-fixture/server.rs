//! Header-recording kind fixture, adapted from trusted_http_route::tests::serve_socket.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};

fn serve_socket(mut socket: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
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
        // Record every request before answering. TCP readiness opens no request.
        {
            let mut log = std::io::stdout().lock();
            log.write_all(head.as_bytes())?;
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
    for socket in TcpListener::bind("0.0.0.0:8080")?.incoming() {
        let socket = socket?;
        std::thread::spawn(move || serve_socket(socket).expect("serve the fixture request"));
    }
    Ok(())
}
