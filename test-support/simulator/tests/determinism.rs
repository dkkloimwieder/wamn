//! The determinism gate: a profile plus a count is a total function to bytes.
//!
//! `docs/poc/wms-prep-spec.md` §1a: "Same seed = byte-identical stream
//! (canonical JSON via the existing shared canonicalization — no new
//! serializer)."

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use wamn_simulator::{Event, FaultPlan, Profile, ProfileKind};

const COUNT: usize = 250;

fn profile(seed: u64) -> Profile {
    Profile {
        seed,
        rate: 40,
        duplicate_pct: 12,
        reorder_window: 4,
        fault_plan: FaultPlan {
            malformed_every: NonZeroU32::new(50),
        },
    }
}

#[test]
fn same_seed_yields_byte_identical_streams() {
    for kind in [ProfileKind::ScanEvents, ProfileKind::SeedInventory] {
        let first = profile(0x5EED).canonical_stream_bytes(kind, COUNT);
        let second = profile(0x5EED).canonical_stream_bytes(kind, COUNT);

        assert_eq!(
            first, second,
            "{} is not byte-stable across two runs of one seed",
            kind.as_str()
        );
        assert!(
            !first.is_empty(),
            "{} produced an empty stream, so equality proves nothing",
            kind.as_str()
        );
    }
}

#[test]
fn a_different_seed_yields_a_different_stream() {
    let left = profile(0x5EED).canonical_stream_bytes(ProfileKind::ScanEvents, COUNT);
    let right = profile(0x5EEE).canonical_stream_bytes(ProfileKind::ScanEvents, COUNT);

    assert_ne!(
        left, right,
        "two seeds collapsed to one stream, so the seed is not wired through"
    );
}

/// The seed must reach GENERATION, not merely the duplicate and reorder draws.
///
/// Every knob is off here on purpose. With duplication and reordering on, two
/// seeds diverge through those stages alone, so a stream whose bodies and ids
/// ignored the seed entirely would still pass
/// [`a_different_seed_yields_a_different_stream`]. This is the test that
/// catches that mutant.
#[test]
fn the_seed_reaches_generated_ids_and_bodies() {
    for kind in [ProfileKind::ScanEvents, ProfileKind::SeedInventory] {
        let left: Vec<Event> = Profile::new(0x5EED, 40).stream(kind, 32).collect();
        let right: Vec<Event> = Profile::new(0x5EEE, 40).stream(kind, 32).collect();

        let left_ids: Vec<&str> = left.iter().map(|e| e.event_id.as_str()).collect();
        let right_ids: Vec<&str> = right.iter().map(|e| e.event_id.as_str()).collect();
        assert_ne!(left_ids, right_ids, "{}: ids ignore the seed", kind.as_str());

        let left_bodies: Vec<&serde_json::Value> = left.iter().map(|e| &e.body).collect();
        let right_bodies: Vec<&serde_json::Value> = right.iter().map(|e| &e.body).collect();
        assert_ne!(
            left_bodies,
            right_bodies,
            "{}: bodies ignore the seed",
            kind.as_str()
        );
    }
}

/// The knob split the spec ratified: `rate` is an emitter concern, so it must
/// not reach the bytes. If this fails, pacing has leaked into generation.
#[test]
fn rate_does_not_reach_the_stream() {
    let mut slow = profile(0x5EED);
    slow.rate = 1;
    let mut fast = profile(0x5EED);
    fast.rate = 10_000;

    assert_eq!(
        slow.canonical_stream_bytes(ProfileKind::ScanEvents, COUNT),
        fast.canonical_stream_bytes(ProfileKind::ScanEvents, COUNT),
        "rate changed the generated stream; wall-clock pacing must stay an emitter concern"
    );
}

/// A redelivery is the *same* event arriving twice, so it must carry the
/// original's identity — that is the whole point of a duplicate-rate knob.
#[test]
fn duplicates_repeat_an_identity_rather_than_minting_a_new_one() {
    let mut duplicating = Profile::new(0x5EED, 40);
    duplicating.duplicate_pct = 100;
    let events: Vec<Event> = duplicating.stream(ProfileKind::ScanEvents, 20).collect();

    assert_eq!(events.len(), 40, "every event should have been redelivered");

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for event in &events {
        *seen.entry(event.event_id.as_str()).or_default() += 1;
    }
    assert_eq!(seen.len(), 20, "duplicates minted fresh ids instead of repeating");
    assert!(
        seen.values().all(|count| *count == 2),
        "each identity should appear exactly twice"
    );
}

/// With the reorder window shut, sequence numbers arrive monotonically.
#[test]
fn a_closed_reorder_window_preserves_order() {
    let events: Vec<Event> = Profile::new(0x5EED, 40)
        .stream(ProfileKind::SeedInventory, 64)
        .collect();
    let sequences: Vec<u64> = events.iter().map(|event| event.sequence).collect();
    let mut sorted = sequences.clone();
    sorted.sort_unstable();

    assert_eq!(sequences, sorted, "a zero reorder window still displaced events");
}

/// An open reorder window must actually displace something, or the knob is
/// inert and the out-of-order tests downstream would pass vacuously.
#[test]
fn an_open_reorder_window_displaces_events() {
    let mut reordering = Profile::new(0x5EED, 40);
    reordering.reorder_window = 8;
    let sequences: Vec<u64> = reordering
        .stream(ProfileKind::ScanEvents, 64)
        .map(|event| event.sequence)
        .collect();
    let mut sorted = sequences.clone();
    sorted.sort_unstable();

    assert_eq!(sorted.len(), 64, "reordering must not drop or add events");
    assert_ne!(sequences, sorted, "the reorder window displaced nothing");
}

/// The in-stream clock advances with the sequence position, never the wall
/// clock — so a replay years later still produces the same timestamps.
#[test]
fn timestamps_are_generated_in_stream() {
    let events: Vec<Event> = Profile::new(0x5EED, 40)
        .stream(ProfileKind::ScanEvents, 8)
        .collect();

    let stamps: Vec<u64> = events.iter().map(|event| event.occurred_at_ms).collect();
    assert_eq!(
        stamps,
        (0..8).map(|i| 1_767_225_600_000 + i * 250).collect::<Vec<_>>(),
        "in-stream clock drifted from the fixed epoch and tick"
    );
}
