//! The Receiving composition over generated screens and the shared terminal loop.
//!
//! `WAMN_BASE_URL`, `WAMN_HOST`, and `WAMN_TARGET_INSTANCE` bind this client to
//! the served activation. `WAMN_TOKEN` supplies the operator PAT.
//! `WAMN_SESSION_ISSUER` and `WAMN_SESSION_AUDIENCE` optionally select session
//! exchange with a PAT. Without a PAT they select interactive password login.
//! `WAMN_SESSION_CA` optionally supplies the trusted issuer certificate bundle.
//! `WAMN_RECEIVING_TARGETS` selects a public deployment file for password login
//! across several environments under the configured issuer.
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
        .no_proxy()
        .retry(reqwest::retry::never())
        .timeout(std::time::Duration::from_secs(15))
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
        let actor_labels = response
            .headers()
            .get("wamn-actor-labels")
            .and_then(|value| serde_json::from_slice(value.as_bytes()).ok())
            .unwrap_or_default();
        let body = response.text().await.map_err(failed)?;
        Ok(HttpResponse {
            actor_labels,
            status,
            body,
        })
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
    let token = match std::env::var("WAMN_TOKEN") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(
                io::Error::other("WAMN_TOKEN must carry the operator's access token").into(),
            );
        }
    };
    let issuer = session_setting("WAMN_SESSION_ISSUER")?;
    let audience = session_setting("WAMN_SESSION_AUDIENCE")?;
    let targets_file = session_setting("WAMN_RECEIVING_TARGETS")?;
    let (mut binding, target, environments) = if let Some(path) = targets_file {
        if token.is_some() {
            return Err(io::Error::other("WAMN_RECEIVING_TARGETS is for password login; use the single-target configuration for PAT access").into());
        }
        let bytes = std::fs::read(path)
            .map_err(|_| io::Error::other("read Receiving environment configuration failed"))?;
        let environments = login::environment_targets(&bytes)?;
        (environments[0].binding.clone(), None, environments)
    } else {
        let binding = SessionBinding {
            url: required_env(
                "WAMN_BASE_URL",
                "WAMN_BASE_URL must name the deployment this client talks to",
            )?,
            host: std::env::var("WAMN_HOST")
                .ok()
                .filter(|host| !host.is_empty()),
            target_instance: required_env(
                "WAMN_TARGET_INSTANCE",
                "WAMN_TARGET_INSTANCE must identify the served activation",
            )?,
        };
        let target = login::session_target(issuer.as_deref(), audience.as_deref())?;
        let environments = audience
            .map(|audience| login::EnvironmentTarget {
                audience,
                binding: binding.clone(),
            })
            .into_iter()
            .collect();
        (binding, target, environments)
    };
    let transport: Arc<dyn Transport> = Arc::new(http_transport(reqwest::Client::builder())?);
    let mut identity_builder = reqwest::Client::builder().https_only(true);
    if let Some(path) = session_setting("WAMN_SESSION_CA")? {
        let pem = std::fs::read(path).map_err(|_| io::Error::other("read identity CA failed"))?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|_| io::Error::other("identity CA refused"))?;
        if certificates.is_empty() {
            return Err(io::Error::other("identity CA is empty").into());
        }
        identity_builder = identity_builder.tls_certs_only(certificates);
    }
    let identity_transport: Arc<dyn Transport> = Arc::new(http_transport(identity_builder)?);
    let mut password_session = None;
    let credentials = if let Some(token) = token {
        login::credentials(token, target, identity_transport).await?
    } else {
        let issuer = issuer
            .ok_or_else(|| io::Error::other("configure WAMN_SESSION_ISSUER for password login"))?;
        let audiences = environments
            .iter()
            .map(|target| target.audience.clone())
            .collect::<Vec<_>>();
        match wamn_client_terminal::login::password_login(&issuer, &audiences, identity_transport)
            .await?
        {
            wamn_client_terminal::login::PasswordLogin::Exit(reason) => return Ok(reason),
            wamn_client_terminal::login::PasswordLogin::Authenticated { index, credentials } => {
                binding = environments[index].binding.clone();
                let session = credentials;
                password_session = Some(session.clone());
                session as Arc<dyn wamn_client::CredentialProvider>
            }
        }
    };
    let client = Arc::new(WamnClient::new(
        binding.url.clone(),
        binding.host.clone(),
        credentials,
        transport,
    ));
    let result =
        run_application_with_client("Receiving", binding, client, ReceivingApplication::new).await;
    if let Some(session) = password_session {
        match session.logout().await {
            Ok(()) => eprintln!(
                "Logged out. Local credentials cleared. This login can no longer start application requests."
            ),
            Err(error) => eprintln!("{error}"),
        }
    }
    result
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
