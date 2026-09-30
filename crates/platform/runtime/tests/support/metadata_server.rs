//! A loopback stand-in for the GKE metadata server's token endpoint.

use std::sync::{Arc, Mutex};

/// Answers each request with the next fixed response and records its head.
pub struct MetadataServer {
    pub token_url: String,
    heads: Arc<Mutex<Vec<String>>>,
    listening: tokio::task::JoinHandle<()>,
}

impl MetadataServer {
    /// The request heads received so far, in order.
    pub fn heads(&self) -> Vec<String> {
        self.heads.lock().expect("heads lock").clone()
    }
}

impl Drop for MetadataServer {
    fn drop(&mut self) {
        self.listening.abort();
    }
}

/// Bind an ephemeral port that answers the `n`th request with `answers[n]`,
/// and every later one with the last answer.
pub async fn metadata_server(answers: Vec<(u16, &'static str)>) -> MetadataServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let token_url = format!(
        "http://{}/computeMetadata/v1/instance/service-accounts/default/token",
        listener
            .local_addr()
            .expect("the listener reports its port")
    );
    let heads = Arc::new(Mutex::new(Vec::new()));
    let recorded = heads.clone();
    let listening = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut head = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => head.extend_from_slice(&chunk[..read]),
                }
            }
            let index = {
                let mut heads = recorded.lock().expect("heads lock");
                heads.push(String::from_utf8_lossy(&head).into_owned());
                heads.len() - 1
            };
            let (status, body) = answers[index.min(answers.len() - 1)];
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    MetadataServer {
        token_url,
        heads,
        listening,
    }
}
