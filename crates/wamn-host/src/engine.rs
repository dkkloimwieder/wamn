//! Shared Wasmtime engine configuration for host and bench.

use wash_runtime::engine::{Engine, WasmProposal};
use wash_runtime::wasmtime::PoolingAllocationConfig;

/// Per-component linear-memory cap (S1 acceptance: 256 MiB, enforced).
pub const MEMORY_CAP_BYTES: usize = 256 << 20;

/// Pooling-allocator slot counts. Slots are per *live instance* (stores are
/// created per invocation), not per resident workload, so this bounds
/// concurrency, not density.
const POOL_SLOTS: u32 = 512;

/// Build the engine every wamn-host mode uses: pooling allocator with the
/// 256 MiB per-memory cap. wash-runtime wires no ResourceLimiter or epoch
/// interruption (upstream gap, recorded in p0-results), so this pooling cap
/// is the only memory-enforcement mechanism available without a fork.
pub fn build_engine(proposals: &[WasmProposal]) -> anyhow::Result<Engine> {
    let mut pooling = PoolingAllocationConfig::default();
    pooling.max_memory_size(MEMORY_CAP_BYTES);
    pooling.total_memories(POOL_SLOTS);
    pooling.total_tables(POOL_SLOTS);
    pooling.total_component_instances(POOL_SLOTS);
    pooling.total_stacks(POOL_SLOTS);

    let mut builder = Engine::builder().with_pooling_config(pooling);
    for proposal in proposals {
        builder = builder.with_wasm_proposal(*proposal);
    }
    builder.build()
}
