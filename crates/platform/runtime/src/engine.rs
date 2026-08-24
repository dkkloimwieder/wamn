//! Shared Wasmtime engine configuration for host and bench.

use std::sync::Arc;
use std::time::Duration;

use wash_runtime::engine::{Engine, WasmProposal};
use wash_runtime::sockets::policy::{EgressMode, SocketPolicy};
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
/// Both fields carry a flag: `--pool-slots` and `--pool-memory-cap-bytes`. The
/// cap needed one extra leg to be real (`wamn-t883`), because the platform
/// ceiling has a SECOND consumer — the fork's per-store ResourceLimiter reads
/// `WAMN_MEMORY_CEILING_MB`. Each serving binary therefore parses its arguments
/// in `main`, hands the parsed cap to [`advertise_memory_ceiling`], and only
/// then starts its Tokio runtime. Advertising ahead of the parse, as the
/// binaries used to, re-clamped a raised cap per store and left the knob
/// silently inert upward.
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

/// Advertise this host group's memory ceiling to the fork's per-store limiter.
///
/// Call this AFTER argument parsing and BEFORE the Tokio runtime starts: the
/// configured cap is what the limiter must clamp to, and `set_var` is sound
/// only while no other thread can read the process environment.
pub fn advertise_memory_ceiling(memory_cap_bytes: usize) {
    // SAFETY: callers uphold the documented single-threaded startup contract.
    unsafe {
        std::env::set_var(
            "WAMN_MEMORY_CEILING_MB",
            memory_ceiling_mb(memory_cap_bytes),
        );
    }
}

/// The ceiling as the fork's limiter reads it.
///
/// Split out from the write because the write lands in a process global no
/// test may read back without racing its siblings. The VALUE is chosen here,
/// so this is what a test pins.
fn memory_ceiling_mb(memory_cap_bytes: usize) -> String {
    (memory_cap_bytes >> 20).to_string()
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
    build_engine_with_socket_policy(proposals, host_socket_policy())
}

/// The host-level socket policy every wamn engine installs.
///
/// Upstream's `SocketPolicy::default()` carries [`EgressMode::Count`]: it
/// evaluates the policy, logs and counts what it WOULD refuse, then allows the
/// connection anyway, so that turning the gate on cannot sever a live host's
/// traffic. wamn takes [`EgressMode::Enforce`] instead (`wamn-0h0g.15.142`).
///
/// Count was never the mode most guests ran under. The fork's
/// `shape_socket_policy` empties the allowlist and forces `Enforce` for any
/// guest WITHOUT the `wamn.allow-raw-sockets` opt-in, and returns the policy
/// UNSHAPED for any guest with it. So the host-wide Count governed exactly the
/// guests granted the most reach, and for them the declared `allowed_hosts` was
/// advisory: evaluated, logged, allowed. Enforcing here makes the opt-in widen
/// the ALLOWLIST rather than also switch enforcement off.
///
/// Upstream's no-severed-traffic rationale does not transfer: no wamn guest
/// opts in today, and for every guest that does not, the shaped policy already
/// enforced. Nothing that is currently permitted becomes refused.
fn host_socket_policy() -> SocketPolicy {
    SocketPolicy {
        egress_mode: EgressMode::Enforce,
        ..SocketPolicy::default()
    }
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
    build_engine_inner(proposals, host_socket_policy(), sizing)
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use wash_runtime::sockets::{AddrDecision, DenyReason, SocketAddrUse};
    use wash_runtime::wasmtime::{Instance, Module, Store};

    use super::*;

    const PAGE: usize = 64 * 1024;

    /// wamn-0h0g.15.142: the host-level egress mode ENFORCES rather than counts.
    ///
    /// Under upstream's `EgressMode::Count` default this exact call is ALLOWED
    /// after logging what it would refuse, which is the whole defect: a guest
    /// carrying `wamn.allow-raw-sockets` gets its policy unshaped, so Count made
    /// its declared `allowed_hosts` a log line rather than a confinement.
    ///
    /// Driven through `decide` rather than by reading `egress_mode` back — the
    /// refusal is the property; the field is only how it is spelled.
    #[test]
    fn the_host_socket_policy_enforces_egress_rather_than_counting_it() {
        let unlisted = SocketAddr::from(([93, 184, 216, 34], 443));
        match host_socket_policy().decide(SocketAddrUse::TcpConnect, unlisted) {
            AddrDecision::Deny(DenyReason::NotPermitted) => {}
            AddrDecision::Deny(other) => {
                panic!("the refusal must come from the allowlist layer, not {other:?}")
            }
            AddrDecision::Allow(_) => panic!(
                "an address no allowlist entry grants must be REFUSED; allowing it is \
                 EgressMode::Count, which leaves an opted-in guest's allowed_hosts advisory"
            ),
        }
    }

    /// Bracket a cap one page either side: the module at the cap is admitted and
    /// the module one page over is refused.
    ///
    /// Only the pooling allocator enforces a per-memory cap at all — the
    /// on-demand default admits both — so this pins the engine to the pooling
    /// strategy AND to that exact ceiling. Dropping `with_pooling_config` fails
    /// the second half; moving the ceiling fails one half or the other.
    fn assert_memory_ceiling(engine: &Engine, cap_bytes: usize) {
        assert_eq!(
            cap_bytes % PAGE,
            0,
            "a pooling memory cap is a whole page count"
        );
        let pages = cap_bytes / PAGE;

        let at_ceiling = wat::parse_str(format!("(module (memory {pages}))"))
            .expect("encode a module at the ceiling");
        Module::new(engine.inner(), &at_ceiling)
            .expect("a memory of exactly the ceiling is admitted");

        let over_ceiling = wat::parse_str(format!("(module (memory {}))", pages + 1))
            .expect("encode a module one page over the ceiling");
        let rejection = Module::new(engine.inner(), &over_ceiling)
            .expect_err("one page over the ceiling is refused, which only pooling does");
        assert!(
            format!("{rejection:?}").contains("exceeds the limit"),
            "the refusal is the allocator's memory limit, not another compile error: {rejection:?}"
        );
    }

    /// wamn-8m4j: wamn-0h0g.17.13 deleted the only guard pinning
    /// `PoolingAllocationConfig::max_memory_size` to the platform ceiling along
    /// with the retired bespoke pool. Its subject is the SURVIVING native path,
    /// so its removal was a real gap rather than corpse coverage.
    #[test]
    fn the_production_engine_pools_at_the_default_platform_ceiling() {
        let engine = build_engine(&[]).expect("the production pooling engine");
        assert_memory_ceiling(&engine, PoolSizing::default().memory_cap_bytes);
    }

    /// wamn-0h0g.17.3 made the sizing configuration, so the restored guard pins
    /// the CONFIGURED value: an engine built with a different cap pools at that
    /// cap, which is what makes the knob more than a field nobody reads.
    #[test]
    fn a_configured_memory_cap_moves_the_pooling_ceiling() {
        let cap_bytes = 64 << 20;
        assert_ne!(
            cap_bytes,
            PoolSizing::default().memory_cap_bytes,
            "the configured cap must differ from the default or this proves nothing"
        );
        let engine = build_engine_sized(
            &[],
            PoolSizing {
                slots: 4,
                memory_cap_bytes: cap_bytes,
            },
        )
        .expect("a resized pooling engine");
        assert_memory_ceiling(&engine, cap_bytes);
    }

    /// wamn-t883: the ceiling handed to the fork's per-store ResourceLimiter is
    /// the CONFIGURED cap, not the compiled constant. Without this leg
    /// `--pool-memory-cap-bytes` would be upward-inert: the pooling allocator
    /// would admit the raised cap and the limiter would re-clamp every store
    /// back to 256 MiB.
    ///
    /// Pins the value [`advertise_memory_ceiling`] chooses rather than the
    /// environment entry it writes. `set_var` is a process global and cargo
    /// runs this suite multithreaded in one process, so reading the entry back
    /// would race every sibling test rather than prove anything.
    #[test]
    fn the_advertised_ceiling_follows_the_configured_cap() {
        assert_eq!(memory_ceiling_mb(64 << 20), "64");
        assert_eq!(
            memory_ceiling_mb(PoolSizing::default().memory_cap_bytes),
            "256",
            "the compiled default still advertises the 256 MiB platform ceiling"
        );
    }

    /// The other half of the sizing: slots bound CONCURRENCY, so a one-slot
    /// engine admits one live instance and refuses the second. Without this the
    /// slot count could be dropped on the floor and only the cap would notice.
    #[test]
    fn configured_slots_bound_concurrent_live_instances() {
        let engine = build_engine_sized(
            &[],
            PoolSizing {
                slots: 1,
                memory_cap_bytes: 1 << 20,
            },
        )
        .expect("a one-slot pooling engine");
        let wasm = wat::parse_str("(module (memory 1))").expect("encode a one-memory module");
        let module = Module::new(engine.inner(), &wasm).expect("one page fits the configured cap");

        let mut first = Store::new(engine.inner(), ());
        Instance::new(&mut first, &module, &[]).expect("the first instance takes the only slot");
        let mut second = Store::new(engine.inner(), ());
        let exhausted = Instance::new(&mut second, &module, &[])
            .expect_err("a second live instance exceeds the configured slot count");
        assert!(
            format!("{exhausted:?}").contains("limit of 1"),
            "the refusal is the configured slot count, not another instantiation error: \
             {exhausted:?}"
        );
    }

    /// A zero arrives from stringly configuration as a value, not as a parse
    /// error, and a zero-slot or zero-cap pool admits nothing at all.
    #[test]
    fn a_zero_sizing_refuses_to_build() {
        for sizing in [
            PoolSizing {
                slots: 0,
                ..PoolSizing::default()
            },
            PoolSizing {
                memory_cap_bytes: 0,
                ..PoolSizing::default()
            },
        ] {
            let error = build_engine_sized(&[], sizing).expect_err("a zero sizing must refuse");
            assert!(
                format!("{error}").starts_with("pooling allocator needs"),
                "the refusal must name the sizing it rejected: {error}"
            );
        }
    }
}
