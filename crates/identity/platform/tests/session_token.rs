use wamn_platform_identity::session_token::minting_times;

#[test]
fn delayed_minting_keeps_original_deadline_and_refuses_late_results() {
    assert_eq!(minting_times(1000, 1000).unwrap(), (1000, 1900));
    assert_eq!(minting_times(1000, 1800).unwrap(), (1800, 1900));
    assert_eq!(minting_times(1000, 1899).unwrap(), (1899, 1900));
    for (started, now) in [
        (1000, 1900),
        (1000, 1901),
        (1000, 999),
        (i64::MAX, i64::MAX),
        (-1, 0),
    ] {
        assert!(minting_times(started, now).is_err());
    }
}
