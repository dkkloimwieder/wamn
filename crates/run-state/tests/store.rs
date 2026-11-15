//! Persisted status encoding at its public interface.
use wamn_run_state::{FailType, RunStatus};

#[test]
fn persisted_statuses_round_trip() {
    for status in RunStatus::ALL {
        assert_eq!(RunStatus::from_sql(status.as_sql()), Some(status));
    }
    for failure in FailType::ALL {
        assert_eq!(FailType::from_sql(failure.as_sql()), Some(failure));
    }
    assert!(RunStatus::from_sql("parked").is_none());
}
