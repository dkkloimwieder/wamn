//! UTC schedule evaluation and host-owned workflow admission.

use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use croner::Cron;
use wamn_catalog::ServingSchedule;

use crate::{StartRequest, Trigger, Workflows};

/// The canonical UTC spelling shared by schedule keys and input.
pub fn due_text(due: DateTime<Utc>) -> String {
    due.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// One key for a package workflow and due moment, shared by all replicas.
pub fn idempotency_key(package_id: &str, workflow_id: &str, due: DateTime<Utc>) -> String {
    format!("{package_id}/{workflow_id}:{}", due_text(due))
}

fn parse_schedule(text: &str) -> anyhow::Result<Cron> {
    anyhow::ensure!(
        text.split_whitespace().count() == 5,
        "schedule requires five fields"
    );
    croner::parser::CronParser::builder()
        .seconds(croner::parser::Seconds::Disallowed)
        .year(croner::parser::Year::Disallowed)
        .build()
        .parse(text)
        .context("parse workflow schedule")
}

fn next_due(cron: &Cron, after: DateTime<Utc>) -> anyhow::Result<DateTime<Utc>> {
    cron.find_next_occurrence(&after, false)
        .context("find next workflow due moment")
}

/// Run the schedules of one loaded environment release until host shutdown.
/// The first tick is strictly after startup. Failed or missed ticks are not retried.
pub async fn serve(
    workflows: &dyn Workflows,
    manifest_digest: &str,
    schedules: &[ServingSchedule],
    mut stopping: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let started = Utc::now();
    let mut pending = schedules
        .iter()
        .map(|schedule| {
            let cron = parse_schedule(&schedule.schedule)?;
            let due = next_due(&cron, started)?;
            Ok((schedule, cron, due))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    loop {
        if *stopping.borrow() {
            return Ok(());
        }
        let Some(due) = pending.iter().map(|(_, _, due)| *due).min() else {
            return Ok(());
        };
        let wait = (due - Utc::now()).to_std().unwrap_or_default();
        tokio::select! {
            biased;
            _ = stopping.changed() => return Ok(()),
            () = tokio::time::sleep(wait) => {}
        }
        for (schedule, cron, due) in &mut pending {
            let now = Utc::now();
            if *due > now {
                continue;
            }
            if next_due(cron, *due)? <= now {
                tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, due = %due, "missed workflow schedule tick");
            } else {
                let request = StartRequest {
                    manifest_digest: manifest_digest.to_owned(),
                    package_id: schedule.package_id.clone(),
                    wiring_id: schedule.wiring_id.clone(),
                    wiring_version: schedule.wiring_version,
                    idempotency_key: idempotency_key(
                        &schedule.package_id,
                        &schedule.workflow_id,
                        *due,
                    ),
                    input: serde_json::json!({"due": due_text(*due)}),
                    trigger: Trigger::Schedule {
                        workflow_id: schedule.workflow_id.clone(),
                        due: *due,
                    },
                };
                let result = tokio::select! {
                    biased;
                    _ = stopping.changed() => return Ok(()),
                    result = workflows.start(&request) => result,
                };
                if let Err(error) = result {
                    tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, due = %due, %error, "workflow schedule tick refused");
                }
                if next_due(cron, *due)? <= Utc::now() {
                    tracing::warn!(package = %schedule.package_id, workflow = %schedule.workflow_id, due = %due, "missed workflow schedule ticks during admission");
                }
            }
            *due = next_due(cron, Utc::now())?;
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
                next_due(&parse_schedule(pattern).unwrap(), utc(now)).unwrap(),
                utc(expected)
            );
        }
    }

    #[test]
    fn key_names_package_workflow_and_utc_due() {
        assert_eq!(
            idempotency_key("inventory", "labels", utc("2026-10-20T10:15:00-04:00")),
            "inventory/labels:2026-10-20T14:15:00Z"
        );
    }
}
