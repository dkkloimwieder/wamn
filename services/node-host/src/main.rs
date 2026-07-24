//! Production HTTP shell for one warm custom node component.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, SIGNATURE_HEADER, SIGNING_KEY_CREDENTIAL,
    SIGNING_KEY_CREDENTIAL_PREVIOUS, SignatureError, TIMESTAMP_HEADER, WireErrorDetail,
    WireNodeError, timestamp_fresh, verify_envelope_with_timestamp,
};
use wamn_node_runtime::{
    AllowedHostsEgress, CredentialLookupError, CredentialProvider, DEFAULT_NODE_ID, NodeRuntime,
    NodeRuntimeConfig,
};

#[derive(Debug, Parser)]
#[command(name = "wamn-node-host", version, about)]
struct Args {
    #[arg(long = "log-level", default_value = "info")]
    log_level: String,

    /// Custom-node component exporting `wamn:node/handler`.
    #[arg(long)]
    node: PathBuf,

    /// TCP port serving `POST /run`.
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Trusted project identity injected by deployment configuration.
    #[arg(long, env = "WAMN_PROJECT", default_value = "default")]
    project: String,

    /// Mounted `{project: {name: secret}}` credential source.
    #[arg(long, env = "WAMN_CREDENTIALS_FILE")]
    credentials_file: Option<PathBuf>,

    /// Hosts the node's outbound `wasi:http` may reach.
    #[arg(
        long = "allowed-hosts",
        env = "WAMN_ALLOWED_HOSTS",
        value_delimiter = ','
    )]
    allowed_hosts: Vec<String>,

    /// Refuse every invocation if the project signing key is absent.
    #[arg(long = "require-signing-key", env = "WAMN_REQUIRE_SIGNING_KEY")]
    require_signing_key: bool,

    /// Maximum accepted signature age in seconds.
    #[arg(long = "signature-max-age-secs", env = "WAMN_SIGNATURE_MAX_AGE_SECS")]
    signature_max_age_secs: Option<u64>,
}

struct FileCredentials {
    has_source: bool,
    projects: HashMap<String, HashMap<String, String>>,
}

impl FileCredentials {
    fn empty() -> Self {
        Self {
            has_source: false,
            projects: HashMap::new(),
        }
    }

    fn from_file(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            tracing::warn!(
                path = %path.display(),
                "credentials file not found — every credential read is unavailable"
            );
            return Ok(Self::empty());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read credentials file {}", path.display()))?;
        let projects = serde_json::from_str(&text)
            .context("credentials file must be a JSON object of project/name string maps")?;
        Ok(Self {
            has_source: true,
            projects,
        })
    }

    fn lookup(&self, project: &str, name: &str) -> Option<String> {
        self.projects
            .get(project)
            .and_then(|credentials| credentials.get(name))
            .cloned()
    }
}

impl CredentialProvider for FileCredentials {
    fn get(&self, project: &str, name: &str) -> Result<String, CredentialLookupError> {
        if !self.has_source {
            return Err(CredentialLookupError::Unavailable);
        }
        self.lookup(project, name)
            .ok_or(CredentialLookupError::NotFound)
    }
}

struct RequestAuth {
    current: Option<Vec<u8>>,
    previous: Option<Vec<u8>>,
    require_key: bool,
    max_age_secs: Option<u64>,
}

impl RequestAuth {
    fn new(
        credentials: &FileCredentials,
        project: &str,
        require_key: bool,
        max_age_secs: Option<u64>,
    ) -> Self {
        Self {
            current: credentials
                .lookup(project, SIGNING_KEY_CREDENTIAL)
                .map(String::into_bytes),
            previous: credentials
                .lookup(project, SIGNING_KEY_CREDENTIAL_PREVIOUS)
                .map(String::into_bytes),
            require_key,
            max_age_secs,
        }
    }

    fn verify(
        &self,
        body: &[u8],
        signature: Option<&str>,
        timestamp: Option<&str>,
        now_secs: u64,
    ) -> Result<(), SignatureError> {
        let Some(current) = self.current.as_deref() else {
            return if self.require_key {
                Err(SignatureError::Unconfigured)
            } else {
                Ok(())
            };
        };
        let signature = signature.ok_or(SignatureError::Missing)?;
        if let Err(error) = verify_envelope_with_timestamp(current, body, timestamp, signature) {
            match self.previous.as_deref() {
                Some(previous) => {
                    verify_envelope_with_timestamp(previous, body, timestamp, signature)?
                }
                None => return Err(error),
            }
        }
        if let Some(max_age) = self.max_age_secs {
            let timestamp = timestamp.ok_or(SignatureError::MissingTimestamp)?;
            let timestamp = timestamp
                .trim()
                .parse()
                .map_err(|_| SignatureError::MalformedTimestamp)?;
            if !timestamp_fresh(timestamp, now_secs, max_age) {
                return Err(SignatureError::Stale);
            }
        }
        Ok(())
    }
}

struct NodeHost {
    runtime: NodeRuntime,
    auth: RequestAuth,
}

impl NodeHost {
    async fn serve(self: Arc<Self>, port: u16) -> anyhow::Result<()> {
        let listener = TcpListener::bind(("0.0.0.0", port)).await?;
        tracing::info!(port, "node-host up (POST /run)");
        loop {
            let (socket, peer) = listener.accept().await?;
            if let Err(error) = serve_connection(socket, &self).await {
                tracing::warn!(%peer, %error, "node-host connection error");
            }
        }
    }
}

async fn serve_connection(socket: TcpStream, host: &NodeHost) -> anyhow::Result<()> {
    socket.set_nodelay(true)?;
    let mut reader = BufReader::new(socket);
    loop {
        let Some((body, signature, timestamp)) = read_request(&mut reader).await? else {
            break;
        };
        if let Err(error) = host.auth.verify(
            &body,
            signature.as_deref(),
            timestamp.as_deref(),
            now_unix_secs(),
        ) {
            let response = unauthorized_response(error);
            reader.get_mut().write_all(response.as_bytes()).await?;
            reader.get_mut().flush().await?;
            continue;
        }
        let response = match NodeInvokeRequest::from_json(&String::from_utf8_lossy(&body)) {
            Ok(request) => host.runtime.invoke(request).await,
            Err(error) => NodeInvokeResponse::Err(WireNodeError::InvalidInput(WireErrorDetail {
                message: format!("malformed invocation envelope: {error}"),
                code: Some("bad-envelope".to_string()),
                data: None,
            })),
        };
        let body = response.to_json();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            body.len(),
            body
        );
        reader.get_mut().write_all(response.as_bytes()).await?;
        reader.get_mut().flush().await?;
    }
    Ok(())
}

fn unauthorized_response(error: SignatureError) -> String {
    let body = format!(
        r#"{{"error":"invocation-unauthorized","reason":"{}"}}"#,
        error.reason()
    );
    format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
        body.len(),
        body
    )
}

async fn read_request<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> anyhow::Result<Option<(Vec<u8>, Option<String>, Option<String>)>> {
    let mut content_length = 0usize;
    let mut chunked = false;
    let mut saw_any = false;
    let mut signature = None;
    let mut timestamp = None;
    let mut line = String::new();
    loop {
        line.clear();
        let count = reader.read_line(&mut line).await?;
        if count == 0 {
            return if saw_any {
                bail!("connection closed mid-headers")
            } else {
                Ok(None)
            };
        }
        saw_any = true;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = lower.strip_prefix("transfer-encoding:") {
            chunked = value.contains("chunked");
        } else if let Some(value) = lower
            .strip_prefix(SIGNATURE_HEADER)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            signature = Some(value.trim().to_string());
        } else if let Some(value) = lower
            .strip_prefix(TIMESTAMP_HEADER)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            timestamp = Some(value.trim().to_string());
        }
    }
    let body = if chunked {
        read_chunked_body(reader).await?
    } else {
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).await?;
        body
    };
    Ok(Some((body, signature, timestamp)))
}

async fn read_chunked_body<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            bail!("connection closed mid-chunk-size");
        }
        let size = line.trim_end().split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| anyhow::anyhow!("bad chunk size {size:?}"))?;
        if size == 0 {
            loop {
                line.clear();
                let count = reader.read_line(&mut line).await?;
                if count == 0 || line.trim_end().is_empty() {
                    break;
                }
            }
            break;
        }
        let mut chunk = vec![0; size];
        reader.read_exact(&mut chunk).await?;
        body.extend_from_slice(&chunk);
        let mut delimiter = [0; 2];
        reader.read_exact(&mut delimiter).await?;
    }
    Ok(body)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn main() -> anyhow::Result<()> {
    wamn_runtime::advertise_memory_ceiling();
    async_main()
}

#[tokio::main]
async fn async_main() -> anyhow::Result<()> {
    let args = Args::parse();
    let level = tracing::Level::from_str(&args.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", args.log_level))?;
    let shutdown_observability =
        wash_runtime::observability::initialize_observability(level, false, false)?;
    wash_runtime::init_crypto();

    let wasm = std::fs::read(&args.node)
        .with_context(|| format!("read node component {}", args.node.display()))?;
    let credentials = Arc::new(match args.credentials_file.as_deref() {
        Some(path) => FileCredentials::from_file(path)?,
        None => FileCredentials::empty(),
    });
    let auth = RequestAuth::new(
        &credentials,
        &args.project,
        args.require_signing_key,
        args.signature_max_age_secs,
    );
    let egress = AllowedHostsEgress::parse(&args.allowed_hosts).context("parse --allowed-hosts")?;

    let engine = wamn_runtime::build_engine(&[])?;
    let ticker = wamn_runtime::spawn_epoch_ticker(&engine, wamn_runtime::DEFAULT_EPOCH_TICK);
    let runtime = NodeRuntime::instantiate(
        &engine,
        &wasm,
        NodeRuntimeConfig {
            component_id: DEFAULT_NODE_ID.to_string(),
            project: args.project.clone(),
            credentials,
            egress: Arc::new(egress),
        },
    )
    .await?;
    let result = Arc::new(NodeHost { runtime, auth }).serve(args.port).await;
    ticker.abort();
    shutdown_observability();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_node_invoke::sign_envelope_with_timestamp;

    #[test]
    fn keyed_auth_accepts_current_and_previous_and_rejects_wrong() {
        let credentials = FileCredentials {
            has_source: true,
            projects: HashMap::from([(
                "project".to_string(),
                HashMap::from([
                    (SIGNING_KEY_CREDENTIAL.to_string(), "current".to_string()),
                    (
                        SIGNING_KEY_CREDENTIAL_PREVIOUS.to_string(),
                        "previous".to_string(),
                    ),
                ]),
            )]),
        };
        let auth = RequestAuth::new(&credentials, "project", true, None);
        let body = b"{}";
        assert!(
            auth.verify(
                body,
                Some(&sign_envelope_with_timestamp(b"current", body, None)),
                None,
                0
            )
            .is_ok()
        );
        assert!(
            auth.verify(
                body,
                Some(&sign_envelope_with_timestamp(b"previous", body, None)),
                None,
                0
            )
            .is_ok()
        );
        assert_eq!(
            auth.verify(
                body,
                Some(&sign_envelope_with_timestamp(b"wrong", body, None)),
                None,
                0
            ),
            Err(SignatureError::Mismatch)
        );
    }
}
