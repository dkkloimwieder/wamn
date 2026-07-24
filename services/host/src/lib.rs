//! wamn-host: the production host library.
//!
//! The custom wasmCloud host image (embeds `wash_runtime::washlet`), the
//! shared trigger dispatcher, and the host plugins (`wamn:postgres`,
//! `wamn:logging`, the `wamn:node/control` stub). The thin `wamn-host`
//! binary (src/main.rs) exposes exactly these as subcommands; the one-shot
//! control-plane verbs live in `wamn-ctl` (SR9) and the gate suite lives
//! in the separate `wamn-gates` binary (docs/archive/structure-review.md SR1) —
//! both consume this library where they embed the runtime, so gates exercise
//! the identical host code they verify.

pub mod doubles;
pub mod egress_guard;
pub mod engine;
pub mod host;
pub mod memory_metrics;
pub mod plugins;
pub mod serve_node;

/// Advertise the platform memory ceiling to the fork's per-store limiter
/// (docs/wash-runtime-fork.md): a workload budget above this is a hard
/// store-creation error, never a silent clamp.
///
/// # Safety contract (upheld by callers)
/// Call before the tokio runtime exists — no other threads may be reading
/// the environment. Every engine-building binary (`wamn-host`, `wamn-gates`,
/// `wamn-run-worker`) calls this first thing in `main`.
pub fn advertise_memory_ceiling() {
    // SAFETY: single-threaded at this point per the function contract.
    unsafe {
        std::env::set_var(
            "WAMN_MEMORY_CEILING_MB",
            (engine::MEMORY_CAP_BYTES >> 20).to_string(),
        );
    }
}
