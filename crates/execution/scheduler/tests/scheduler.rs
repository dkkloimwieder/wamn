//! Scheduling ownership tests: cron, reconciliation, and cadence stay separate
//! from durable run-state and require the adapter to supply time.

use wamn_scheduler::{
    Cadence, CronError, DEFAULT_MAX_INTERVAL_MS, DEFAULT_MIN_INTERVAL_MS, cron_firing,
    cron_tick_of, due_tick, mint_cron_run_id, next_fire, next_reconcile, reconcile_due,
};

/// 2026-01-01 00:00:00 UTC.
const JAN1_2026: i64 = 1_767_225_600_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;

#[test]
fn reconciliation_uses_adapter_supplied_time() {
    assert!(!reconcile_due(1_000, 900, 200));
    assert!(reconcile_due(1_100, 900, 200));
    assert_eq!(next_reconcile(900, 200), 1_100);
}

#[test]
fn cron_next_fire_is_strictly_after() {
    let two_am = JAN1_2026 + 2 * HOUR;
    assert_eq!(next_fire("0 2 * * *", JAN1_2026).unwrap(), two_am);
    assert_eq!(next_fire("0 2 * * *", two_am).unwrap(), two_am + DAY);
    assert!(next_fire("not a cron", 0).is_err());
}

#[test]
fn cron_calendar_edges() {
    let feb29_2028: i64 = 1_835_395_200_000;
    assert_eq!(next_fire("0 0 29 2 *", JAN1_2026).unwrap(), feb29_2028);

    let apr1_2026 = JAN1_2026 + 90 * DAY;
    assert_eq!(
        next_fire("0 0 31 * *", apr1_2026).unwrap(),
        apr1_2026 + 60 * DAY
    );
}

#[test]
fn due_tick_fires_latest_and_collapses_misfires() {
    let schedule = "0 2 * * *";
    let first_tick = JAN1_2026 + 2 * HOUR;
    assert_eq!(
        due_tick(schedule, JAN1_2026, JAN1_2026 + HOUR).unwrap(),
        None
    );
    assert_eq!(
        due_tick(schedule, JAN1_2026, first_tick).unwrap(),
        Some(first_tick)
    );
    assert_eq!(
        due_tick(schedule, JAN1_2026, first_tick + 500).unwrap(),
        Some(first_tick)
    );

    let now = JAN1_2026 + 3 * DAY + 12 * HOUR;
    let latest = JAN1_2026 + 3 * DAY + 2 * HOUR;
    assert_eq!(due_tick(schedule, first_tick, now).unwrap(), Some(latest));
    assert_eq!(due_tick(schedule, latest, now).unwrap(), None);
    assert!(due_tick("* * bogus", 0, 1).is_err());
    assert!(due_tick("0 0 30 2 *", JAN1_2026, JAN1_2026 + DAY).is_err());
    assert!(next_fire("0 0 30 2 *", JAN1_2026).is_err());
}

#[test]
fn cron_error_variants_pin_the_failure_mode() {
    assert!(matches!(
        next_fire("not a cron", 0),
        Err(CronError::InvalidExpression { .. })
    ));
    assert!(matches!(
        next_fire("0 0 30 2 *", JAN1_2026),
        Err(CronError::NoOccurrence { .. })
    ));
    assert!(matches!(
        due_tick("0 0 30 2 *", JAN1_2026, JAN1_2026 + DAY),
        Err(CronError::NoOccurrence { .. })
    ));
    assert!(matches!(
        next_fire("* * * * *", i64::MAX),
        Err(CronError::OutOfRangeInstant { ms }) if ms == i64::MAX
    ));

    for error in [
        CronError::InvalidExpression {
            schedule: "x".into(),
            detail: "bad".into(),
        },
        CronError::OutOfRangeInstant { ms: i64::MAX },
        CronError::NoOccurrence {
            schedule: "0 0 30 2 *".into(),
            detail: "none".into(),
        },
    ] {
        assert!(error.to_string().starts_with("cron: "), "{error}");
    }
}

#[test]
fn cron_run_ids_are_deterministic_and_ordered() {
    let a = mint_cron_run_id("escalate-stale-holds", JAN1_2026);
    let b = mint_cron_run_id("escalate-stale-holds", JAN1_2026 + DAY);
    assert_eq!(a, "escalate-stale-holds:cron:0:2026-01-01T00:00:00Z");
    assert!(a < b);
    assert_eq!(cron_tick_of("escalate-stale-holds", &a), Some(JAN1_2026));

    let small = mint_cron_run_id("f", 42);
    assert_eq!(small, "f:cron:0:1970-01-01T00:00:00.042Z");
    assert_eq!(cron_tick_of("f", &small), Some(42));
    assert_eq!(cron_tick_of("f", "f:outbox:42"), None);
    assert_eq!(cron_tick_of("f", "plain-run"), None);
    assert_eq!(
        cron_tick_of("a", "acron5:cron:0:1970-01-01T00:00:00.042Z"),
        None
    );
    assert_eq!(
        cron_tick_of("a", "a:cron:5x:cron:0:1970-01-01T00:00:00.042Z"),
        None
    );
    assert_eq!(
        cron_tick_of("a:cron:5x", "a:cron:5x:cron:0:1970-01-01T00:00:00.042Z"),
        Some(42)
    );

    let firing = cron_firing("escalate-stale-holds", 3, 0, JAN1_2026, JAN1_2026 + 5_000).unwrap();
    assert_eq!(firing.run_id, a);
    assert_eq!(firing.flow_id, "escalate-stale-holds");
    assert_eq!(firing.flow_version, 3);
    assert_eq!(firing.trigger_source, "cron");
    let input: serde_json::Value = serde_json::from_str(&firing.input_json).unwrap();
    assert_eq!(input["scheduled-at"], "2026-01-01T00:00:00Z");
    assert_eq!(input["fired-at"], "2026-01-01T00:00:05Z");
}

#[test]
fn adaptive_interval_tightens_on_work_and_decays_to_max() {
    let cadence = Cadence::new(DEFAULT_MIN_INTERVAL_MS, DEFAULT_MAX_INTERVAL_MS).unwrap();
    let (min, max) = (cadence.min(), cadence.max());
    assert_eq!(cadence.next_interval(max, true), min);
    assert_eq!(cadence.next_interval(min, true), min);
    assert_eq!(cadence.next_interval(min, false), 2 * min);
    assert_eq!(cadence.next_interval(2 * min, false), 4 * min);
    assert_eq!(cadence.next_interval(20_000, false), max);
    assert_eq!(cadence.next_interval(max, false), max);
    assert_eq!(cadence.next_interval(0, false), min);
}
