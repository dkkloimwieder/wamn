use wamn_identity::session_token::signing_times;

#[test]
fn delayed_signing_keeps_original_deadline_and_refuses_late_results() {
    assert_eq!(signing_times(1000, 1000).unwrap(), (1000, 1900));
    assert_eq!(signing_times(1000, 1800).unwrap(), (1800, 1900));
    assert_eq!(signing_times(1000, 1899).unwrap(), (1899, 1900));
    for (started, now) in [
        (1000, 1900),
        (1000, 1901),
        (1000, 999),
        (i64::MAX, i64::MAX),
        (-1, 0),
    ] {
        assert!(signing_times(started, now).is_err());
    }
}
