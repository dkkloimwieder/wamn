//! HTTP transport for the router driver's probe-owned readiness state.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use wamn_execution_host::RouterReadinessProbe;

pub(crate) const DEFAULT_BIND: &str = "0.0.0.0:8089";

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn bind(address: SocketAddr) -> anyhow::Result<TcpListener> {
    TcpListener::bind(address)
        .await
        .with_context(|| format!("bind executor readiness endpoint on {address}"))
}

/// Serve one status-only endpoint while the existing probe owns evaluation.
///
/// The refresh task stores no state: it only supplies a bounded retry cadence
/// to [`RouterReadinessProbe`]. The listener handles one non-keepalive request
/// at a time and bounds an incomplete connection, so readiness cannot grow an
/// unbounded task or request queue inside the process.
pub(crate) async fn serve(
    listener: TcpListener,
    probe: Arc<RouterReadinessProbe>,
) -> anyhow::Result<()> {
    let address = listener
        .local_addr()
        .context("read executor readiness listener address")?;
    tracing::info!(%address, "executor readiness endpoint listening");

    let mut tasks = JoinSet::new();
    tasks.spawn(refresh(Arc::clone(&probe)));
    loop {
        tokio::select! {
            refresh = tasks.join_next() => match refresh {
                Some(Ok(())) => anyhow::bail!("executor readiness refresh loop stopped"),
                Some(Err(error)) => return Err(error).context("executor readiness refresh task"),
                None => anyhow::bail!("executor readiness refresh task disappeared"),
            },
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept executor readiness connection")?;
                let probe = Arc::clone(&probe);
                let service = service_fn(move |request| route(Arc::clone(&probe), request));
                let connection = hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(stream), service);
                match tokio::time::timeout(CONNECTION_TIMEOUT, connection).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::debug!(%error, "executor readiness connection ended");
                    }
                    Err(_) => {
                        tracing::debug!("executor readiness connection timed out");
                    }
                }
            }
        }
    }
}

async fn refresh(probe: Arc<RouterReadinessProbe>) {
    loop {
        let snapshot = probe.refresh().await;
        if snapshot.is_ready() {
            tracing::debug!(
                generation = snapshot.generation,
                synchronous_wirings = snapshot.synchronous_wirings,
                component_digests = snapshot.component_digests,
                "executor release closure is ready"
            );
        } else {
            tracing::warn!(
                generation = snapshot.generation,
                attempts = snapshot.attempts,
                refusal = snapshot.refusal.unwrap_or("release-readiness-unavailable"),
                "executor release closure is not ready"
            );
        }
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

async fn route(
    probe: Arc<RouterReadinessProbe>,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let status = route_status(
        request.method(),
        request.uri().path(),
        probe.snapshot().is_ready(),
    );
    Ok(Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .expect("static executor readiness response is valid"))
}

fn route_status(method: &Method, path: &str, ready: bool) -> StatusCode {
    match (method, path, ready) {
        (&Method::GET, "/readyz", true) => StatusCode::OK,
        (&Method::GET, "/readyz", false) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::NOT_FOUND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_get_endpoint_exposes_binary_readiness() {
        assert_eq!(route_status(&Method::GET, "/readyz", false), 503);
        assert_eq!(route_status(&Method::GET, "/readyz", true), 200);
        assert_eq!(route_status(&Method::POST, "/readyz", true), 404);
        assert_eq!(route_status(&Method::GET, "/ready", true), 404);
    }
}
