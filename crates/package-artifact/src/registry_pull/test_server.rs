//! Loopback HTTP fixture shared by the registry reader unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[derive(Debug)]
pub struct Server {
    pub reference: oci_client::Reference,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    pub async fn start(responses: Vec<(u16, Vec<u8>)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                let (status, body) = if request.starts_with(b"GET /v2/ HTTP/") {
                    (200, &b""[..])
                } else {
                    let index = count.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = &responses[index.min(responses.len() - 1)];
                    (*status, body.as_slice())
                };
                let header = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/vnd.oci.image.manifest.v1+json\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if socket.write_all(header.as_bytes()).await.is_ok() {
                    let _ = socket.write_all(body).await;
                }
            }
        });
        Self {
            reference: format!("{address}/components:fixture").parse().unwrap(),
            requests,
            task,
        }
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
