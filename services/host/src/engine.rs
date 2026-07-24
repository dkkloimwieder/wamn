//! Shared Wasmtime engine configuration for host and bench.

use std::time::Duration;

use wash_runtime::engine::{Engine, WasmProposal};
use wash_runtime::wasmtime::{Config, PoolingAllocationConfig};

/// Platform memory ceiling (S1 acceptance: 256 MiB, enforced): the pooling
/// allocator's engine-wide `max_memory_size`, the largest budget any
/// component may hold. Per-component budgets (`memory_limit_mb` /
/// `wamn.memory-limit-mb`) are enforced BELOW this by the fork's per-store
/// ResourceLimiter (docs/wash-runtime-fork.md).
pub const MEMORY_CAP_BYTES: usize = 256 << 20;

/// Pooling-allocator slot counts. Slots are per *live instance* (stores are
/// created per invocation), not per resident workload, so this bounds
/// concurrency, not density.
const POOL_SLOTS: u32 = 512;

/// Default epoch tick period. One tick = one deadline unit, so a store
/// deadline of N ticks caps guest execution at roughly N × 10 ms.
pub const DEFAULT_EPOCH_TICK: Duration = Duration::from_millis(10);

/// Build the engine every wamn-host mode uses: pooling allocator with the
/// 256 MiB memory ceiling, epoch interruption enabled. Memory enforcement is
/// two-tier: this pooling cap is the platform ceiling, and the fork's
/// per-store ResourceLimiter enforces per-component budgets below it
/// (carried commit, docs/wash-runtime-fork.md; bench phase 5 is the gate).
/// Epoch interruption is our hard-cancellation layer: [`spawn_epoch_ticker`]
/// advances the epoch and the carried epoch commit gives every store a
/// deadline (`wamn.epoch-deadline-ticks` config / WAMN_EPOCH_DEADLINE_TICKS
/// env).
pub fn build_engine(proposals: &[WasmProposal]) -> anyhow::Result<Engine> {
    let mut pooling = PoolingAllocationConfig::default();
    pooling.max_memory_size(MEMORY_CAP_BYTES);
    pooling.total_memories(POOL_SLOTS);
    pooling.total_tables(POOL_SLOTS);
    pooling.total_component_instances(POOL_SLOTS);
    pooling.total_stacks(POOL_SLOTS);

    let mut base = Config::new();
    base.epoch_interruption(true);

    // with_config sets the *base*; pooling and proposals layer on top.
    let mut builder = Engine::builder()
        .with_config(base)
        .with_pooling_config(pooling);
    for proposal in proposals {
        builder = builder.with_wasm_proposal(*proposal);
    }
    builder.build()
}

/// Advance the engine epoch every `period` forever. Stores trap once the
/// epoch passes their deadline; without a ticker the epoch never moves and
/// deadlines never fire.
pub fn spawn_epoch_ticker(engine: &Engine, period: Duration) -> tokio::task::JoinHandle<()> {
    let engine = engine.inner().clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            engine.increment_epoch();
        }
    })
}
