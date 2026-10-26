//! Workload identity policy around the runtime's default OCI start.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use wash_runtime::host::{Host, HostApi as _};
use wash_runtime::washlet::{HostCommandHandler, HostControlDefaults, types::v2};

use wamn_execution_host::OperationHost;
use wamn_runtime::plugins::WamnPostgres;

pub(super) struct Commands {
    pub postgres: Arc<WamnPostgres>,
    pub operations: Option<Arc<OperationHost>>,
    /// The base the host resolves members and the composed entry against.
    pub component_artifact_base: Option<String>,
    pub drain: Arc<ServiceDrain>,
}

#[derive(Clone, Default)]
struct DrainState {
    draining: bool,
    services: BTreeSet<String>,
}

/// Keep control alive until the operator has stopped the service workloads.
/// Native ingress shutdown also shuts down the attached control loop.
#[derive(Default)]
pub(super) struct ServiceDrain {
    state: tokio::sync::watch::Sender<DrainState>,
}

impl ServiceDrain {
    fn admit(&self, workload: &str, service: bool) -> anyhow::Result<()> {
        let mut admitted = false;
        self.state.send_modify(|state| {
            if !state.draining {
                admitted = true;
                if service {
                    state.services.insert(workload.to_owned());
                }
            }
        });
        anyhow::ensure!(admitted, "host is draining");
        Ok(())
    }

    fn stopped(&self, workload: &str) {
        self.state.send_modify(|state| {
            state.services.remove(workload);
        });
    }

    pub async fn stop(
        &self,
        host: &Host,
        local_materializer: Option<&str>,
        bound: Duration,
    ) -> anyhow::Result<()> {
        self.state.send_modify(|state| state.draining = true);
        let services = self.state.borrow().services.clone();
        let mut state = self.state.subscribe();
        tokio::time::timeout(bound, async {
            // Workload stop aborts the service supervisor, including its restart
            // loop. Host::stop alone stops plugins, not running service tasks.
            for workload_id in services
                .iter()
                .map(String::as_str)
                .chain(local_materializer)
            {
                host.workload_stop(wash_runtime::types::WorkloadStopRequest {
                    workload_id: workload_id.to_owned(),
                })
                .await?;
            }
            // A local stop does not release the operator's finalizer. Keep
            // control alive for the operator's own stop, within the same bound,
            // so the queue drain keeps the rest of the host's deadline.
            state.wait_for(|state| state.services.is_empty()).await?;
            anyhow::Ok(())
        })
        .await
        .context("the materializer did not stop within its drain bound")?
    }
}

struct PendingStart<'a> {
    postgres: &'a WamnPostgres,
    workload: &'a str,
}

impl Drop for PendingStart<'_> {
    fn drop(&mut self) {
        self.postgres.forget_operation_start(self.workload);
    }
}

#[async_trait::async_trait]
impl HostCommandHandler for Commands {
    async fn start(
        &self,
        defaults: &HostControlDefaults,
        request: v2::WorkloadStartRequest,
    ) -> anyhow::Result<v2::WorkloadStartResponse> {
        let workload = request.workload.as_ref().context("start has no workload")?;
        let components = workload
            .wit_world
            .as_ref()
            .map(|world| &world.components[..])
            .unwrap_or_default();
        for component in components {
            wamn_catalog::validate_component_start_reference(&component.image)?;
        }
        let workload_id = request.workload_id.clone();
        self.drain.admit(&workload_id, workload.service.is_some())?;
        let mut draining = self.drain.state.subscribe();
        let mut pending = None;
        if let Some(operations) = &self.operations
            && let Some(entry) = &operations.release.manifest().composed_entry
        {
            let reference = wamn_catalog::composed_entry_start_reference(
                self.component_artifact_base.as_deref(),
                &entry.reference,
            )?;
            let mut entries = components
                .iter()
                .filter(|component| component.image == reference);
            if let Some(component) = entries.next() {
                anyhow::ensure!(
                    entries.next().is_none(),
                    "workload repeats its composed entry"
                );
                anyhow::ensure!(
                    component.pool_size == 0,
                    "composed entry requires poolSize 0"
                );
                self.postgres.bind_operation_workload(
                    &workload_id,
                    Arc::clone(&operations.release),
                    &component.image,
                    &component.name,
                    operations.claims(
                        &operations.release.scope().tenant_id,
                        Some(operations.release_identity()),
                    ),
                )?;
                pending = Some(PendingStart {
                    postgres: &self.postgres,
                    workload: &workload_id,
                });
            }
        }
        // The runtime still owns reservations, pulls, verification, compilation and stop races.
        let result = tokio::select! {
            biased;
            _ = draining.wait_for(|state| state.draining) => {
                Err(anyhow::anyhow!("host is draining"))
            }
            result = defaults.start(request) => result,
        };
        drop(pending);
        result
    }

    async fn stop(
        &self,
        defaults: &HostControlDefaults,
        request: v2::WorkloadStopRequest,
    ) -> anyhow::Result<v2::WorkloadStopResponse> {
        let workload = request.workload_id.clone();
        let result = defaults.stop(request).await?;
        self.drain.stopped(&workload);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn drain_waits_for_operator_stop_and_refuses_restarts() {
        use wash_runtime::types::{
            Workload, WorkloadStartRequest, WorkloadState, WorkloadStatusRequest,
        };

        let host = Host::builder().build().unwrap();
        let started = host
            .workload_start(WorkloadStartRequest {
                workload_id: "materializer".into(),
                workload: Workload {
                    namespace: "test".into(),
                    name: "materializer".into(),
                    annotations: std::collections::HashMap::new(),
                    service: None,
                    components: Vec::new(),
                    host_interfaces: Vec::new(),
                    volumes: Vec::new(),
                },
            })
            .await
            .unwrap();
        assert_eq!(
            started.workload_status.workload_state,
            WorkloadState::Running
        );
        let drain = ServiceDrain::default();
        drain.admit("materializer", true).unwrap();
        drain.admit("http", false).unwrap();
        let stopping = drain.stop(&host, None, Duration::from_secs(5));
        tokio::pin!(stopping);
        tokio::select! {
            biased;
            result = &mut stopping => panic!("operator has not stopped the workload: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        let status = host
            .workload_status(WorkloadStatusRequest {
                workload_id: "materializer".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            status.workload_status.workload_state,
            WorkloadState::NotFound
        );
        assert_eq!(
            drain.admit("materializer", true).unwrap_err().to_string(),
            "host is draining"
        );
        assert!(drain.admit("new-http", false).is_err());
        // Local teardown completed, but a reply to the operator is still due.
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::select! {
            biased;
            result = &mut stopping => panic!("local stop must not stand in for an operator reply: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        drain.stopped("materializer");
        stopping.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn drain_ends_at_its_bound_when_operator_stop_never_arrives() {
        let host = Host::builder().build().unwrap();
        let drain = ServiceDrain::default();
        drain.admit("materializer", true).unwrap();
        let started = tokio::time::Instant::now();
        let error = drain
            .stop(&host, None, Duration::from_secs(1))
            .await
            .expect_err("no operator stop arrived");
        assert_eq!(
            error.to_string(),
            "the materializer did not stop within its drain bound"
        );
        assert_eq!(started.elapsed(), Duration::from_secs(1));
        assert!(drain.admit("materializer", true).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn local_and_already_stopped_services_need_no_operator_reply() {
        let host = Host::builder().build().unwrap();
        let drain = ServiceDrain::default();
        drain.admit("materializer", true).unwrap();
        drain.stopped("materializer");
        let started = tokio::time::Instant::now();
        drain
            .stop(&host, Some("local-materializer"), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
}
