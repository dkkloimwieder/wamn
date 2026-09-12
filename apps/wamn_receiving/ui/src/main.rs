//! The Receiving composition over generated screens and the shared terminal loop.
//!
//! `WAMN_BASE_URL`, `WAMN_HOST`, and `WAMN_TARGET_INSTANCE` bind this client to
//! the served activation. `WAMN_TOKEN` supplies the operator PAT.
//! `WAMN_SESSION_ISSUER` and `WAMN_SESSION_AUDIENCE` optionally select session
//! login. Configure both or neither. Login completes before terminal entry.
//!
//! The developer session in `docs/operations/development-loop.md` supplies
//! the launch commands and interaction keys.

use std::error::Error;
use std::io;
use std::sync::Arc;

use wamn_client::{ClientError, HttpRequest, HttpResponse, Transport, WamnClient};
use wamn_client_terminal::operator::{ExitReason, run_application_with_client};
use wamn_client_tui::submission::SessionBinding;
use wamn_receiving_tui::{ReceivingApplication, login};

/// One HTTP exchange over the real network.
#[derive(Debug)]
struct HttpTransport {
    client: reqwest::Client,
}

fn http_transport(builder: reqwest::ClientBuilder) -> Result<HttpTransport, ClientError> {
    let client = builder
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ClientError::Transport {
            detail: "HTTP client configuration failed".to_owned(),
        })?;
    Ok(HttpTransport { client })
}

#[async_trait::async_trait]
impl Transport for HttpTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        let failed = |_: reqwest::Error| ClientError::Transport {
            detail: "HTTP exchange failed".to_owned(),
        };
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| {
            ClientError::Transport {
                detail: "HTTP method is invalid".to_owned(),
            }
        })?;
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let response = builder.body(request.body).send().await.map_err(failed)?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(failed)?;
        Ok(HttpResponse { status, body })
    }
}

fn required_env(name: &str, message: &str) -> io::Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, message))
}

fn session_setting(name: &str) -> io::Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must contain Unicode text"),
        )),
    }
}

#[tokio::main]
async fn main() -> Result<ExitReason, Box<dyn Error>> {
    let base_url = required_env(
        "WAMN_BASE_URL",
        "WAMN_BASE_URL must name the deployment this client talks to",
    )?;
    let token = required_env(
        "WAMN_TOKEN",
        "WAMN_TOKEN must carry the operator's access token",
    )?;
    let binding = SessionBinding {
        url: base_url,
        host: std::env::var("WAMN_HOST")
            .ok()
            .filter(|host| !host.is_empty()),
        target_instance: required_env(
            "WAMN_TARGET_INSTANCE",
            "WAMN_TARGET_INSTANCE must identify the served activation",
        )?,
    };
    let issuer = session_setting("WAMN_SESSION_ISSUER")?;
    let audience = session_setting("WAMN_SESSION_AUDIENCE")?;
    let target = login::session_target(issuer.as_deref(), audience.as_deref())?;
    let transport: Arc<dyn Transport> = Arc::new(http_transport(reqwest::Client::builder())?);
    let credentials = login::credentials(token, target, transport.clone()).await?;
    let client = Arc::new(WamnClient::new(
        binding.url.clone(),
        binding.host.clone(),
        credentials,
        transport,
    ));
    run_application_with_client("Receiving", binding, client, ReceivingApplication::new).await
}

#[cfg(test)]
mod tests {
    use super::http_transport;
    use std::collections::BTreeMap;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::TcpListener;
    use std::time::Duration;
    use wamn_client::{HttpRequest, Transport as _};

    #[tokio::test]
    async fn the_real_transport_returns_a_redirect_without_following_it() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("redirect fixture listener");
        let address = listener.local_addr().expect("redirect fixture address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("one request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("bounded header read");
            let mut reader = BufReader::new(&stream);
            let mut headers = String::new();
            while !headers.ends_with("\r\n\r\n") {
                assert!(reader.read_line(&mut headers).expect("request header") > 0);
                assert!(headers.len() < 8192, "bounded request headers");
            }
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{address}/other\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .expect("redirect response");
        });
        let transport = http_transport(
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5)),
        )
        .expect("real HTTP transport");
        let response = transport
            .send(HttpRequest {
                url: format!("http://{address}/session"),
                method: "POST".to_owned(),
                headers: BTreeMap::from([(
                    "authorization".to_owned(),
                    "Bearer receiving-redirect-fixture-pat".to_owned(),
                )]),
                body: Vec::new(),
            })
            .await;
        server.join().expect("redirect fixture completed");
        assert_eq!(response.expect("one redirect response").status, 307);
    }

    #[tokio::test]
    async fn transport_failures_do_not_render_request_credentials() {
        let sentinel = "receiving-error-fixture-pat";
        let transport =
            http_transport(reqwest::Client::builder().no_proxy()).expect("real HTTP transport");
        let error = transport
            .send(HttpRequest {
                url: format!("invalid-relative-url/{sentinel}"),
                method: "POST".to_owned(),
                headers: BTreeMap::from([(
                    "authorization".to_owned(),
                    format!("Bearer {sentinel}"),
                )]),
                body: Vec::new(),
            })
            .await
            .expect_err("refuse the invalid request URL");
        assert!(!format!("{error:?} {error}").contains(sentinel));
    }
}
