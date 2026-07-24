//! Gates-only HTTP/auth adapter around [`wamn_node_runtime::NodeRuntime`].
//!
//! The gate suite keeps its in-process server seam, but component compilation,
//! linking, config memoization, invocation serialization, and credential-grant
//! lifecycle all belong to `wamn-node-runtime`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::check_allowed_hosts;

use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, SIGNATURE_HEADER, SIGNING_KEY_CREDENTIAL,
    SIGNING_KEY_CREDENTIAL_PREVIOUS, SignatureError, TIMESTAMP_HEADER, WireErrorDetail,
    WireNodeError, timestamp_fresh, verify_envelope_with_timestamp,
};
use wamn_node_runtime::{
    CredentialLookupError, CredentialProvider, EgressPolicy, EgressRequest, NodeRuntime,
    NodeRuntimeConfig,
};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;

pub const DEFAULT_NODE_ID: &str = wamn_node_runtime::DEFAULT_NODE_ID;

#[derive(Debug, Args)]
pub struct ServeNodeArgs {
    #[arg(long)]
    pub node: PathBuf,
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
    #[arg(long, env = "WAMN_PROJECT", default_value = "default")]
    pub project: String,
    #[arg(long, env = "WAMN_CREDENTIALS_FILE")]
    pub credentials_file: Option<PathBuf>,
    #[arg(
        long = "allowed-hosts",
        env = "WAMN_ALLOWED_HOSTS",
        value_delimiter = ','
    )]
    pub allowed_hosts: Vec<String>,
    #[arg(long = "require-signing-key", env = "WAMN_REQUIRE_SIGNING_KEY")]
    pub require_signing_key: bool,
    #[arg(long = "signature-max-age-secs", env = "WAMN_SIGNATURE_MAX_AGE_SECS")]
    pub signature_max_age_secs: Option<u64>,
}

pub struct ServeNodeAuthn {
    pub require_signing_key: bool,
    pub max_signature_age_secs: Option<u64>,
}

struct GateCredentials {
    vault: Arc<WamnCredentials>,
}

impl CredentialProvider for GateCredentials {
    fn get(&self, project: &str, name: &str) -> Result<String, CredentialLookupError> {
        self.vault
            .lookup(project, name)
            .ok_or(CredentialLookupError::NotFound)
    }
}

struct GateEgress {
    allowed_hosts: Arc<[AllowedHost]>,
}

impl EgressPolicy for GateEgress {
    fn allows(&self, request: &EgressRequest) -> bool {
        let Ok(request) = hyper::Request::builder()
            .method(request.method.as_str())
            .uri(request.uri.as_str())
            .body(())
        else {
            return false;
        };
        check_allowed_hosts(&request, &self.allowed_hosts).is_ok()
    }
}

/// Gate-facing wrapper that retains the historical auth and HTTP surface.
pub struct ServeNode {
    runtime: NodeRuntime,
    signing_key: Option<Vec<u8>>,
    previous_signing_key: Option<Vec<u8>>,
    require_signing_key: bool,
    max_signature_age_secs: Option<u64>,
}

impl ServeNode {
    pub async fn new(
        engine: &Engine,
        wasm: &[u8],
        vault: Arc<WamnCredentials>,
        node_id: &str,
        project: &str,
        allowed_hosts: Arc<[AllowedHost]>,
        authn: ServeNodeAuthn,
    ) -> anyhow::Result<Self> {
        let signing_key = vault
            .lookup(project, SIGNING_KEY_CREDENTIAL)
            .map(String::into_bytes);
        let previous_signing_key = vault
            .lookup(project, SIGNING_KEY_CREDENTIAL_PREVIOUS)
            .map(String::into_bytes);
        let runtime = NodeRuntime::instantiate(
            engine,
            wasm,
            NodeRuntimeConfig {
                component_id: node_id.to_string(),
                project: project.to_string(),
                credentials: Arc::new(GateCredentials { vault }),
                egress: Arc::new(GateEgress { allowed_hosts }),
            },
        )
        .await?;

        Ok(Self {
            runtime,
            signing_key,
            previous_signing_key,
            require_signing_key: authn.require_signing_key,
            max_signature_age_secs: authn.max_signature_age_secs,
        })
    }

    pub fn verify_signature(
        &self,
        body: &[u8],
        signature: Option<&str>,
        timestamp: Option<&str>,
        now_secs: u64,
    ) -> Result<(), SignatureError> {
        let Some(key) = self.signing_key.as_deref() else {
            return if self.require_signing_key {
                Err(SignatureError::Unconfigured)
            } else {
                Ok(())
            };
        };
        let signature = signature.ok_or(SignatureError::Missing)?;
        if let Err(error) = verify_envelope_with_timestamp(key, body, timestamp, signature) {
            match self.previous_signing_key.as_deref() {
                Some(previous) => {
                    verify_envelope_with_timestamp(previous, body, timestamp, signature)?
                }
                None => return Err(error),
            }
        }
        if let Some(max_age) = self.max_signature_age_secs {
            let timestamp = timestamp.ok_or(SignatureError::MissingTimestamp)?;
            let timestamp = timestamp
                .trim()
                .parse::<u64>()
                .map_err(|_| SignatureError::MalformedTimestamp)?;
            if !timestamp_fresh(timestamp, now_secs, max_age) {
                return Err(SignatureError::Stale);
            }
        }
        Ok(())
    }

    pub async fn invoke(&self, request: NodeInvokeRequest) -> NodeInvokeResponse {
        self.runtime.invoke(request).await
    }

    pub fn grant_install_count(&self) -> u64 {
        self.runtime.grant_install_count()
    }

    pub fn invocation_grant_active(&self) -> bool {
        self.runtime.invocation_grant_active()
    }

    pub async fn config_parse_count(&self) -> u64 {
        self.runtime.config_parse_count().await
    }
}

pub async fn serve(node: Arc<ServeNode>, port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    loop {
        let (socket, peer) = listener.accept().await?;
        if let Err(error) = serve_connection(socket, &node).await {
            tracing::warn!(%peer, %error, "serve-node gate adapter connection error");
        }
    }
}

async fn serve_connection(socket: TcpStream, node: &ServeNode) -> anyhow::Result<()> {
    socket.set_nodelay(true)?;
    let mut reader = BufReader::new(socket);
    loop {
        let Some((body, signature, timestamp)) = read_http_request_body(&mut reader).await? else {
            break;
        };
        if let Err(error) = node.verify_signature(
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
            Ok(request) => node.invoke(request).await,
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

async fn read_http_request_body<R: tokio::io::AsyncBufRead + Unpin>(
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
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return if saw_any {
                bail!("connection closed mid-headers")
            } else {
                Ok(None)
            };
        }
        saw_any = true;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let header = header.to_ascii_lowercase();
        if let Some(value) = header.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = header.strip_prefix("transfer-encoding:") {
            chunked = value.contains("chunked");
        } else if let Some(value) = header
            .strip_prefix(SIGNATURE_HEADER)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            signature = Some(value.trim().to_string());
        } else if let Some(value) = header
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
                let read = reader.read_line(&mut line).await?;
                if read == 0 || line.trim_end().is_empty() {
                    break;
                }
            }
            break;
        }
        let mut chunk = vec![0; size];
        reader.read_exact(&mut chunk).await?;
        body.extend_from_slice(&chunk);
        let mut crlf = [0; 2];
        reader.read_exact(&mut crlf).await?;
    }
    Ok(body)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub async fn run(args: ServeNodeArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    let wasm = std::fs::read(&args.node)
        .with_context(|| format!("read node component {}", args.node.display()))?;
    let vault = Arc::new(match &args.credentials_file {
        Some(path) => WamnCredentials::from_file(path)?,
        None => WamnCredentials::empty(),
    });
    let allowed_hosts = args
        .allowed_hosts
        .iter()
        .map(|host| host.parse::<AllowedHost>())
        .collect::<Result<Vec<_>, _>>()
        .context("parse --allowed-hosts")?
        .into();
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let node = Arc::new(
        ServeNode::new(
            &engine,
            &wasm,
            vault,
            DEFAULT_NODE_ID,
            &args.project,
            allowed_hosts,
            ServeNodeAuthn {
                require_signing_key: args.require_signing_key,
                max_signature_age_secs: args.signature_max_age_secs,
            },
        )
        .await?,
    );
    let result = serve(node, args.port).await;
    ticker.abort();
    result
}
