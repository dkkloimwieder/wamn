//! Deterministic, seeded event streams for the POC application portfolio.
//!
//! # The law this crate exists under
//!
//! **Simulators drive real routes and consumers. They never write the
//! database.** That is `docs/poc/poc-application-portfolio.md`'s rule for every
//! portfolio driver, and it is restated here because this crate's nearest
//! neighbour breaks it on purpose: [`wamn_gate_harness`] seeds Postgres
//! directly (`scope_session`, `seed_flow_version`) so that gates can stand up a
//! fixture. A reviewer who reads the two crates side by side will conflate
//! them unless the line is drawn out loud. The harness prepares state; this
//! crate produces *traffic*.
//!
//! [`wamn_gate_harness`]: https://docs.rs/wamn-gate-harness
//!
//! # Determinism
//!
//! A [`Profile`] plus an event count is a total function to a byte-identical
//! stream: ids and timestamps are generated **in-stream** from the seed, never
//! from the wall clock or a random source. [`Profile::canonical_stream_bytes`]
//! is the comparable form, and it goes through
//! [`wamn_execution_contract::canonical_json_bytes`] rather than
//! `serde_json::to_vec` so a simulator stream is comparable with every other
//! digest in the platform.
//!
//! `rate` is deliberately **not** applied here. It is carried on the profile
//! and consumed by an emitter; wall-clock pacing is an emission concern, so the
//! stream stays a pure function of `(seed, count, knobs)`.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_execution_contract::canonical_json_bytes;

pub mod emit;
pub mod profiles;

pub use emit::{EmissionTarget, HttpRouteTarget, ItemOutcome};
pub use profiles::ProfileKind;

/// First timestamp in every stream, as epoch milliseconds (2026-01-01T00:00:00Z).
const EPOCH_MS: u64 = 1_767_225_600_000;

/// In-stream clock step between consecutive sequence numbers, milliseconds.
///
/// Fixed, and deliberately unrelated to [`Profile::rate`]: coupling the two
/// would smuggle an emission concern into the stream and make the same seed
/// produce different bytes at a different rate.
const TICK_MS: u64 = 250;

/// Deterministic fault injection. A `None` field injects nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaultPlan {
    /// Replace every Nth event's body with a payload the route must refuse.
    pub malformed_every: Option<NonZeroU32>,
}

/// One driver configuration. The same profile and count always yield the same
/// bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Seeds every generated id, body value, duplicate draw and reorder swap.
    pub seed: u64,
    /// Events per second an emitter should pace at. Carried, never applied by
    /// the stream — see the crate docs.
    pub rate: u32,
    /// Percentage of events re-emitted immediately after their original, as an
    /// at-least-once redelivery. `0` emits each event once.
    pub duplicate_pct: u8,
    /// Maximum distance an event may be displaced from its sequence position.
    /// `0` leaves the stream in order.
    pub reorder_window: usize,
    /// Deterministic fault injection.
    pub fault_plan: FaultPlan,
}

impl Profile {
    /// A profile with every knob off: in order, no duplicates, no faults.
    #[must_use]
    pub const fn new(seed: u64, rate: u32) -> Self {
        Self {
            seed,
            rate,
            duplicate_pct: 0,
            reorder_window: 0,
            fault_plan: FaultPlan {
                malformed_every: None,
            },
        }
    }

    /// The stream this profile describes, in emission order.
    ///
    /// Transforms apply in a fixed order — generate, fault, duplicate,
    /// reorder — because a different order is a different stream, and the
    /// byte-identity guarantee is only meaningful if the order is pinned.
    pub fn stream(&self, kind: ProfileKind, count: usize) -> impl Iterator<Item = Event> + use<> {
        let mut events = self.generate(kind, count);
        self.inject_faults(&mut events);
        let events = self.inject_duplicates(events);
        let events = self.reorder(events);
        events.into_iter()
    }

    /// Canonical bytes for the whole stream — the comparable form.
    ///
    /// This is what a determinism gate compares: two runs of the same profile
    /// must produce identical bytes here.
    #[must_use]
    pub fn canonical_stream_bytes(&self, kind: ProfileKind, count: usize) -> Vec<u8> {
        let events: Vec<Event> = self.stream(kind, count).collect();
        canonical_json_bytes(&serde_json::to_value(&events).expect("Event always serializes"))
    }

    fn generate(&self, kind: ProfileKind, count: usize) -> Vec<Event> {
        (0..count)
            .map(|index| {
                let sequence = index as u64;
                let mut lcg = Lcg::new(self.seed ^ sequence.wrapping_mul(0x9E37_79B9));
                Event {
                    sequence,
                    event_id: lcg.hex_id(),
                    occurred_at_ms: EPOCH_MS + sequence * TICK_MS,
                    kind: kind.as_str(),
                    body: kind.body(&mut lcg, sequence),
                }
            })
            .collect()
    }

    fn inject_faults(&self, events: &mut [Event]) {
        let Some(every) = self.fault_plan.malformed_every else {
            return;
        };
        let every = every.get() as usize;
        for event in events.iter_mut().skip(every - 1).step_by(every) {
            event.body = Value::String("malformed".to_owned());
        }
    }

    fn inject_duplicates(&self, events: Vec<Event>) -> Vec<Event> {
        if self.duplicate_pct == 0 {
            return events;
        }
        let mut lcg = Lcg::new(self.seed ^ 0x00D0_D0D0_u64);
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            let redeliver = lcg.below(100) < u64::from(self.duplicate_pct);
            out.push(event.clone());
            if redeliver {
                out.push(event);
            }
        }
        out
    }

    fn reorder(&self, mut events: Vec<Event>) -> Vec<Event> {
        if self.reorder_window == 0 || events.len() < 2 {
            return events;
        }
        let mut lcg = Lcg::new(self.seed ^ 0x5EED_5A17_u64);
        for index in 0..events.len() {
            let window =
                u64::try_from(self.reorder_window).expect("reorder window fits in u64") + 1;
            let offset =
                usize::try_from(lcg.below(window)).expect("a draw below the window fits in usize");
            let target = index + offset;
            if target < events.len() {
                events.swap(index, target);
            }
        }
        events
    }
}

/// One generated event. Every field is a pure function of the profile seed and
/// the sequence number.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Event {
    /// Position in the generated stream, before duplication and reordering.
    pub sequence: u64,
    /// Deterministic identity. A duplicate carries its original's id, which is
    /// what makes it a redelivery rather than a second event.
    pub event_id: String,
    /// In-stream clock, epoch milliseconds. A reordered event keeps the
    /// timestamp of its sequence position, so out-of-order arrival is visible.
    pub occurred_at_ms: u64,
    /// The profile that produced this event.
    pub kind: &'static str,
    /// Profile-specific payload.
    pub body: Value,
}

/// The seeded generator. An LCG rather than `rand`, matching the existing
/// deterministic-content precedent in `walbench`'s `wide_blob`: a reproducible
/// record is the point, and a dependency on a random source would defeat it.
pub(crate) struct Lcg {
    state: u64,
}

impl Lcg {
    pub(crate) const fn new(seed: u64) -> Self {
        Self {
            state: seed
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(0x1234_5678_9ABC_DEF1),
        }
    }

    pub(crate) const fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state >> 33
    }

    /// Uniform-enough draw in `0..bound`. `bound` must be non-zero.
    pub(crate) const fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// A 32-character lowercase hex identity.
    pub(crate) fn hex_id(&mut self) -> String {
        let high = self.next();
        let low = self.next();
        format!("{high:016x}{low:016x}")
    }
}
