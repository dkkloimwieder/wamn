//! UTC schedule evaluation and host-owned workflow admission.

use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use croner::Cron;
use wamn_catalog::{ServingSchedule, parse_workflow_schedule};

use crate::{StartRequest, Trigger, Workflows};

/// The canonical UTC spelling shared by schedule keys and input.
pub fn due_text(due: DateTime<Utc>) -> String {
    due.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// One key for a package workflow and due moment, shared by all replicas.
pub fn idempotency_key(package_id: &str, workflow_id: &str, due: DateTime<Utc>) -> String {
    format!("schedule:{package_id}/{workflow_id}:{}", due_text(due))
}

fn next_due(cron: &Cron, after: DateTime<Utc>) -> anyhow::Result<DateTime<Utc>> {
    cron.find_next_occurrence(&after, false)
        .context("find next workflow due moment")
}

/// Run the schedules of one loaded environment release until host shutdown.
/// Each start names the package whose service principal the contract resolves.
/// The first tick is strictly after startup. Failed or missed ticks are not retried.
pub async fn serve(
    workflows: &dyn Workflows,
    manifest_digest: &str,
    schedules: &[ServingSchedule],
    stopping: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    serve_with_clock(workflows, manifest_digest, schedules, stopping, Utc::now).await
}

async fn serve_with_clock(
    workflows: &dyn Workflows,
    manifest_digest: &str,
    schedules: &[ServingSchedule],
    mut stopping: tokio::sync::watch::Receiver<bool>,
    now: impl Fn() -> DateTime<Utc>,
) -> anyhow::Result<()> {
    let started = now();
    let mut pending = Vec::new();
    for schedule in schedules {
        let parsed = parse_workflow_schedule(&schedule.schedule)
            .map_err(anyhow::Error::from)
            .and_then(|cron| next_due(&cron, started).map(|due| (cron, due)));
        match parsed {
            Ok((cron, due)) => pending.push((schedule, cron, Some(due))),
            Err(error) => {
                tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, %error, "workflow schedule skipped");
            }
        }
    }
    loop {
        if *stopping.borrow() {
            return Ok(());
        }
        let Some(due) = pending.iter().filter_map(|(_, _, due)| *due).min() else {
            let _ = stopping.changed().await;
            return Ok(());
        };
        let wait = (due - now()).to_std().unwrap_or_default();
        tokio::select! {
            biased;
            _ = stopping.changed() => return Ok(()),
            () = tokio::time::sleep(wait) => {}
        }
        for (schedule, cron, due) in &mut pending {
            let Some(tick) = *due else { continue };
            if tick > now() {
                continue;
            }
            let next = match next_due(cron, tick) {
                Ok(next) => next,
                Err(error) => {
                    tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, %error, "workflow schedule skipped");
                    *due = None;
                    continue;
                }
            };
            if next <= now() {
                tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, due = %tick, "missed workflow schedule tick");
            } else {
                let request = StartRequest {
                    manifest_digest: manifest_digest.to_owned(),
                    package_id: schedule.package_id.clone(),
                    wiring_id: schedule.wiring_id.clone(),
                    wiring_version: schedule.wiring_version,
                    idempotency_key: idempotency_key(
                        &schedule.package_id,
                        &schedule.workflow_id,
                        tick,
                    ),
                    input: serde_json::json!({"due": due_text(tick)}),
                    trigger: Trigger::Schedule {
                        workflow_id: schedule.workflow_id.clone(),
                        due: tick,
                    },
                };
                let result = tokio::select! {
                    biased;
                    _ = stopping.changed() => return Ok(()),
                    result = workflows.start(&request) => result,
                };
                if let Err(error) = result {
                    tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, due = %tick, %error, "workflow schedule tick refused");
                }
                if next <= now() {
                    tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, due = %tick, "missed workflow schedule ticks during admission");
                }
            }
            *due = match next_due(cron, now()) {
                Ok(next) => Some(next),
                Err(error) => {
                    tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, %error, "workflow schedule skipped");
                    None
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[derive(Default)]
    struct Recorded(std::sync::Mutex<Vec<StartRequest>>);

    #[async_trait::async_trait]
    impl Workflows for Recorded {
        async fn start(&self, request: &StartRequest) -> Result<String, crate::WorkflowError> {
            self.0.lock().unwrap().push(request.clone());
            Ok("run".into())
        }
        async fn park(&self, _: &str) -> Result<(), crate::WorkflowError> {
            unreachable!()
        }
        async fn release(&self, _: &str) -> Result<(), crate::WorkflowError> {
            unreachable!()
        }
        async fn list(&self, _: u32) -> Result<Vec<crate::WorkflowRun>, crate::WorkflowError> {
            unreachable!()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn serve_restart_skips_missed_ticks_and_invalid_schedules() {
        let recorded = Recorded::default();
        let valid = ServingSchedule {
            package_id: "integration".into(),
            workflow_id: "minute".into(),
            wiring_id: "pull".into(),
            wiring_version: 1,
            schedule: "* * * * *".into(),
        };
        let mut invalid = valid.clone();
        invalid.workflow_id = "impossible".into();
        invalid.schedule = "0 0 31 2 *".into();
        let schedules = [invalid, valid];
        for start in ["2026-10-20T10:15:23Z", "2026-10-20T10:20:23Z"] {
            let start = utc(start);
            let instant = tokio::time::Instant::now();
            let (stop, stopped) = tokio::sync::watch::channel(false);
            let serving = serve_with_clock(&recorded, "release", &schedules, stopped, || {
                start + chrono::Duration::from_std(instant.elapsed()).unwrap()
            });
            tokio::pin!(serving);
            tokio::select! {
                result = &mut serving => panic!("schedule task stopped: {result:?}"),
                () = tokio::time::sleep(std::time::Duration::from_secs(38)) => {}
            }
            stop.send(true).unwrap();
            serving.await.unwrap();
        }
        let requests = recorded.0.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].input["due"], "2026-10-20T10:16:00Z");
        assert_eq!(requests[1].input["due"], "2026-10-20T10:21:00Z");
        assert!(
            requests
                .iter()
                .all(|request| request.idempotency_key.starts_with("schedule:"))
        );
    }

    #[test]
    fn next_due_is_strict_and_uses_utc_across_boundaries() {
        for (pattern, now, expected) in [
            ("* * * * *", "2026-10-20T10:15:23Z", "2026-10-20T10:16:00Z"),
            (
                "* * * * *",
                "2026-10-20T10:15:59.999Z",
                "2026-10-20T10:16:00Z",
            ),
            ("* * * * *", "2026-10-20T10:16:00Z", "2026-10-20T10:17:00Z"),
            ("0 0 * * *", "2026-10-20T23:59:59Z", "2026-10-21T00:00:00Z"),
            ("0 0 1 * *", "2026-12-31T23:59:59Z", "2027-01-01T00:00:00Z"),
            (
                "0 0 * * *",
                "2026-10-20T21:00:00-04:00",
                "2026-10-22T00:00:00Z",
            ),
        ] {
            assert_eq!(
                next_due(&parse_workflow_schedule(pattern).unwrap(), utc(now)).unwrap(),
                utc(expected)
            );
        }
    }

    #[test]
    fn key_names_package_workflow_and_utc_due() {
        assert_eq!(
            idempotency_key("inventory", "labels", utc("2026-10-20T10:15:00-04:00")),
            "schedule:inventory/labels:2026-10-20T14:15:00Z"
        );
    }
}
