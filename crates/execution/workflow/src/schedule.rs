//! UTC schedule evaluation and host-owned schedule workflow admission
//! (docs/plan/workflows.md R3, R8, epic 2).
//!
//! The task wakes at every minute boundary, reads the enabled schedule
//! workflows of the applications its environment declares in one SELECT, and
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
/// tenant, `$2` environment, `$3` the application names.
const ENABLED_SCHEDULES_SQL: &str = "\
SELECT application, workflow_id, schedule FROM catalog.workflows \
 WHERE tenant_id = $1 AND environment = $2 AND application = ANY($3) \
   AND enabled AND trigger = 'schedule'";

/// The schedule rows of the applications a host's environment declares. It
/// reads them on the task's executor connection and admits through
/// [`WamnPostgres::admit_schedule_run`].
pub struct PostgresSchedules {
    client: tokio_postgres::Client,
    postgres: Arc<WamnPostgres>,
    project: String,
    tenant: String,
    environment: String,
    applications: wamn_catalog::EnvironmentApplications,
    /// The names of `applications`, as the read binds them.
    names: Vec<String>,
    /// The release the host serves, as its manifest digest.
    manifest_digest: String,
}

impl std::fmt::Debug for PostgresSchedules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresSchedules")
            .field("project", &self.project)
            .field("tenant", &self.tenant)
            .field("environment", &self.environment)
            .field("applications", &self.applications)
            .field("manifest_digest", &self.manifest_digest)
            .finish_non_exhaustive()
    }
}

impl PostgresSchedules {
    /// Serve the schedules of `applications` in `tenant` and `environment`,
    /// against the release `manifest_digest`.
    pub fn new(
        client: tokio_postgres::Client,
        postgres: Arc<WamnPostgres>,
        project: impl Into<String>,
        tenant: impl Into<String>,
        environment: impl Into<String>,
        applications: wamn_catalog::EnvironmentApplications,
        manifest_digest: impl Into<String>,
    ) -> Self {
        let names = applications.keys().cloned().collect();
        Self {
            names,
            client,
            postgres,
            project: project.into(),
            tenant: tenant.into(),
            environment: environment.into(),
            applications,
            manifest_digest: manifest_digest.into(),
        }
    }
}

#[async_trait::async_trait]
impl ScheduleRows for PostgresSchedules {
    async fn enabled(&self) -> anyhow::Result<Vec<EnabledSchedule>> {
        let rows = self
            .client
            .query(
                ENABLED_SCHEDULES_SQL,
                &[&self.tenant, &self.environment, &self.names],
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
        let application = self
            .applications
            .get(&schedule.application)
            .with_context(|| format!("application {:?} is not declared", schedule.application))?;
        self.postgres
            .admit_schedule_run(
                &self.project,
                &self.tenant,
                &ScheduleRunAdmission {
                    environment: &self.environment,
                    application: &schedule.application,
                    application_packages: &application.packages,
                    workflow_id: &schedule.workflow_id,
                    manifest_digest: &self.manifest_digest,
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
    serve_with_clock(rows, stopping, Utc::now).await
}

async fn serve_with_clock(
    rows: &dyn ScheduleRows,
    mut stopping: tokio::sync::watch::Receiver<bool>,
    now: impl Fn() -> DateTime<Utc>,
) -> anyhow::Result<()> {
    let mut tick = next_minute(now());
    loop {
        if *stopping.borrow() {
            return Ok(());
        }
        let wait = (tick - now()).to_std().unwrap_or_default();
        tokio::select! {
            biased;
            _ = stopping.changed() => return Ok(()),
            () = tokio::time::sleep(wait) => {}
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
    use std::sync::Mutex;

    use super::*;

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn row(workflow_id: &str, schedule: &str) -> EnabledSchedule {
        EnabledSchedule {
            application: "app".into(),
            workflow_id: workflow_id.into(),
            schedule: schedule.into(),
        }
    }

    /// Rows a test changes between ticks, and every admission and read.
    #[derive(Default)]
    struct Recorded {
        rows: Mutex<Vec<EnabledSchedule>>,
        reads: Mutex<u32>,
        admitted: Mutex<Vec<(String, DateTime<Utc>)>>,
        /// One admission sleeps this long, to run past the next tick.
        slow: Mutex<Option<std::time::Duration>>,
    }

    #[async_trait::async_trait]
    impl ScheduleRows for Recorded {
        async fn enabled(&self) -> anyhow::Result<Vec<EnabledSchedule>> {
            *self.reads.lock().unwrap() += 1;
            Ok(self.rows.lock().unwrap().clone())
        }
        async fn admit(
            &self,
            schedule: &EnabledSchedule,
            due: DateTime<Utc>,
        ) -> anyhow::Result<ScheduleRunAdmitted> {
            self.admitted
                .lock()
                .unwrap()
                .push((schedule.workflow_id.clone(), due));
            let slow = self.slow.lock().unwrap().take();
            if let Some(slow) = slow {
                tokio::time::sleep(slow).await;
            }
            Ok(ScheduleRunAdmitted::Queued {
                run_id: "run".into(),
            })
        }
    }

    /// Run the task from `start` for `seconds` of paused time, changing the
    /// rows with `change` after `change_after` seconds.
    async fn run(
        recorded: &Recorded,
        start: &str,
        seconds: u64,
        change_after: u64,
        change: impl FnOnce(&Recorded),
    ) {
        let start = utc(start);
        let instant = tokio::time::Instant::now();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let serving = serve_with_clock(recorded, stopped, || {
            start + chrono::Duration::from_std(instant.elapsed()).unwrap()
        });
        tokio::pin!(serving);
        tokio::select! {
            result = &mut serving => panic!("schedule task stopped: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(change_after)) => {}
        }
        change(recorded);
        tokio::select! {
            result = &mut serving => panic!("schedule task stopped: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(seconds - change_after)) => {}
        }
        stop.send(true).unwrap();
        serving.await.unwrap();
    }

    fn admitted(recorded: &Recorded) -> Vec<(String, String)> {
        recorded
            .admitted
            .lock()
            .unwrap()
            .iter()
            .map(|(workflow_id, due)| (workflow_id.clone(), due_text(*due)))
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn restart_fires_strictly_after_start_and_skips_invalid_rows() {
        let recorded = Recorded::default();
        *recorded.rows.lock().unwrap() = vec![
            row("never", "0 0 31 2 *"),
            row("invalid", "* * * * * *"),
            row("minute", "* * * * *"),
        ];
        for start in ["2026-10-20T10:15:23Z", "2026-10-20T10:20:23Z"] {
            run(&recorded, start, 38, 0, |_| {}).await;
        }
        assert_eq!(
            admitted(&recorded),
            [
                ("minute".into(), "2026-10-20T10:16:00Z".into()),
                ("minute".into(), "2026-10-20T10:21:00Z".into()),
            ]
        );
        // One read per tick.
        assert_eq!(*recorded.reads.lock().unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_row_enabled_or_disabled_between_ticks_takes_effect_at_the_next_tick() {
        let recorded = Recorded::default();
        run(&recorded, "2026-10-20T10:15:23Z", 158, 60, |recorded| {
            *recorded.rows.lock().unwrap() = vec![row("minute", "* * * * *")];
        })
        .await;
        assert_eq!(
            admitted(&recorded),
            [
                ("minute".into(), "2026-10-20T10:17:00Z".into()),
                ("minute".into(), "2026-10-20T10:18:00Z".into()),
            ]
        );
        run(&recorded, "2026-10-20T10:20:23Z", 98, 60, |recorded| {
            recorded.rows.lock().unwrap().clear();
        })
        .await;
        assert_eq!(admitted(&recorded).len(), 3);
        assert_eq!(admitted(&recorded)[2].1, "2026-10-20T10:21:00Z");
    }

    #[tokio::test(start_paused = true)]
    async fn ticks_missed_during_a_slow_admission_are_skipped() {
        let recorded = Recorded::default();
        *recorded.rows.lock().unwrap() = vec![row("minute", "* * * * *")];
        *recorded.slow.lock().unwrap() = Some(std::time::Duration::from_secs(90));
        run(&recorded, "2026-10-20T10:15:23Z", 158, 0, |_| {}).await;
        // 10:17 passed during the 10:16 admission; the next tick is 10:18.
        assert_eq!(
            admitted(&recorded),
            [
                ("minute".into(), "2026-10-20T10:16:00Z".into()),
                ("minute".into(), "2026-10-20T10:18:00Z".into()),
            ]
        );
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
