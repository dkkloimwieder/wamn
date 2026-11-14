//! Schedule decisions at the public interface.
use chrono::{DateTime, Utc};
use wamn_workflow::schedule::is_due;

#[test]
fn a_schedule_matches_its_calendar_minutes() {
    let minute = |value: &str| {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
            .naive_utc()
    };
    assert!(is_due("0 0 * * *", minute("2026-10-21T00:00:00Z")).unwrap());
    assert!(!is_due("0 0 * * *", minute("2026-10-21T00:01:00Z")).unwrap());
    assert!(is_due("0 0 1 * *", minute("2027-01-01T00:00:00Z")).unwrap());
    assert!(!is_due("0 0 31 2 *", minute("2027-02-28T00:00:00Z")).unwrap());
    assert!(is_due("* * * * * *", minute("2027-01-01T00:00:00Z")).is_err());
}
