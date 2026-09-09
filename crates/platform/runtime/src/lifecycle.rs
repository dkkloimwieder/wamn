//! Bounded service shutdown and supervision of native runtime liveness.

use std::time::Duration;

use anyhow::Context as _;
use wash_runtime::host::probes::{Liveness, ProbeState};

/// Bound the runtime's final wait for blocking exporters and guest compilation.
/// A timed-out `flush_within` leaves its blocking task running until this wait.
pub const RUNTIME_SHUTDOWN_BUDGET: Duration = Duration::from_secs(1);

/// Finish telemetry without hiding a service failure or an incomplete flush.
pub async fn finish(result: anyhow::Result<()>) -> anyhow::Result<()> {
    let flushed =
        wash_runtime::observability::flush_within(wash_runtime::observability::FLUSH_BUDGET).await;
    if !flushed {
        tracing::error!("telemetry flush failed or exceeded its shutdown budget");
        return result.and(Err(anyhow::anyhow!("telemetry flush did not complete")));
    }
    result
}

/// Await cleanup for at most `budget`, retaining its failure context.
pub async fn bounded_cleanup(
    budget: Duration,
    cleanup: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    tokio::time::timeout(budget, cleanup)
        .await
        .context("service cleanup exceeded its shutdown budget")?
}

/// Supervise real loop progress and announce startup only after the first beat.
///
/// Native liveness intentionally permits an unstarted host indefinitely. Once
/// native startup has returned, a command task that never subscribes must still
/// fail within its normal silence budget. This observer never writes a beat.
pub async fn watch_liveness(
    liveness: &Liveness,
    probes: &ProbeState,
    silence_budget: Duration,
) -> anyhow::Error {
    let started = tokio::time::Instant::now();
    let mut check = tokio::time::interval(Duration::from_millis(100));
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        check.tick().await;
        match liveness_progress(liveness.silence(), started.elapsed(), silence_budget) {
            Ok(true) => probes.started(),
            Ok(false) => {}
            Err(error) => return error,
        }
    }
}

fn liveness_progress(
    silence: Option<Duration>,
    startup_elapsed: Duration,
    silence_budget: Duration,
) -> anyhow::Result<bool> {
    match silence {
        None if startup_elapsed >= silence_budget => {
            anyhow::bail!("service loop did not produce its first liveness beat")
        }
        None => Ok(false),
        Some(silence) if silence > silence_budget => {
            anyhow::bail!("service loop exceeded its native liveness silence budget")
        }
        Some(_) => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_requires_a_beat_and_stale_detection_preserves_the_native_boundary() {
        let budget = Duration::from_secs(45);
        assert!(
            !liveness_progress(
                None,
                budget.checked_sub(Duration::from_nanos(1)).unwrap(),
                budget
            )
            .unwrap()
        );
        assert!(liveness_progress(None, budget, budget).is_err());
        assert!(liveness_progress(Some(budget), Duration::MAX, budget).unwrap());
        assert!(
            liveness_progress(
                Some(budget + Duration::from_nanos(1)),
                Duration::ZERO,
                budget
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn missing_first_beat_fails_without_creating_a_beat() {
        let liveness = Liveness::new(Duration::ZERO);
        let error = watch_liveness(&liveness, &ProbeState::default(), Duration::ZERO).await;
        assert!(error.to_string().contains("first liveness beat"));
        assert_eq!(liveness.silence(), None);
    }

    #[tokio::test]
    async fn native_probes_answer_while_ingress_is_saturated_and_after_it_recovers() {
        use std::net::SocketAddr;
        use std::sync::Arc;

        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use wash_runtime::host::http::{DynamicRouter, HostHandler as _, Ingress};
        use wash_runtime::host::probes::{self, ReadinessCheck as _};

        async fn request(address: SocketAddr, path: &str) -> String {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        }

        tokio::time::timeout(Duration::from_secs(5), async {
            let ingress =
                Ingress::builder(DynamicRouter::default(), "127.0.0.1:0".parse().unwrap())
                    .max_connections(1)
                    .build()
                    .await
                    .unwrap();
            let connections = ingress.connection_limit();
            let state = ProbeState::default().with_readiness(Arc::new(connections.clone()));
            let listener = probes::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let mut tasks = tokio::task::JoinSet::new();
            tasks.spawn(probes::serve(listener, state.clone(), async {
                let _ = stopped.await;
            }));
            ingress.start().await.unwrap();
            state.started();
            assert!(
                request(address, "/readyz")
                    .await
                    .starts_with("HTTP/1.1 200")
            );

            // An unfinished HTTP request holds the sole native ingress permit.
            let held = tokio::net::TcpStream::connect(ingress.addr())
                .await
                .unwrap();
            while connections.ready() {
                tokio::task::yield_now().await;
            }
            let response = request(address, "/readyz").await;
            assert!(response.starts_with("HTTP/1.1 503"), "{response}");
            assert!(response.contains("http_ingress_saturated"), "{response}");
            assert!(request(address, "/livez").await.starts_with("HTTP/1.1 200"));

            drop(held);
            while !connections.ready() {
                tokio::task::yield_now().await;
            }
            assert!(
                request(address, "/readyz")
                    .await
                    .starts_with("HTTP/1.1 200")
            );
            state.drain();
            let response = request(address, "/readyz").await;
            assert!(response.starts_with("HTTP/1.1 503"), "{response}");
            assert!(response.contains("draining"), "{response}");
            assert!(request(address, "/livez").await.starts_with("HTTP/1.1 200"));
            ingress.stop().await.unwrap();
            stop.send(()).unwrap();
            tasks.join_next().await.unwrap().unwrap();
        })
        .await
        .expect("native probes remain responsive through saturation, recovery, and drain");
    }

    #[tokio::test]
    async fn cleanup_timeout_and_original_failure_remain_failures() {
        let error = bounded_cleanup(Duration::ZERO, std::future::pending())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("shutdown budget"));
        let error = bounded_cleanup(Duration::from_secs(1), async {
            anyhow::bail!("native command task failed")
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "native command task failed");
    }
}
