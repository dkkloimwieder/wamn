//! Shared Wasmtime engine configuration for host and bench.

use std::sync::Arc;
use std::time::Duration;

use wash_runtime::engine::{Engine, WasmProposal};
use wash_runtime::sockets::policy::SocketPolicy;
use wash_runtime::wasmtime::{Config, PoolingAllocationConfig};

/// Platform memory ceiling (S1 acceptance: 256 MiB, enforced): the pooling
/// allocator's engine-wide `max_memory_size`, the largest budget any
/// component may hold. Per-component budgets (`memory_limit_mb` /
/// `wamn.memory-limit-mb`) are enforced BELOW this by the fork's per-store
/// ResourceLimiter (docs/archive/platform/wash-runtime-fork.md).
pub const MEMORY_CAP_BYTES: usize = 256 << 20;

/// Pooling-allocator slot counts. Slots are per *live instance* (stores are
/// created per invocation), not per resident workload, so this bounds
/// concurrency, not density.
const POOL_SLOTS: u32 = 512;

/// Pooling-allocator sizing for one host group.
///
/// Written key-shaped on purpose (`wamn-0h0g.17.3`): the resting answer is a
/// first-class chart key pair, `runtime.pool.slots` and
/// `runtime.pool.memoryCapBytes`, and the interim carrier is a per-host-group
/// `extraArgs` CLI flag. Naming the fields after the eventual keys makes that
/// migration a values rewire rather than a code change.
///
/// `memory_cap_bytes` has no flag yet, and that gap is deliberate rather than
/// unfinished. The platform ceiling has a SECOND consumer: the fork's per-store
/// ResourceLimiter reads `WAMN_MEMORY_CEILING_MB`, which
/// [`advertise_memory_ceiling`] must set before the Tokio runtime exists — i.e.
/// in each binary's `main`, ahead of argument parsing. Until that leg moves, a
/// cap raised here would be re-clamped per store and the flag would silently do
/// nothing upward, which is the exact invisible-red class the interim carrier
/// was chosen to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSizing {
    /// Concurrent live instances this engine admits.
    pub slots: u32,
    /// Largest memory any one instance may hold.
    pub memory_cap_bytes: usize,
}

impl Default for PoolSizing {
    fn default() -> Self {
        Self {
            slots: POOL_SLOTS,
            memory_cap_bytes: MEMORY_CAP_BYTES,
        }
    }
}

/// Default epoch tick period. One tick = one deadline unit, so a store
/// deadline of N ticks caps guest execution at roughly N × 10 ms.
pub const DEFAULT_EPOCH_TICK: Duration = Duration::from_millis(10);

/// Hard upper bound for one guest attempt and any host call it starts.
pub const MAX_HOST_CALL_DURATION: Duration = Duration::from_secs(30);

/// Advertise the platform memory ceiling to the fork's per-store limiter.
///
/// Call this before the Tokio runtime starts, while no other thread can read
/// the process environment.
pub fn advertise_memory_ceiling() {
    // SAFETY: callers uphold the documented single-threaded startup contract.
    unsafe {
        std::env::set_var(
            "WAMN_MEMORY_CEILING_MB",
            (MEMORY_CAP_BYTES >> 20).to_string(),
        );
    }
}

/// Build the engine every wamn-host mode uses: pooling allocator with the
/// 256 MiB memory ceiling, epoch interruption enabled. Memory enforcement is
/// two-tier: this pooling cap is the platform ceiling, and the fork's
/// per-store ResourceLimiter enforces per-component budgets below it
/// (carried commit, docs/archive/platform/wash-runtime-fork.md; bench phase 5 is the gate).
/// Epoch interruption is our hard-cancellation layer: [`spawn_epoch_ticker`]
/// advances the epoch and the carried epoch commit gives every store a
/// deadline (`wamn.epoch-deadline-ticks` config / WAMN_EPOCH_DEADLINE_TICKS
/// env).
pub fn build_engine(proposals: &[WasmProposal]) -> anyhow::Result<Engine> {
    build_engine_with_socket_policy(proposals, SocketPolicy::default())
}

/// Build the platform engine with an explicit host-level socket policy.
pub fn build_engine_with_socket_policy(
    proposals: &[WasmProposal],
    socket_policy: SocketPolicy,
) -> anyhow::Result<Engine> {
    build_engine_inner(proposals, socket_policy, PoolSizing::default())
}

/// Build the platform engine with this host group's pooling sizing.
///
/// Only the two serving deployables pass a sizing: `wamn-host` and the executor.
/// Every other builder — component admission, benches, proofs — keeps the
/// compiled defaults, because their subject is the artifact, not the capacity.
pub fn build_engine_sized(
    proposals: &[WasmProposal],
    sizing: PoolSizing,
) -> anyhow::Result<Engine> {
    build_engine_inner(proposals, SocketPolicy::default(), sizing)
}

fn build_engine_inner(
    proposals: &[WasmProposal],
    socket_policy: SocketPolicy,
    sizing: PoolSizing,
) -> anyhow::Result<Engine> {
    // Sizing is stringly configuration until the chart keys land, so a zero
    // arrives here as a value rather than as a typo the parser could catch: a
    // zero-slot pool admits no instance at all and a zero cap admits no memory.
    anyhow::ensure!(
        sizing.slots > 0,
        "pooling allocator needs at least one slot"
    );
    anyhow::ensure!(
        sizing.memory_cap_bytes > 0,
        "pooling allocator needs a nonzero memory cap"
    );
    let mut pooling = PoolingAllocationConfig::default();
    pooling.max_memory_size(sizing.memory_cap_bytes);
    pooling.total_memories(sizing.slots);
    pooling.total_tables(sizing.slots);
    pooling.total_component_instances(sizing.slots);
    pooling.total_stacks(sizing.slots);

    let mut base = Config::new();
    base.epoch_interruption(true);

    // with_config sets the *base*; pooling and proposals layer on top.
    let mut builder = Engine::builder()
        .with_config(base)
        .with_pooling_config(pooling)
        .with_socket_policy(Arc::new(socket_policy));
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
