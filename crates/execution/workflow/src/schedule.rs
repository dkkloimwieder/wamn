//! UTC schedule evaluation and host-owned schedule workflow admission
//! (docs/architecture/execution.md#workflows).
//!
//! The task wakes at every minute boundary, reads the enabled schedule
//! workflows of the application its loaded release serves in one SELECT, and
//! admits a run for each row whose cron names that minute. A row enabled or disabled between ticks takes
//! effect at the next tick.

use std::sync::Arc;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use wamn_catalog::parse_workflow_schedule;
use wamn_runtime::plugins::WamnPostgres;
use wamn_runtime::plugins::wamn_postgres::{ScheduleRunAdmission, ScheduleRunAdmitted, due_text};

/// One enabled schedule workflow of a declared application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnabledSchedule {
    pub application: String,
    pub workflow_id: String,
    /// Five-field cron, evaluated in UTC.
    pub schedule: String,
}

/// Where the schedule task reads its rows and admits its runs.
#[async_trait::async_trait]
pub trait ScheduleRows: Send + Sync {
    /// The enabled schedule workflows of the declared applications.
    async fn enabled(&self) -> anyhow::Result<Vec<EnabledSchedule>>;

    /// Admit the run of `schedule` due at `due`.
    async fn admit(
        &self,
        schedule: &EnabledSchedule,
        due: DateTime<Utc>,
    ) -> anyhow::Result<ScheduleRunAdmitted>;
}

/// The enabled schedule workflows of the declared applications. Binds: `$1`
/// project, `$2` environment, `$3` the application name.
const ENABLED_SCHEDULES_SQL: &str = "\
SELECT application, workflow_id, schedule FROM catalog.workflows \
 WHERE project_env_key = $1 AND environment = $2 AND application = $3 \
   AND enabled AND trigger = 'schedule'";

/// The schedule rows of the application that the host serves. It
/// reads them on the task's executor connection and admits through
/// [`WamnPostgres::admit_schedule_run`].
pub struct PostgresSchedules {
    client: tokio_postgres::Client,
    postgres: Arc<WamnPostgres>,
    project: String,
    release: Arc<wamn_engine::release_manifest::LoadedRelease>,
}

impl std::fmt::Debug for PostgresSchedules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresSchedules")
            .field("project", &self.project)
            .field("release", &self.release)
            .finish_non_exhaustive()
    }
}

impl PostgresSchedules {
    /// Serve the schedules of `applications` in `project_env_key` and `environment`,
    /// against the release `manifest_digest`.
    pub fn new(
        client: tokio_postgres::Client,
        postgres: Arc<WamnPostgres>,
        project: impl Into<String>,
        release: Arc<wamn_engine::release_manifest::LoadedRelease>,
    ) -> Self {
        Self {
            client,
            postgres,
            project: project.into(),
            release,
        }
    }
}

#[async_trait::async_trait]
impl ScheduleRows for PostgresSchedules {
    async fn enabled(&self) -> anyhow::Result<Vec<EnabledSchedule>> {
        let scope = self.release.scope();
        let (application, _) = scope
            .applications
            .first_key_value()
            .context("schedule host has no application")?;
        let rows = self
            .client
            .query(
                ENABLED_SCHEDULES_SQL,
                &[&scope.project_env_key, &scope.environment, application],
            )
            .await
            .context("read the enabled workflow schedules")?;
        rows.iter()
            .map(|row| {
                Ok(EnabledSchedule {
                    application: row.try_get(0)?,
                    workflow_id: row.try_get(1)?,
                    schedule: row.try_get(2)?,
                })
            })
            .collect::<Result<_, tokio_postgres::Error>>()
            .context("decode an enabled workflow schedule")
    }

    async fn admit(
        &self,
        schedule: &EnabledSchedule,
        due: DateTime<Utc>,
    ) -> anyhow::Result<ScheduleRunAdmitted> {
        let scope = self.release.scope();
        let application = scope
            .applications
            .get(&schedule.application)
            .with_context(|| format!("application {:?} is not declared", schedule.application))?;
        self.postgres
            .admit_schedule_run(
                &self.project,
                &scope.project_env_key,
                &ScheduleRunAdmission {
                    environment: &scope.environment,
                    application: &schedule.application,
                    application_packages: &application.packages,
                    workflow_id: &schedule.workflow_id,
                    manifest_digest: self.release.release().manifest_digest.as_str(),
                    due,
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!("admit the schedule run: {error}"))
    }
}

/// The first minute boundary strictly after `after`.
fn next_minute(after: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(after.timestamp().div_euclid(60) * 60 + 60, 0)
        .expect("a minute boundary after a valid time is a valid time")
}

/// Whether the cron of `schedule` names the minute `tick`.
fn is_due(schedule: &str, tick: DateTime<Utc>) -> anyhow::Result<bool> {
    parse_workflow_schedule(schedule)?
        .is_time_matching(&tick)
        .context("match the workflow schedule")
}

/// Serve the schedule workflows of `rows` until host shutdown. The first
/// tick is strictly after startup. Failed or missed ticks are not retried.
pub async fn serve(
    rows: &dyn ScheduleRows,
    stopping: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    serve_with_clock(rows, stopping, Utc::now, |tick| {
        tokio::time::sleep((tick - Utc::now()).to_std().unwrap_or_default())
    })
    .await
}

/// Serve schedules with a supplied clock and a wait for each minute boundary.
pub async fn serve_with_clock<F: std::future::Future<Output = ()>>(
    rows: &dyn ScheduleRows,
    mut stopping: tokio::sync::watch::Receiver<bool>,
    now: impl Fn() -> DateTime<Utc>,
    wait_until: impl Fn(DateTime<Utc>) -> F,
) -> anyhow::Result<()> {
    let mut tick = next_minute(now());
    loop {
        if *stopping.borrow() {
            return Ok(());
        }
        tokio::select! {
            biased;
            _ = stopping.changed() => return Ok(()),
            () = wait_until(tick) => {}
        }
        if next_minute(tick) <= now() {
            tracing::warn!(due = %due_text(tick), "missed workflow schedule tick");
            tick = next_minute(now());
            continue;
        }
        let enabled = tokio::select! {
            biased;
            _ = stopping.changed() => return Ok(()),
            enabled = rows.enabled() => enabled,
        };
        match enabled {
            Err(error) => {
                tracing::warn!(due = %due_text(tick), error = format!("{error:#}"), "workflow schedule tick skipped");
            }
            Ok(enabled) => {
                for schedule in enabled {
                    match is_due(&schedule.schedule, tick) {
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
        if next_minute(tick) <= now() {
            tracing::warn!(due = %due_text(tick), "missed workflow schedule ticks during admission");
        }
        tick = next_minute(now());
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

    #[test]
    fn a_cron_matches_its_minutes_in_utc() {
        assert!(is_due("0 0 * * *", utc("2026-10-21T00:00:00Z")).unwrap());
        assert!(is_due("0 0 * * *", utc("2026-10-20T20:00:00-04:00")).unwrap());
        assert!(!is_due("0 0 * * *", utc("2026-10-21T00:00:00-04:00")).unwrap());
        assert!(is_due("0 0 1 * *", utc("2027-01-01T00:00:00Z")).unwrap());
        assert!(!is_due("0 0 31 2 *", utc("2027-02-28T00:00:00Z")).unwrap());
        assert!(is_due("* * * * * *", utc("2027-01-01T00:00:00Z")).is_err());
    }
}
