//! The status of a running edge: `GET /status` on a Unix socket beside the
//! run-state file (docs/plan/edge-diagnostics.md).
//!
//! The answer is one JSON object: the dropped frames of the device loop, the
//! credential failures of the forward and whether it stopped at
//! `credential_bound`, the pending sample count, and the key, capture time and
//! reason of each refused sample that no operator resolved. It carries no
//! sample body. The socket has no port and no key, so nothing off the box
//! reaches it, and it checks no credential, as the host's probe listener
//! checks none. Its own accept loop keeps it apart from the release routes,
//! which keep their one matcher.

use std::convert::Infallible;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Full};
use hyper::header::{CONTENT_TYPE, HOST};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::samples::SampleStore;

/// The file name of the socket, in the directory of the run-state file.
const SOCKET_FILE: &str = "status.sock";
/// The one path the socket answers.
const STATUS_PATH: &str = "/status";
/// How long one status connection lives. Each serves one request.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
/// The wait after a failed accept, so a lack of descriptors does not spin.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

/// The status socket of the edge whose run-state file is `db`.
pub fn socket_path(db: &Path) -> anyhow::Result<PathBuf> {
    let db = std::path::absolute(db).context("resolve the run-state file path")?;
    Ok(db
        .parent()
        .context("the run-state file has no directory")?
        .join(SOCKET_FILE))
}

/// What the status reads, shared with the device loop and the forward.
#[derive(Debug, Clone)]
pub(crate) struct Facts {
    pub(crate) started_at: String,
    pub(crate) dropped_frames: Option<Arc<AtomicU64>>,
    pub(crate) forward: Option<(Arc<AtomicU64>, Arc<AtomicBool>)>,
    pub(crate) samples: SampleStore,
}

impl Facts {
    /// The status answer.
    async fn read(&self) -> anyhow::Result<Value> {
        let pending = self.samples.pending_count().await?;
        let refused: Vec<Value> = self
            .samples
            .refused_samples()
            .await?
            .into_iter()
            .map(|sample| {
                json!({
                    "sample_key": sample.sample_key,
                    "captured_at": sample.captured_at,
                    "reason": sample.reason,
                })
            })
            .collect();
        Ok(json!({
            "started_at": self.started_at,
            "device": self.dropped_frames.as_ref().map(|dropped| {
                json!({"dropped_frames": dropped.load(Ordering::Relaxed)})
            }),
            "forward": self.forward.as_ref().map(|(failures, stopped)| {
                json!({
                    "credential_failures": failures.load(Ordering::Relaxed),
                    "stopped": stopped.load(Ordering::Relaxed),
                })
            }),
            "samples": {"pending": pending, "refused": refused},
        }))
    }
}

/// Bind the socket at `path`, before the host takes work, so a failure fails
/// the start. The caller holds the run-state file, so a socket file left
/// there belongs to a stopped edge, and is removed.
pub(crate) fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("remove the old socket {}", path.display()));
        }
    }
    UnixListener::bind(path).with_context(|| format!("bind the status socket {}", path.display()))
}

/// Serve `listener` until `stopped` turns true.
pub(crate) fn start(
    listener: UnixListener,
    facts: Facts,
    mut stopped: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = stopped.wait_for(|stopped| *stopped) => return,
            };
            let client = match accepted {
                Ok((client, _)) => client,
                Err(error) => {
                    tracing::warn!(%error, "the status socket cannot accept a connection");
                    tokio::time::sleep(ACCEPT_PAUSE).await;
                    continue;
                }
            };
            let facts = facts.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let facts = facts.clone();
                    async move { Ok::<_, Infallible>(answer(&facts, &request).await) }
                });
                let served = tokio::time::timeout(
                    CONNECTION_TIMEOUT,
                    hyper::server::conn::http1::Builder::new()
                        .keep_alive(false)
                        .serve_connection(TokioIo::new(client), service),
                );
                if let Ok(Err(error)) = served.await {
                    tracing::debug!(%error, "a status connection failed");
                }
            });
        }
    })
}

async fn answer<B>(facts: &Facts, request: &Request<B>) -> Response<Full<Bytes>> {
    if request.method() != Method::GET || request.uri().path() != STATUS_PATH {
        return text(StatusCode::NOT_FOUND, "not found\n".to_owned());
    }
    match facts.read().await {
        Ok(status) => {
            let mut response = text(StatusCode::OK, status.to_string());
            response.headers_mut().insert(
                CONTENT_TYPE,
                hyper::header::HeaderValue::from_static("application/json"),
            );
            response
        }
        Err(error) => {
            tracing::warn!(%error, "the status cannot read the samples");
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the status cannot read the samples: {error:#}\n"),
            )
        }
    }
}

fn text(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    response
}

/// Read the status of the running edge whose run-state file is `db`.
///
/// # Errors
///
/// Fails when no edge serves the socket, or when the edge does not answer
/// the status.
pub async fn read(db: &Path) -> anyhow::Result<Value> {
    let path = socket_path(db)?;
    let stream = UnixStream::connect(&path).await.with_context(|| {
        format!(
            "connect to {}; the edge serves the status only while it runs",
            path.display()
        )
    })?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .context("open the status connection")?;
    tokio::spawn(connection);
    let request = Request::get(STATUS_PATH)
        .header(HOST, "localhost")
        .body(Empty::<Bytes>::new())
        .context("build the status request")?;
    let response = sender
        .send_request(request)
        .await
        .context("send the status request")?;
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .context("read the status answer")?
        .to_bytes();
    anyhow::ensure!(
        status == StatusCode::OK,
        "the edge answered {status}: {}",
        String::from_utf8_lossy(&body).trim_end()
    );
    serde_json::from_slice(&body).context("parse the status answer")
}

/// `wamn-edge status`: the status of the running edge whose run-state file
/// is `db`, as text. Each line is a name and its value, separated by a tab,
/// and each refused sample is one line of its key, capture time and reason,
/// as `wamn-edge samples list` prints them.
///
/// # Errors
///
/// Fails as [`read`] fails.
pub async fn run(db: &Path) -> anyhow::Result<String> {
    Ok(text_of(&read(db).await?)?)
}

/// The text of one status answer.
fn text_of(status: &Value) -> Result<String, std::fmt::Error> {
    let mut output = String::new();
    writeln!(output, "started_at\t{}", plain(&status["started_at"]))?;
    for (section, fields) in [
        ("device", &["dropped_frames"][..]),
        ("forward", &["credential_failures", "stopped"][..]),
    ] {
        if status[section].is_null() {
            writeln!(output, "{section}\tnone")?;
            continue;
        }
        for field in fields {
            writeln!(
                output,
                "{section}.{field}\t{}",
                plain(&status[section][field])
            )?;
        }
    }
    writeln!(
        output,
        "samples.pending\t{}",
        plain(&status["samples"]["pending"])
    )?;
    let refused = status["samples"]["refused"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    writeln!(output, "samples.refused\t{}", refused.len())?;
    for sample in refused {
        writeln!(
            output,
            "refused\t{}\t{}\t{}",
            plain(&sample["sample_key"]),
            plain(&sample["captured_at"]),
            plain(&sample["reason"])
        )?;
    }
    Ok(output)
}

/// A JSON value as text: a string without its quotes.
fn plain(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}
