//! Shared Wasmtime engine configuration for host and bench.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use wash_runtime::engine::guest_memory::GuestMemoryMode;
pub use wash_runtime::engine::host_memory::HostMemoryBudgets;
use wash_runtime::engine::{Engine, WasmProposal};
use wash_runtime::host::egress_policy::EgressAddressPolicy;
use wash_runtime::sockets::policy::{EgressMode, SocketPolicy};

/// Platform heap ceiling (S1 acceptance: 256 MiB, enforced).
///
/// This becomes `HostMemoryBudgets::default_heap_memory`, which wasmCloud
/// installs as the pooling allocator's `max_memory_size`. It bounds each
/// linear memory. Native store limiters also charge the shared guest budget.
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
///
/// Enforce activates BOTH egress layers, and wamn narrows the second one
/// (`wamn-d0w4`). Upstream's [`EgressAddressPolicy::default`] already denies
/// loopback, link-local — the IPv4 metadata address with it — the IPv6
/// metadata address, unspecified, multicast, and documentation ranges, but it
/// PERMITS private ranges, because reaching a sibling service on a private
/// address is the ordinary wasmCloud case. It is not the ordinary wamn case: a
/// guest reaches Postgres through `wamn:postgres` and HTTP through
/// `wamn:connection/http`, so a raw socket to the cluster service network is
/// the Kubernetes API, not a sibling. Denying private ranges makes the address
/// layer a default-deny FLOOR and leaves a guest's declared `allowed_hosts` an
/// opt-in on top of it rather than the sole confinement. Both layers judge
/// `to_canonical()`, so `::ffff:10.0.0.1` is refused as `10.0.0.1`.
fn host_socket_policy() -> SocketPolicy {
    SocketPolicy {
        egress_mode: EgressMode::Enforce,
        egress_addrs: EgressAddressPolicy {
            deny_special: true,
            allow_private: false,
        },
        ..SocketPolicy::default()
    }
}

/// Build the platform engine with an explicit host-level socket policy.
pub fn build_engine_with_socket_policy(
    proposals: &[WasmProposal],
    socket_policy: SocketPolicy,
) -> anyhow::Result<Engine> {
    build_engine_inner(
        proposals,
        socket_policy,
        default_host_memory_budgets(),
        None,
    )
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
    build_engine_inner(proposals, host_socket_policy(), host_memory, None)
}

/// Build the serving engine with a persistent compiled-component cache.
///
/// The caller owns the cache's persistence boundary. Wasmtime validates that
/// the path is absolute and creates it before the engine starts.
pub fn build_engine_with_host_memory_and_compilation_cache(
    proposals: &[WasmProposal],
    host_memory: HostMemoryBudgets,
    compilation_cache_dir: &Path,
) -> anyhow::Result<Engine> {
    let mut cache_config = wasmtime::CacheConfig::new();
    cache_config.with_directory(compilation_cache_dir);
    let cache = wasmtime::Cache::new(cache_config).map_err(|error| {
        anyhow::anyhow!(
            "configure Wasmtime compilation cache at {}: {error}",
            compilation_cache_dir.display()
        )
    })?;
    let mut config = wasmtime::Config::new();
    config.cache(Some(cache));
    build_engine_inner(proposals, host_socket_policy(), host_memory, Some(config))
}

fn build_engine_inner(
    proposals: &[WasmProposal],
    socket_policy: SocketPolicy,
    host_memory: HostMemoryBudgets,
    config: Option<wasmtime::Config>,
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
        .with_guest_memory_mode(GuestMemoryMode::Count)
        .with_pooling_allocator(true)
        .with_socket_policy(Arc::new(socket_policy));
    if let Some(config) = config {
        builder = builder.with_config(config);
    }
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
    use std::fs;
    use std::future::Future as _;
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Waker};
    use std::time::{SystemTime, UNIX_EPOCH};

    use wash_runtime::engine::ctx::{Ctx, SharedCtx};
    use wash_runtime::engine::guest_memory::install_memory_limiter;
    use wash_runtime::host::allowed_hosts::AllowedHost;
    use wash_runtime::sockets::{AddrDecision, DenyReason, SocketAddrUse};
    use wash_runtime::wasmtime::component::Component;
    use wash_runtime::wasmtime::{Instance, Linker, Memory, Module, Store};

    use super::*;

    const PAGE: usize = 64 * 1024;

    fn cache_test_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the test clock is after the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "wamn-wasmtime-cache-{}-{nonce}",
            std::process::id()
        ))
    }

    fn file_count(path: &Path) -> usize {
        fs::read_dir(path)
            .expect("read the compilation cache")
            .map(|entry| entry.expect("read a compilation-cache entry").path())
            .map(|entry| {
                if entry.is_dir() {
                    file_count(&entry)
                } else {
                    1
                }
            })
            .sum()
    }

    /// Every range the address floor denies, with the reason it is on the list.
    ///
    /// Dropping a range from [`host_socket_policy`] turns the matching row from
    /// a `BlockedRange` refusal into an `Allow`, which is what makes this table
    /// a pin rather than a comment.
    ///
    /// The Kubernetes rows use kind's documented defaults —
    /// `serviceSubnet` 10.96.0.0/16 and `podSubnet` 10.244.0.0/16. ASSUMPTION:
    /// `deploy/infra/kind-config.yaml` declares no `networking` block, so kind
    /// supplies both; no manifest in `deploy/` names a subnet of its own. Each
    /// sits inside 10/8, so the RFC1918 half of the floor is what covers them.
    const FLOOR_DENIED: &[(&str, &str)] = &[
        ("IPv4 link-local", "169.254.1.1:443"),
        ("IPv4 cloud metadata", "169.254.169.254:80"),
        ("IPv6 link-local", "[fe80::1]:443"),
        ("IPv6 cloud metadata", "[fd00:ec2::254]:80"),
        ("Kubernetes service network", "10.96.0.1:443"),
        ("Kubernetes pod network", "10.244.0.5:8080"),
        ("RFC1918 10/8", "10.0.0.5:443"),
        ("RFC1918 172.16/12", "172.16.0.1:443"),
        ("RFC1918 192.168/16", "192.168.1.1:443"),
        ("carrier-grade NAT 100.64/10", "100.64.0.1:443"),
        ("IPv6 unique-local fc00::/7", "[fd00::1]:443"),
        // The mapped spellings. A floor an attacker steps over by writing the
        // same address as `::ffff:…` is not a floor.
        ("IPv4-mapped cloud metadata", "[::ffff:169.254.169.254]:80"),
        ("IPv4-mapped service network", "[::ffff:10.96.0.1]:443"),
        ("IPv4-mapped RFC1918", "[::ffff:192.168.1.1]:443"),
    ];

    fn socket_addr(text: &str) -> SocketAddr {
        text.parse().expect("a test address parses")
    }

    fn allowed_host(text: &str) -> AllowedHost {
        text.parse().expect("a test allowed-host entry parses")
    }

    /// The production policy as a guest that opted in to every host sees it.
    fn opted_in(entries: &[&str]) -> SocketPolicy {
        SocketPolicy {
            allowed_hosts: entries.iter().copied().map(allowed_host).collect(),
            ..host_socket_policy()
        }
    }

    /// wamn-d0w4: the address layer is a default-deny FLOOR, not a permissive
    /// pass-through.
    ///
    /// Driven through a `*` allowlist, so layer 1 permits every address here
    /// and every refusal that remains is the floor's alone. Without that the
    /// empty production allowlist would refuse all of these as `NotPermitted`
    /// and the test would pass with no floor at all.
    #[test]
    fn the_address_floor_denies_link_local_metadata_cluster_and_private_ranges() {
        let policy = opted_in(&["*"]);
        for (label, text) in FLOOR_DENIED {
            let addr = socket_addr(text);
            match policy.decide(SocketAddrUse::TcpConnect, addr) {
                AddrDecision::Deny(DenyReason::BlockedRange) => {}
                AddrDecision::Deny(other) => panic!(
                    "{label} ({text}) must be refused by the address floor, not as {other:?}"
                ),
                AddrDecision::Allow(_) => panic!(
                    "{label} ({text}) is reachable: the address floor does not cover it, and a \
                     guest's allowed_hosts is again the sole confinement"
                ),
            }
        }
    }

    /// wamn-d0w4, the other half: the floor is a floor, not a replacement. A
    /// declared `allowed_hosts` still decides what a guest reaches among the
    /// addresses the floor permits, in both directions.
    #[test]
    fn a_declared_allowlist_still_gates_what_the_address_floor_permits() {
        let policy = opted_in(&["93.184.216.34:443"]);

        match policy.decide(SocketAddrUse::TcpConnect, socket_addr("93.184.216.34:443")) {
            AddrDecision::Allow(_) => {}
            AddrDecision::Deny(why) => panic!(
                "a routable address the guest declared must stay reachable; refused as {why:?}"
            ),
        }

        match policy.decide(SocketAddrUse::TcpConnect, socket_addr("1.1.1.1:443")) {
            AddrDecision::Deny(DenyReason::NotPermitted) => {}
            AddrDecision::Deny(other) => panic!(
                "an undeclared routable address is refused by the allowlist layer, not {other:?}"
            ),
            AddrDecision::Allow(_) => {
                panic!("the address floor must not widen what the allowlist granted")
            }
        }
    }

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

    #[test]
    fn a_serving_engine_persists_compiled_components_in_its_declared_cache() {
        let cache_dir = cache_test_path();
        let engine = build_engine_with_host_memory_and_compilation_cache(
            &[],
            default_host_memory_budgets(),
            &cache_dir,
        )
        .expect("build the cached serving engine");
        let bytes = wat::parse_str("(component)").expect("encode an empty component");
        Component::new(engine.inner(), &bytes).expect("compile through the cached engine");

        assert!(
            file_count(&cache_dir) > 0,
            "compiling through the serving engine must create a persistent cache artifact"
        );
        drop(engine);
        fs::remove_dir_all(cache_dir).expect("remove the isolated compilation cache");
    }

    #[test]
    fn a_relative_compilation_cache_path_is_refused() {
        let error = build_engine_with_host_memory_and_compilation_cache(
            &[],
            default_host_memory_budgets(),
            Path::new("relative-cache"),
        )
        .expect_err("a relative cache path must be refused before host startup");
        assert!(error.to_string().contains("relative-cache"), "{error:#}");
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

    fn memory_budgets(pages: u64) -> HostMemoryBudgets {
        HostMemoryBudgets::resolve(Some(pages * PAGE as u64), Some(4 * PAGE as u64), Some(4))
            .expect("nonzero guest budgets")
    }

    fn memory_store(engine: &Engine) -> (Store<SharedCtx>, Memory) {
        let bytes = wat::parse_str("(module (memory (export \"memory\") 1))")
            .expect("encode the memory fixture");
        let module = Module::new(engine.inner(), bytes).expect("compile the memory fixture");
        let ctx = SharedCtx::new(Ctx::builder("memory-proof", "memory-proof").build())
            .with_guest_memory(engine.guest_memory());
        let mut store = Store::new(engine.inner(), ctx);
        install_memory_limiter(&mut store);
        let instance = Instance::new(&mut store, &module, &[]).expect("allocate the first page");
        let memory = instance
            .get_memory(&mut store, "memory")
            .expect("the fixture exports its memory");
        (store, memory)
    }

    #[test]
    fn count_mode_accounts_for_concurrent_stores_and_keeps_the_heap_ceiling() {
        let engine = build_engine_with_host_memory(&[], memory_budgets(2))
            .expect("build the production Count engine");
        let cloned = engine.clone();
        let budget = engine.guest_memory();
        let (mut first, first_memory) = memory_store(&engine);
        let (second, _) = memory_store(&cloned);
        assert_eq!(budget.in_use(), 2 * PAGE as u64);

        assert_eq!(first_memory.grow(&mut first, 2).unwrap(), 1);
        assert_eq!(budget.in_use(), 4 * PAGE as u64);
        assert_eq!(budget.would_refuse(), 1);
        assert_eq!(budget.refused(), 0);
        assert_eq!(budget.high_water(), 4 * PAGE as u64);

        first_memory
            .grow(&mut first, 2)
            .expect_err("Count still refuses growth beyond the per-memory ceiling");
        assert_eq!(first_memory.size(&first), 3);
        assert_eq!(budget.in_use(), 4 * PAGE as u64);
        drop(first);
        assert_eq!(budget.in_use(), PAGE as u64);
        drop(second);
        assert_eq!(budget.in_use(), 0);
    }

    #[test]
    fn enforce_mode_refuses_aggregate_growth_until_another_store_releases_memory() {
        let engine = Engine::builder()
            .with_host_memory(memory_budgets(3))
            .with_guest_memory_mode(GuestMemoryMode::Enforce)
            .with_pooling_allocator(true)
            .build()
            .expect("build the disposable Enforce engine");
        let budget = engine.guest_memory();
        let (mut first, first_memory) = memory_store(&engine);
        let (mut second, second_memory) = memory_store(&engine);
        first_memory.grow(&mut first, 1).unwrap();
        assert_eq!(budget.in_use(), 3 * PAGE as u64);

        second_memory
            .grow(&mut second, 1)
            .expect_err("both stores charge the same aggregate ceiling");
        assert_eq!(second_memory.size(&second), 1);
        assert_eq!(budget.refused(), 1);
        assert_eq!(budget.in_use(), 3 * PAGE as u64);
        drop(first);
        assert_eq!(budget.in_use(), PAGE as u64);
        second_memory.grow(&mut second, 2).unwrap();
        assert_eq!(budget.in_use(), 3 * PAGE as u64);
        drop(second);
        assert_eq!(budget.in_use(), 0);
    }

    #[tokio::test]
    async fn cancelling_a_guest_waiting_in_a_host_call_releases_its_memory() {
        let engine = build_engine_with_host_memory(&[], memory_budgets(2))
            .expect("build the production Count engine");
        let bytes = wat::parse_str(
            r#"(module
                (import "host" "wait" (func $wait))
                (memory 2)
                (func (export "run") call $wait))"#,
        )
        .expect("encode a guest that waits in a host call");
        let module = Module::new(engine.inner(), bytes).expect("compile the waiting guest");
        let entered = Arc::new(AtomicBool::new(false));
        let entered_call = Arc::clone(&entered);
        let mut linker = Linker::<SharedCtx>::new(engine.inner());
        linker
            .func_wrap_async("host", "wait", move |_, ()| {
                entered_call.store(true, Ordering::Relaxed);
                Box::new(std::future::pending::<()>())
            })
            .expect("link the pending host call");
        let ctx = SharedCtx::new(Ctx::builder("memory-proof", "memory-proof").build())
            .with_guest_memory(engine.guest_memory());
        let mut store = Store::new(engine.inner(), ctx);
        install_memory_limiter(&mut store);
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .expect("instantiate the waiting guest");
        let run = instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("the fixture exports run");
        let mut call = Box::pin(async move {
            run.call_async(&mut store, ())
                .await
                .expect("the waiting guest must not trap");
        });
        let mut context = Context::from_waker(Waker::noop());
        assert!(call.as_mut().poll(&mut context).is_pending());
        assert!(entered.load(Ordering::Relaxed));
        assert_eq!(engine.guest_memory().in_use(), 2 * PAGE as u64);
        drop(call);
        assert_eq!(engine.guest_memory().in_use(), 0);
    }

    #[tokio::test]
    async fn dropping_a_store_after_epoch_interruption_releases_its_memory() {
        let engine = build_engine_with_host_memory(&[], memory_budgets(2))
            .expect("build the production Count engine");
        let bytes = wat::parse_str(
            r#"(module
                (memory 2)
                (func (export "run") (loop br 0)))"#,
        )
        .expect("encode a memory-bearing guest that spins");
        let module = Module::new(engine.inner(), bytes).expect("compile the spinning guest");
        let ctx = SharedCtx::new(Ctx::builder("memory-proof", "memory-proof").build())
            .with_guest_memory(engine.guest_memory());
        let mut store = Store::new(engine.inner(), ctx);
        install_memory_limiter(&mut store);
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = Instance::new_async(&mut store, &module, &[])
            .await
            .expect("instantiate the spinning guest");
        let run = instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .expect("the fixture exports run");
        assert_eq!(engine.guest_memory().in_use(), 2 * PAGE as u64);

        let bound = Duration::from_secs(2);
        let started = std::time::Instant::now();
        store.set_epoch_deadline(1);
        let error = tokio::time::timeout(bound, run.call_async(&mut store, ()))
            .await
            .expect("the native epoch ticker must interrupt the guest within two seconds")
            .expect_err("the epoch deadline must interrupt the infinite guest");
        assert!(
            started.elapsed() < bound,
            "the epoch interruption exceeded {bound:?}"
        );
        assert!(
            matches!(
                error.downcast_ref::<wash_runtime::wasmtime::Trap>(),
                Some(wash_runtime::wasmtime::Trap::Interrupt)
            ),
            "the guest must stop with Trap::Interrupt, got {error:#}"
        );
        assert_eq!(engine.guest_memory().in_use(), 2 * PAGE as u64);
        drop(store);
        assert_eq!(engine.guest_memory().in_use(), 0);

        let (fresh, memory) = memory_store(&engine);
        assert_eq!(memory.size(&fresh), 1);
        assert_eq!(engine.guest_memory().in_use(), PAGE as u64);
        drop(fresh);
        assert_eq!(engine.guest_memory().in_use(), 0);
    }

    #[tokio::test]
    async fn dropping_a_store_after_a_guest_start_trap_releases_its_memory() {
        let engine = build_engine_with_host_memory(&[], memory_budgets(2))
            .expect("build the production Count engine");
        let bytes = wat::parse_str("(module (memory 1) (func $start unreachable) (start $start))")
            .expect("encode a guest that traps after allocating memory");
        let module = Module::new(engine.inner(), bytes).expect("compile the trapping guest");
        let ctx = SharedCtx::new(Ctx::builder("memory-proof", "memory-proof").build())
            .with_guest_memory(engine.guest_memory());
        let mut store = Store::new(engine.inner(), ctx);
        install_memory_limiter(&mut store);
        store.set_epoch_deadline(u64::MAX / 2);
        let error = Instance::new_async(&mut store, &module, &[])
            .await
            .expect_err("the guest start function traps");
        assert!(format!("{error:?}").contains("unreachable"), "{error:?}");
        assert_eq!(engine.guest_memory().in_use(), PAGE as u64);
        drop(store);
        assert_eq!(engine.guest_memory().in_use(), 0);
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
