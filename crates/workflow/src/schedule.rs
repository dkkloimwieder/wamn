//! Calendar schedule evaluation and host-owned schedule workflow admission
//! (docs/architecture/execution.md#workflows).
//!
//! The task wakes at every minute boundary, reads the enabled schedule
//! workflows of the current application selection in one SELECT, and
//! admits a run for each row whose cron names that minute. A row enabled or disabled between ticks takes
//! effect at the next tick.

use std::sync::Arc;

use crate::admission::{ScheduleRunAdmitted, admit_schedule_run, due_text};
use anyhow::Context as _;
use chrono::{DateTime, Utc};
use wamn_catalog::parse_workflow_schedule;
use wamn_postgres_capability::WamnPostgres;

/// One enabled schedule workflow of a declared application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnabledSchedule {
    pub application: String,
    pub workflow_id: String,
    /// Five-field cron, evaluated in the environment time zone.
    pub schedule: String,
    /// The due instant expressed in the environment time zone.
    pub local_tick: chrono::NaiveDateTime,
}

/// The schedule rows of the application that the host serves. It
/// reads them on the task's executor connection and admits through
/// [`admit_schedule_run`].
pub struct PostgresSchedules {
    postgres: Arc<WamnPostgres>,
    project: String,
    application: String,
    selection:
        tokio::sync::watch::Receiver<Option<Arc<wamn_engine::release_manifest::LoadedRelease>>>,
}

impl std::fmt::Debug for PostgresSchedules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresSchedules")
            .field("project", &self.project)
            .field("selection", &self.selection.borrow())
            .finish_non_exhaustive()
    }
}

impl PostgresSchedules {
    /// Resolve schedule admission against the current host selection.
    pub fn new(
        postgres: Arc<WamnPostgres>,
        project: impl Into<String>,
        application: impl Into<String>,
        selection: tokio::sync::watch::Receiver<
            Option<Arc<wamn_engine::release_manifest::LoadedRelease>>,
        >,
    ) -> Self {
        Self {
            postgres,
            project: project.into(),
            application: application.into(),
            selection,
        }
    }
}

impl PostgresSchedules {
    /// Read schedules through the executor pool owned by the runtime.
    pub async fn enabled(&self, tick: DateTime<Utc>) -> anyhow::Result<Vec<EnabledSchedule>> {
        let release = self
            .selection
            .borrow()
            .as_ref()
            .map(Arc::clone)
            .context("no current release")?;
        let target = release.target();
        anyhow::ensure!(
            target.applications.contains_key(&self.application),
            "schedule application is outside the selected release"
        );
        let applications = vec![self.application.clone()];
        self.postgres
            .executor_read(&self.project, &target.project_env_key, async |connection| {
                Ok(wamn_run_state::admission::enabled_schedules(
                    connection,
                    &target.project_env_key,
                    &target.environment,
                    &applications,
                    &self.project,
                    tick,
                )
                .await?
                .into_iter()
                .map(
                    |(application, workflow_id, schedule, local_tick)| EnabledSchedule {
                        application,
                        workflow_id,
                        schedule,
                        local_tick,
                    },
                )
                .collect())
            })
            .await
    }

    /// Admit one due schedule against the current release.
    pub async fn admit(
        &self,
        schedule: &EnabledSchedule,
        due: DateTime<Utc>,
    ) -> anyhow::Result<ScheduleRunAdmitted> {
        let release = self
            .selection
            .borrow()
            .as_ref()
            .map(Arc::clone)
            .context("no current release")?;
        anyhow::ensure!(
            schedule.application == self.application,
            "schedule belongs to another application"
        );
        Ok(admit_schedule_run(
            &self.postgres,
            &self.project,
            &release,
            &schedule.application,
            &schedule.workflow_id,
            due,
        )
        .await?)
    }
}

/// The first minute boundary strictly after `after`.
fn next_minute(after: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(after.timestamp().div_euclid(60) * 60 + 60, 0)
        .expect("a minute boundary after a valid time is a valid time")
}

/// Whether the cron of `schedule` names the minute `tick`.
pub fn is_due(schedule: &str, tick: chrono::NaiveDateTime) -> anyhow::Result<bool> {
    parse_workflow_schedule(schedule)?
        .is_time_matching(&tick.and_utc())
        .context("match the workflow schedule")
}

/// Serve the schedule workflows of `rows` until host shutdown. The first
/// tick is strictly after startup. Failed or missed ticks are not retried.
pub async fn serve(
    rows: &PostgresSchedules,
    mut stopping: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut tick = next_minute(Utc::now());
    loop {
        if *stopping.borrow() {
            return Ok(());
        }
        tokio::select! {
            biased;
            _ = stopping.changed() => return Ok(()),
            () = tokio::time::sleep((tick - Utc::now()).to_std().unwrap_or_default()) => {}
        }
        if next_minute(tick) <= Utc::now() {
            tracing::warn!(due = %due_text(tick), "missed workflow schedule tick");
            tick = next_minute(Utc::now());
            continue;
        }
        let enabled = tokio::select! {
            biased;
            _ = stopping.changed() => return Ok(()),
            enabled = rows.enabled(tick) => enabled,
        };
        match enabled {
            Err(error) => {
                tracing::warn!(due = %due_text(tick), error = format!("{error:#}"), "workflow schedule tick skipped");
            }
            Ok(enabled) => {
                for schedule in enabled {
                    match is_due(&schedule.schedule, schedule.local_tick) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(error) => {
                            tracing::warn!(workflow = %schedule.workflow_id, error = format!("{error:#}"), "workflow schedule skipped");
                            continue;
                        }
                    }
                    let admitted = tokio::select! {
                        biased;
                        _ = stopping.changed() => return Ok(()),
                        admitted = rows.admit(&schedule, tick) => admitted,
                    };
                    match admitted {
                        Ok(ScheduleRunAdmitted::Queued { run_id }) => {
                            tracing::info!(workflow = %schedule.workflow_id, due = %due_text(tick), %run_id, "workflow schedule run admitted");
                        }
                        Ok(refused) => {
                            tracing::warn!(workflow = %schedule.workflow_id, due = %due_text(tick), ?refused, "workflow schedule tick refused");
                        }
                        Err(error) => {
                            tracing::warn!(workflow = %schedule.workflow_id, due = %due_text(tick), error = format!("{error:#}"), "workflow schedule tick refused");
                        }
                    }
                }
            }
        }
        if next_minute(tick) <= Utc::now() {
            tracing::warn!(due = %due_text(tick), "missed workflow schedule ticks during admission");
        }
        tick = next_minute(Utc::now());
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
    fn next_minute_is_strict_and_uses_utc() {
        for (now, expected) in [
            ("2026-10-20T10:15:23Z", "2026-10-20T10:16:00Z"),
            ("2026-10-20T10:15:59.999Z", "2026-10-20T10:16:00Z"),
            ("2026-10-20T10:16:00Z", "2026-10-20T10:17:00Z"),
            ("2026-12-31T23:59:59Z", "2027-01-01T00:00:00Z"),
            ("2026-10-20T21:00:00-04:00", "2026-10-21T01:01:00Z"),
        ] {
            assert_eq!(next_minute(utc(now)), utc(expected));
        }
    }
}
