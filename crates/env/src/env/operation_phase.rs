//! Coordinate the project Job and the env Job that owns its lifecycle lock.

use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_kubernetes::{client::KubernetesApi, job};
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::Triple;

use super::{
    Platform, capture_gap,
    database::{DatabaseConnection, LifecycleLock},
    ensure as host,
    worker::Operation,
};

const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    SuspendHost,
    Publication,
    WaitReader,
    ResumeHost,
    PauseCapture,
    SyncCapture,
}

impl Step {
    fn key(self) -> &'static str {
        match self {
            Self::SuspendHost => "suspend-host",
            Self::Publication => "publication",
            Self::WaitReader => "wait-reader",
            Self::ResumeHost => "resume-host",
            Self::PauseCapture => "pause-capture",
            Self::SyncCapture => "sync-capture",
        }
    }
}

pub(super) fn steps(operation: Operation) -> &'static [Step] {
    use Step::{PauseCapture, Publication, ResumeHost, SuspendHost, SyncCapture, WaitReader};
    match operation {
        Operation::Deploy | Operation::Apply => &[Publication, WaitReader],
        Operation::Reset => &[SuspendHost, Publication, WaitReader, ResumeHost],
        Operation::Recover => &[PauseCapture],
        Operation::Close => &[SyncCapture],
        Operation::Rollback | Operation::Stop | Operation::Diff => &[],
    }
}

pub(super) fn config_map_name(coordinate: &str) -> String {
    format!("{coordinate}-operation-phase")
}

pub(super) struct Phase<'a> {
    api: &'a KubernetesApi,
    map: String,
    project_job: &'a str,
}

impl<'a> Phase<'a> {
    pub(super) fn new(api: &'a KubernetesApi, coordinate: &str, project_job: &'a str) -> Self {
        Self {
            api,
            map: config_map_name(coordinate),
            project_job,
        }
    }

    pub(super) async fn join<T>(
        &self,
        env_job: &str,
        work: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let run = async {
            loop {
                let data = self.api.config_map_data(&self.map).await?;
                if data.get("job").is_some_and(|name| name == self.project_job) {
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            work.await
        };
        let env_ended = async {
            while job::finished(self.api, env_job).await? == Some(false) {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            anyhow::bail!("environment phase Job {env_job} ended; see its logs");
        };
        while_peer_active(env_ended, run).await
    }

    pub(super) async fn request(&self, step: Step) -> anyhow::Result<()> {
        self.send(step, String::new()).await
    }

    pub(super) async fn publication(&self, schemas: &[CanonicalName]) -> anyhow::Result<()> {
        self.send(Step::Publication, serde_json::to_string(schemas)?)
            .await
    }

    fn key(&self, step: Step) -> String {
        format!("{}.{}", self.project_job, step.key())
    }

    async fn send(&self, step: Step, request: String) -> anyhow::Result<()> {
        let key = self.key(step);
        let reply = format!("{key}-reply");
        self.api
            .merge_config_map_data(&self.map, json!({(&key): request}))
            .await?;
        loop {
            let data = self.api.config_map_data(&self.map).await?;
            ensure!(
                data.get("job").is_some_and(|name| name == self.project_job),
                "environment phase no longer holds the lifecycle lock"
            );
            if let Some(result) = data.get(&reply) {
                ensure!(result == "ok", "environment phase {} failed", step.key());
                return Ok(());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    pub(super) async fn serve(
        &self,
        platform: &Platform,
        triple: &Triple,
        operation: Operation,
    ) -> anyhow::Result<()> {
        // Control submits the env Job before the project Job. An interrupted
        // submission must release the env worker without waiting for its deadline.
        let active = tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                match job::finished(self.api, self.project_job).await? {
                    Some(true) => return Ok::<_, anyhow::Error>(false),
                    Some(false) if job::has_input(self.api, self.project_job).await? => {
                        return Ok(true);
                    }
                    _ => {}
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .context("project operation Job was not submitted")??;
        if !active {
            return Ok(());
        }
        LifecycleLock::hold(
            DatabaseConnection::Direct(&platform.system_database_url),
            triple,
            async |system| {
                let project_ended = async {
                    while job::finished(self.api, self.project_job).await? == Some(false) {
                        tokio::time::sleep(POLL_INTERVAL).await;
                    }
                    Ok(())
                };
                while_peer_active(
                    project_ended,
                    self.serve_locked(platform, system, triple, operation),
                )
                .await
            },
        )
        .await
    }

    async fn serve_locked(
        &self,
        platform: &Platform,
        system: &crate::env_sql_effects::PostgresSession,
        triple: &Triple,
        operation: Operation,
    ) -> anyhow::Result<()> {
        let steps = steps(operation);
        // Clear old messages before signalling readiness. Each message key
        // includes its Job name so a late request cannot affect the next run.
        let mut data: serde_json::Map<_, _> = self
            .api
            .config_map_data(&self.map)
            .await?
            .into_keys()
            .map(|key| (key, Value::Null))
            .collect();
        data.insert("job".into(), self.project_job.into());
        self.api
            .merge_config_map_data(&self.map, Value::Object(data))
            .await?;

        let mut wait_for_reader = false;
        for &step in steps {
            let key = self.key(step);
            let request = loop {
                let mut data = self.api.config_map_data(&self.map).await?;
                ensure!(
                    data.get("job").is_some_and(|name| name == self.project_job),
                    "environment phase lost its operation ConfigMap"
                );
                if let Some(request) = data.remove(&key) {
                    break request;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            let result = match step {
                Step::SuspendHost => host::suspend_project(platform, system, triple, None).await,
                Step::Publication => {
                    async {
                        let schemas: Vec<CanonicalName> = serde_json::from_str(&request)?;
                        wait_for_reader = super::operation::update_publication(
                            platform, system, triple, &schemas,
                        )
                        .await?;
                        Ok(())
                    }
                    .await
                }
                Step::WaitReader if wait_for_reader => {
                    platform.cluster.wait_reader_ready(triple).await
                }
                Step::WaitReader => Ok(()),
                Step::ResumeHost => {
                    async {
                        let values = host::read_host_values(
                            platform,
                            system,
                            &triple.org,
                            &triple.env,
                            &[],
                            None,
                            None,
                        )
                        .await?;
                        host::upgrade_host(platform, &triple.org, &triple.env, &values).await
                    }
                    .await
                }
                Step::PauseCapture => {
                    async {
                        capture_gap::set_paused(platform, triple, true).await?;
                        platform.cluster.wait_reader_stopped(triple).await
                    }
                    .await
                }
                Step::SyncCapture => {
                    async {
                        let paused = system.has_capture_gap(triple).await?;
                        capture_gap::set_paused(platform, triple, paused).await
                    }
                    .await
                }
            };
            let reply = if result.is_ok() { "ok" } else { "failed" };
            self.api
                .merge_config_map_data(&self.map, json!({(format!("{key}-reply")): reply}))
                .await?;
            result.with_context(|| format!("apply environment phase {}", step.key()))?;
        }
        // Keep the lock through the remaining project work, including the app rollout.
        std::future::pending().await
    }
}

async fn while_peer_active<T>(
    peer_ended: impl Future<Output = anyhow::Result<T>>,
    work: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::select! {
        biased;
        result = peer_ended => result,
        result = Box::pin(work) => result,
    }
}

#[cfg(test)]
mod tests {
    use super::{Operation, Step, steps, while_peer_active};

    #[tokio::test]
    async fn an_ended_peer_stops_the_other_job_before_its_next_effect() -> anyhow::Result<()> {
        let work = async { panic!("work started after the peer Job ended") };
        while_peer_active(std::future::ready(Ok(())), work).await
    }

    #[test]
    fn reset_brackets_the_publication_and_reader_with_host_changes() {
        assert_eq!(
            steps(Operation::Reset),
            [
                Step::SuspendHost,
                Step::Publication,
                Step::WaitReader,
                Step::ResumeHost
            ]
        );
    }

    #[test]
    fn deploy_and_schema_apply_update_the_publication_before_waiting_for_the_reader() {
        for operation in [Operation::Deploy, Operation::Apply] {
            assert_eq!(steps(operation), [Step::Publication, Step::WaitReader]);
        }
    }

    #[test]
    fn capture_recovery_pauses_and_close_reconciles_the_reader() {
        assert_eq!(steps(Operation::Recover), [Step::PauseCapture]);
        assert_eq!(steps(Operation::Close), [Step::SyncCapture]);
    }
}
