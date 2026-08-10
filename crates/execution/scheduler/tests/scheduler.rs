//! Public cadence contract tests.

use wamn_scheduler::{Cadence, DEFAULT_MAX_INTERVAL_MS, DEFAULT_MIN_INTERVAL_MS};

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
