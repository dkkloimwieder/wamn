//! Test-host doubles: the reusable mock-at-the-capability-boundary machinery
//! (design-note 9, S6 productionization, bead wamn-t92).
//!
//! S6 proved the mock-at-capability-boundary thesis with doubles wired inside
//! the `testhostbench` gate. This module extracts them into real, reusable
//! test-host machinery the production `wamn-run-worker` can select at build/config
//! time (the `--test-doubles` selector), so "run the SAME flow binary under a
//! prod host and a test host, differing only in injected capabilities" is a
//! property of the production runner, not of a bench:
//!
//! - [`VirtualClock`] / [`VirtualWallClock`] — the wall clock a test scheduler
//!   drives (delta 2).
//! - [`TestScheduler`] — advance the virtual clock to the next parked-wake
//!   deadline and re-drive, collapsing arbitrary delays (delta 2).
//! - [`EgressRecorder`] — record every outbound request + per-flow allowlist +
//!   assertion surface (delta 3).
//! - [`EphemeralSchemaProvisioner`] / [`case_pool`] — a fresh schema + app pool
//!   per test case (delta 4).
//! - [`SeededRng`] / [`build_virtual_wasi`] — a deterministic `wasi:random`
//!   seed double (delta 5; a forward hook — no guest consumes randomness yet).
//!
//! ## The injection seam is the run-worker store build, NOT the washlet host
//!
//! Injecting a virtual clock (and the seeded random) requires control over the
//! per-workload `WasiCtx`. The washlet `ClusterHost` path
//! (`crates/wamn-host/src/host.rs`) does NOT expose per-workload `WasiCtx` to
//! `wamn-host`, so a virtual clock CANNOT be injected there. The only production
//! path that hand-builds the store — and can therefore call
//! `CtxBuilder::with_wasi_ctx` — is `wamn_run_worker::RunWorker::instantiate`.
//! That is where a [`DoubleSet`] is threaded in. (A washlet-path test host is a
//! separate, larger piece of work — see the lane report deferrals.)

mod clock;
mod egress;
mod random;
mod scheduler;
mod schema;

use std::sync::Arc;

use wash_runtime::host::http::HostHandler;
use wasmtime_wasi::WasiCtx;

pub use clock::{VirtualClock, VirtualWallClock};
pub use egress::{EgressRecord, EgressRecorder};
pub use random::{SeededRng, build_virtual_wasi};
pub use scheduler::{
    RUN_QUEUE_DUE_NUDGE_SQL, RUN_QUEUE_NEXT_WAKE_SQL, RUN_S6_WAKE_DEADLINES_SQL, SchedulerBackend,
    TestScheduler,
};
pub use schema::{EphemeralSchemaProvisioner, case_pool};

/// The capabilities the test host swaps into a run-worker store: a custom
/// `WasiCtx` (virtual wall clock + deterministic seeded random) and the egress
/// handler (an [`EgressRecorder`]). `RunWorker::instantiate` takes an
/// `Option<DoubleSet>`; `Some` selects the test host, `None` the prod host.
///
/// The caller keeps its own handles to the [`VirtualClock`] (to drive) and the
/// [`EgressRecorder`] (to audit) BEFORE moving the set into `instantiate` — the
/// set carries only what the store consumes.
pub struct DoubleSet {
    /// The custom `WasiCtx` the store gets (virtual clock + seeded random).
    pub wasi: WasiCtx,
    /// The store's outbound-HTTP handler (the egress recorder).
    pub egress: Arc<dyn HostHandler>,
}

impl DoubleSet {
    /// Assemble a "virtual" test-host double set: a virtual wall clock based at
    /// `epoch_secs`, `wasi:random` seeded with `seed`, and `egress` as the
    /// store's HTTP handler. Returns the set plus the shared [`VirtualClock`]
    /// the caller drives (via a [`TestScheduler`]). `egress` is typically an
    /// `Arc<EgressRecorder>` the caller also holds for audit.
    pub fn virtual_host(
        epoch_secs: u64,
        seed: u64,
        egress: Arc<dyn HostHandler>,
    ) -> (Self, VirtualClock) {
        let clock = VirtualClock::at_secs(epoch_secs);
        let wasi = build_virtual_wasi(&clock, seed);
        (Self { wasi, egress }, clock)
    }
}
