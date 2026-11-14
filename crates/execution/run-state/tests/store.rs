//! Persisted status encoding at its public interface.
use wamn_run_state::{FailType, RunStatus};

#[test]
fn persisted_statuses_round_trip() {
    for status in RunStatus::ALL {
        assert_eq!(RunStatus::from_sql(status.as_sql()), Some(status));
        assert_eq!(
            serde_json::from_value::<RunStatus>(serde_json::to_value(status).unwrap()).unwrap(),
            status
        );
    }
    for failure in FailType::ALL {
        assert_eq!(FailType::from_sql(failure.as_sql()), Some(failure));
        assert_eq!(
            serde_json::from_value::<FailType>(serde_json::to_value(failure).unwrap()).unwrap(),
            failure
        );
    }
    assert!(RunStatus::from_sql("parked").is_none());
    assert!(serde_json::from_value::<RunStatus>(serde_json::json!("parked")).is_err());
}
