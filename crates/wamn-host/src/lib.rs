//! wamn-host: the production host library.
//!
//! The custom wasmCloud host image (embeds `wash_runtime::washlet`), the
//! shared trigger dispatcher, the host plugins (`wamn:postgres`,
//! `wamn:logging`, the `wamn:node/control` stub), and the project
//! provisioning tool (`publish-catalog`). The thin `wamn-host` binary
//! (src/main.rs) exposes exactly these as subcommands; the gate suite lives
//! in the separate `wamn-gates` binary (docs/structure-review.md SR1) and
//! consumes this library so gates exercise the identical host code they
//! verify.

pub mod dispatch;
pub mod dump_project_env;
pub mod engine;
pub mod host;
pub mod migrate_catalog;
pub mod move_org_tier;
pub mod plugins;
pub mod provision;
pub mod provision_org;
pub mod provision_project_env;
pub mod publish_catalog;
pub mod restore_project_env;

/// Advertise the platform memory ceiling to the fork's per-store limiter
/// (docs/wash-runtime-fork.md): a workload budget above this is a hard
/// store-creation error, never a silent clamp.
///
/// # Safety contract (upheld by callers)
/// Call before the tokio runtime exists — no other threads may be reading
/// the environment. Both binaries call this first thing in `main`.
pub fn advertise_memory_ceiling() {
    // SAFETY: single-threaded at this point per the function contract.
    unsafe {
        std::env::set_var(
            "WAMN_MEMORY_CEILING_MB",
            (engine::MEMORY_CAP_BYTES >> 20).to_string(),
        );
    }
}
