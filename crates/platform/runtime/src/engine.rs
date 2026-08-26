//! Shared Wasmtime engine configuration for host and bench.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

pub use wash_runtime::engine::host_memory::HostMemoryBudgets;
use wash_runtime::engine::{Engine, WasmProposal};
use wash_runtime::sockets::policy::{EgressMode, SocketPolicy};

/// Platform heap ceiling (S1 acceptance: 256 MiB, enforced).
///
/// This becomes `HostMemoryBudgets::default_heap_memory`, which wasmCloud
/// installs as the pooling allocator's `max_memory_size`. It bounds each
/// linear memory; WAMN intentionally has no second, fork-owned per-store
/// limiter.
pub const MEMORY_CAP_BYTES: usize = 256 << 20;

/// Default pooling-allocator core-instance budget for one host group.
///
/// Core instances are allocator capacity, not reusable WAMN instances. Guest
/// state remains per-invocation because workload admission keeps `pool_size`
/// at zero.
pub const DEFAULT_CORE_INSTANCES: u32 = 512;

/// Hard upper bound for one guest attempt and any host call it starts.
pub const MAX_HOST_CALL_DURATION: Duration = Duration::from_secs(30);

/// Pooling environment knobs that alter residency or decommit cadence without
/// changing allocator capacity.
const ALLOWED_POOLING_ENV: [&str; 5] = [
    "WASMTIME_POOLING_MAX_UNUSED_WASM_SLOTS",
    "WASMTIME_POOLING_DECOMMIT_BATCH_SIZE",
    "WASMTIME_POOLING_ASYNC_STACK_KEEP_RESIDENT",
    "WASMTIME_POOLING_LINEAR_MEMORY_KEEP_RESIDENT",
    "WASMTIME_POOLING_TABLE_KEEP_RESIDENT",
];

/// Build wasmCloud's native memory budgets from WAMN's two capacity knobs.
///
/// `max_guest_memory` remains wasmCloud's cgroup-aware derived advisory. WAMN
/// owns the per-memory ceiling and allocator instance count, which are the two
/// values it previously installed through a bespoke pooling configuration.
pub fn host_memory_budgets(
    memory_cap_bytes: usize,
    core_instances: u32,
) -> anyhow::Result<HostMemoryBudgets> {
    let default_heap_memory = u64::try_from(memory_cap_bytes)
        .map_err(|_| anyhow::anyhow!("pool memory cap does not fit in a u64 byte count"))?;
    HostMemoryBudgets::resolve(None, Some(default_heap_memory), Some(core_instances))
        .map_err(anyhow::Error::msg)
}

/// WAMN's compiled default host-memory configuration.
pub fn default_host_memory_budgets() -> HostMemoryBudgets {
    host_memory_budgets(MEMORY_CAP_BYTES, DEFAULT_CORE_INSTANCES)
        .expect("compiled host-memory defaults are nonzero and fit in u64")
}

/// Build the engine every WAMN host mode uses with the compiled native memory
/// budgets.
///
/// wasmCloud enables epoch interruption and owns its ticker. WAMN configures
/// only its manual stores' deadlines.
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
/// WAMN admission rejects tenant components importing `wasi:sockets`, while
/// host-owned guests use the same declared-host policy as every other guest.
/// Enforcing here therefore keeps the public policy authoritative without a
/// fork-only raw-socket opt-in.
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
    build_engine_inner(proposals, socket_policy, default_host_memory_budgets())
}

/// Build the platform engine with this host group's native memory budgets.
///
/// Serving deployables pass their resolved [`HostMemoryBudgets`]. Component
/// admission, benches, and proofs use [`build_engine`] and the compiled
/// defaults because their subject is the artifact, not host capacity.
pub fn build_engine_with_host_memory(
    proposals: &[WasmProposal],
    host_memory: HostMemoryBudgets,
) -> anyhow::Result<Engine> {
    build_engine_inner(proposals, host_socket_policy(), host_memory)
}

fn build_engine_inner(
    proposals: &[WasmProposal],
    socket_policy: SocketPolicy,
    host_memory: HostMemoryBudgets,
) -> anyhow::Result<Engine> {
    validate_pooling_capacity_environment(std::env::vars_os().map(|(key, _)| key))?;
    anyhow::ensure!(
        host_memory.max_guest_memory > 0,
        "max guest memory must be greater than zero"
    );
    anyhow::ensure!(
        host_memory.default_heap_memory > 0,
        "default heap memory must be greater than zero"
    );
    anyhow::ensure!(
        host_memory.core_instances > 0,
        "core instances must be greater than zero"
    );

    let mut builder = Engine::builder()
        .with_host_memory(host_memory)
        .with_pooling_allocator(true)
        .with_socket_policy(Arc::new(socket_policy));
    for proposal in proposals {
        builder = builder.with_wasm_proposal(*proposal);
    }
    builder.build()
}

/// Reject environment entries that can override WAMN's capacity budgets.
///
/// Unknown `WASMTIME_POOLING_*` entries are rejected too. A future wasmCloud
/// capacity knob must be reviewed before an operator can silently activate it;
/// only the known residency/decommit tuning knobs pass through.
fn validate_pooling_capacity_environment(
    keys: impl IntoIterator<Item = OsString>,
) -> anyhow::Result<()> {
    let mut rejected = keys
        .into_iter()
        .map(|key| key.to_string_lossy().into_owned())
        .filter(|key| {
            (key == "WASMTIME_POOLING" || key.starts_with("WASMTIME_POOLING_"))
                && !ALLOWED_POOLING_ENV.contains(&key.as_str())
        })
        .collect::<Vec<_>>();
    rejected.sort_unstable();
    rejected.dedup();
    anyhow::ensure!(
        rejected.is_empty(),
        "pooling capacity is configured through HostMemoryBudgets; remove environment override(s): {}",
        rejected.join(", ")
    );
    Ok(())
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
    /// Driven through `decide` rather than by reading `egress_mode` back — the
    /// refusal across every raw-egress operation is the property; the field is
    /// only how it is spelled.
    #[test]
    fn the_host_socket_policy_enforces_egress_rather_than_counting_it() {
        let unlisted = SocketAddr::from(([93, 184, 216, 34], 443));
        let policy = host_socket_policy();
        for operation in [
            SocketAddrUse::TcpConnect,
            SocketAddrUse::UdpConnect,
            SocketAddrUse::UdpOutgoingDatagram,
        ] {
            match policy.decide(operation, unlisted) {
                AddrDecision::Deny(DenyReason::NotPermitted) => {}
                AddrDecision::Deny(other) => {
                    panic!(
                        "the {operation:?} refusal must come from the allowlist layer, not \
                         {other:?}"
                    )
                }
                AddrDecision::Allow(_) => panic!(
                    "an address no allowlist entry grants must be refused for {operation:?}; \
                     allowing it is EgressMode::Count"
                ),
            }
        }
    }

    /// Bracket a cap one page either side: the module at the cap is admitted and
    /// the module one page over is refused.
    ///
    /// Only the pooling allocator enforces a per-memory cap at all — the
    /// on-demand default admits both — so this pins the engine to the pooling
    /// strategy and to the native host-memory ceiling.
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

    /// Pin both parts of the surviving native path: the pooling allocator is
    /// active, and `HostMemoryBudgets` supplies the platform ceiling it uses.
    #[test]
    fn the_production_engine_pools_at_the_default_platform_ceiling() {
        let engine = build_engine(&[]).expect("the production pooling engine");
        assert_memory_ceiling(&engine, MEMORY_CAP_BYTES);
        assert_eq!(engine.host_memory(), default_host_memory_budgets());
        assert_eq!(engine.total_core_instances(), Some(DEFAULT_CORE_INSTANCES));
    }

    /// wamn-0h0g.17.3 made the sizing configuration, so the restored guard pins
    /// the CONFIGURED value: an engine built with a different cap pools at that
    /// cap, which is what makes the knob more than a field nobody reads.
    #[test]
    fn a_configured_memory_cap_moves_the_pooling_ceiling() {
        let cap_bytes = 64 << 20;
        assert_ne!(
            cap_bytes, MEMORY_CAP_BYTES,
            "the configured cap must differ from the default or this proves nothing"
        );
        let budgets = host_memory_budgets(cap_bytes, 4).expect("configured native budgets");
        let engine = build_engine_with_host_memory(&[], budgets).expect("a resized pooling engine");
        assert_memory_ceiling(&engine, cap_bytes);
        assert_eq!(engine.host_memory(), budgets);
        assert_eq!(engine.total_core_instances(), Some(4));
    }

    /// The other half of the sizing: slots bound CONCURRENCY, so a one-slot
    /// engine admits one live instance and refuses the second. Without this the
    /// slot count could be dropped on the floor and only the cap would notice.
    #[test]
    fn configured_slots_bound_concurrent_live_instances() {
        let budgets = host_memory_budgets(1 << 20, 1).expect("one-slot native budgets");
        let engine =
            build_engine_with_host_memory(&[], budgets).expect("a one-slot pooling engine");
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

    /// Invalid native budgets fail before wasmCloud or Wasmtime can interpret
    /// zero as allocator capacity.
    #[test]
    fn zero_native_budgets_are_refused() {
        let zero_heap = host_memory_budgets(0, 1).expect_err("a zero heap must be refused");
        assert!(zero_heap.to_string().contains("default-heap-memory"));

        let zero_instances =
            host_memory_budgets(1 << 20, 0).expect_err("zero instances must be refused");
        assert!(zero_instances.to_string().contains("core-instances"));

        let invalid = HostMemoryBudgets {
            max_guest_memory: 0,
            ..default_host_memory_budgets()
        };
        let error = build_engine_with_host_memory(&[], invalid)
            .expect_err("a direct invalid native budget must be refused");
        assert_eq!(
            error.to_string(),
            "max guest memory must be greater than zero"
        );
    }

    #[test]
    fn residency_and_decommit_environment_knobs_are_permitted() {
        let keys = ALLOWED_POOLING_ENV.map(OsString::from);
        validate_pooling_capacity_environment(keys)
            .expect("residency and decommit knobs do not alter capacity");
    }

    #[test]
    fn capacity_and_unknown_pooling_environment_knobs_fail_closed() {
        let error = validate_pooling_capacity_environment([
            OsString::from("PATH"),
            OsString::from("WASMTIME_POOLING"),
            OsString::from("WASMTIME_POOLING_TOTAL_CORE_INSTANCES"),
            OsString::from("WASMTIME_POOLING_MAX_MEMORY_SIZE"),
            OsString::from("WASMTIME_POOLING_UNREVIEWED"),
        ])
        .expect_err("capacity and unknown pooling knobs must be rejected");
        assert_eq!(
            error.to_string(),
            "pooling capacity is configured through HostMemoryBudgets; remove environment \
             override(s): WASMTIME_POOLING, WASMTIME_POOLING_MAX_MEMORY_SIZE, \
             WASMTIME_POOLING_TOTAL_CORE_INSTANCES, WASMTIME_POOLING_UNREVIEWED"
        );
    }
}
