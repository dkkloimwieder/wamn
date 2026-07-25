//! Deterministic capability adapters for product scenario execution.
//!
//! These product adapters run the same flowrunner component as serving, with
//! virtual time, seeded randomness, recorded egress, and isolated schemas:
//!
//! - [`ScenarioClock`] / [`VirtualWallClock`] — one absolute virtual instant for
//!   every scenario scheduling comparison.
//! - [`DatabaseClockBoundary`] — the one-way boundary from a logical due
//!   decision to a claimable PostgreSQL release marker.
//! - [`ScenarioScheduler`] — advance the virtual clock to the next parked-wake
//!   deadline and re-drive, collapsing arbitrary delays (delta 2).
//! - [`RecordingEgress`] — record every outbound request + per-flow allowlist +
//!   assertion surface (delta 3).
//! - [`EphemeralSchemaProvisioner`] / [`case_pool`] — an isolated schema and app
//!   pool per scenario.
//! - [`SeededRng`] / [`build_virtual_wasi`] — a deterministic `wasi:random`
//!   seed adapter (a forward hook; no guest consumes randomness yet).
//!
//! ## Injection seam
//!
//! The scenario-worker injects [`ScenarioCapabilities`] through the shared
//! execution-host seam. The serving executor has no dependency on this crate
//! and cannot select these adapters at runtime.

mod clock;
mod credentials;
mod egress;
mod random;
mod scheduler;
mod schema;

use std::sync::Arc;

use wash_runtime::host::http::HostHandler;
use wasmtime_wasi::WasiCtx;

pub use clock::{DatabaseClockBoundary, ScenarioClock, VirtualWallClock};
pub use credentials::{ScenarioCredentials, load_scenario_credentials};
pub use egress::{EgressObservation, RecordingEgress};
pub use random::{SeededRng, build_virtual_wasi};
pub use scheduler::{
    QueueScheduleShiftError, RUN_QUEUE_DUE_NUDGE_SQL, RUN_QUEUE_NEXT_WAKE_SQL,
    RUN_S6_WAKE_DEADLINES_SQL, ScenarioScheduler, SchedulerBackend, validate_queue_due_nudge,
};
pub use schema::{
    EphemeralSchemaProvisioner, InvalidScenarioSchemaName, ScenarioSchemaName, case_pool,
};

/// Deterministic capabilities consumed by one scenario execution store.
///
/// The caller retains handles to [`ScenarioClock`] and [`RecordingEgress`] before
/// moving this value into the shared execution host.
pub struct ScenarioCapabilities {
    /// The custom `WasiCtx` the store gets (virtual clock + seeded random).
    pub wasi: WasiCtx,
    /// The store's outbound-HTTP handler (the egress recorder).
    pub egress: Arc<dyn HostHandler>,
}

impl std::fmt::Debug for ScenarioCapabilities {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScenarioCapabilities")
            .finish_non_exhaustive()
    }
}

impl ScenarioCapabilities {
    /// Assemble virtual scenario capabilities: a wall clock based at
    /// `epoch_secs`, `wasi:random` seeded with `seed`, and `egress` as the
    /// store's HTTP handler. Returns the set plus the shared [`ScenarioClock`]
    /// the caller drives (via a [`ScenarioScheduler`]). `egress` is typically
    /// an `Arc<RecordingEgress>` the caller also holds for audit.
    pub fn virtualized(
        epoch_secs: u64,
        seed: u64,
        egress: Arc<dyn HostHandler>,
    ) -> (Self, ScenarioClock) {
        let clock = ScenarioClock::at_secs(epoch_secs);
        let wasi = build_virtual_wasi(&clock, seed);
        (Self { wasi, egress }, clock)
    }
}
