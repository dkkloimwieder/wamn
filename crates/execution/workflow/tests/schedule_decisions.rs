//! Scheduling decisions with controlled time and in-memory rows.
use chrono::{DateTime, Utc};
use std::sync::Mutex;
use wamn_runtime::plugins::wamn_postgres::{ScheduleRunAdmitted, due_text};
use wamn_workflow::schedule::{EnabledSchedule, ScheduleRows, serve_with_clock};

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
    let now = || start + chrono::Duration::from_std(instant.elapsed()).unwrap();
    let serving = serve_with_clock(recorded, stopped, now, |tick| {
        tokio::time::sleep((tick - now()).to_std().unwrap_or_default())
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
