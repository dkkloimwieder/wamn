//! Native probe transport for the router driver's readiness authority.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use wash_runtime::host::probes::{self, ProbeState, ReadinessCheck};

use wamn_execution_host::RouterReadinessProbe;

pub(crate) const DEFAULT_BIND: &str = "0.0.0.0:8089";
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct ReleaseClosure(Arc<RouterReadinessProbe>);

impl ReadinessCheck for ReleaseClosure {
    fn name(&self) -> &'static str {
        "release_closure"
    }

    fn ready(&self) -> bool {
        self.0.snapshot().is_ready()
    }
}

pub(crate) async fn bind(address: SocketAddr) -> anyhow::Result<TcpListener> {
    probes::bind(address).await.context("bind executor probes")
}

/// Keep release evaluation under its existing owner and supervise both tasks.
pub(crate) async fn serve(
    listener: TcpListener,
    probe: Arc<RouterReadinessProbe>,
    state: ProbeState,
) -> anyhow::Result<()> {
    state.register(Arc::new(ReleaseClosure(Arc::clone(&probe))));
    let mut tasks = JoinSet::new();
    tasks.spawn(refresh(probe));
    tasks.spawn(probes::serve(listener, state, std::future::pending()));
    // Dropping this owner aborts both tasks, including on a service failure.
    match tasks.join_next().await {
        Some(Ok(())) => anyhow::bail!("executor probe listener or readiness refresh stopped"),
        Some(Err(error)) => Err(error).context("executor probe listener or readiness refresh task"),
        None => anyhow::bail!("executor probe tasks disappeared"),
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;

    use super::*;

    #[derive(Debug)]
    struct Available(AtomicBool);

    impl ReadinessCheck for Available {
        fn name(&self) -> &'static str {
            "release_closure"
        }
        fn ready(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }

    async fn request(address: SocketAddr, method: &str, path: &str) -> String {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream
                .write_all(
                    format!(
                        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        })
        .await
        .expect("native probe answers within its test budget")
    }

    #[tokio::test]
    async fn native_probes_preserve_readiness_authority_and_report_starting_and_draining() {
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = ProbeState::default();
        let available = Arc::new(Available(AtomicBool::new(false)));
        state.register(available.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(probes::serve(listener, state.clone(), async {
            let _ = stopped.await;
        }));
        let response = request(address, "GET", "/readyz").await;
        assert!(response.starts_with("HTTP/1.1 503"));
        assert!(response.contains("starting"));
        state.started();
        let response = request(address, "GET", "/readyz").await;
        assert!(response.starts_with("HTTP/1.1 503"));
        assert!(response.contains("release_closure"));
        assert!(
            request(address, "GET", "/livez")
                .await
                .starts_with("HTTP/1.1 200")
        );
        available.0.store(true, Ordering::Relaxed);
        assert!(
            request(address, "GET", "/readyz")
                .await
                .starts_with("HTTP/1.1 200")
        );
        // The native contract routes by exact path, independently of method.
        assert!(
            request(address, "POST", "/readyz")
                .await
                .starts_with("HTTP/1.1 200")
        );
        assert!(
            request(address, "GET", "/ready")
                .await
                .starts_with("HTTP/1.1 404")
        );
        state.drain();
        let response = request(address, "GET", "/readyz").await;
        assert!(response.starts_with("HTTP/1.1 503"));
        assert!(response.contains("draining"));
        assert!(
            request(address, "GET", "/livez")
                .await
                .starts_with("HTTP/1.1 200")
        );
        stop.send(()).unwrap();
        serving.await.unwrap();
    }
}
