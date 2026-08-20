//! Behavioral fresh-instance proof for the digest-keyed execution pool
//! (wamn-0h0g.17.2).
//!
//! `tests/conformance/src/runtime_inventory.rs` guards the pooling seam by
//! SOURCE TEXT alone: it reads the fork's `src/engine/instance_pool.rs` for the
//! warm-store activation literal and rejects a nonzero `poolSize` in workload
//! YAML. Neither statement runs a guest, so neither can say what a second
//! invocation actually observes.
//!
//! This file does. It builds the engine `wamn_runtime::engine::build_engine`
//! builds for every wamn-host mode — the production pooling-allocator config,
//! not a stand-in — pre-instantiates one component through `instantiate_pre`,
//! prewarms two stores from that single `InstancePre` under ONE
//! [`ExecutionPoolKey`], and asserts what the second checkout sees.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wamn_execution_host::{
    ExecutionInstancePool, ExecutionLease, ExecutionPoolKey, ExecutionPoolLimits,
    INVOCATIONS_PER_INSTANCE, InvocationDisposition, RetirementReason, ReusableExecutionInstance,
    TrustedExecutionRuntimeRevision,
};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, MEMORY_CAP_BYTES, build_engine};
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, InstancePre, Linker, TypedFunc};

/// The guest under test.
///
/// `run` calls the host tick, crosses a loop back-edge so the epoch deadline is
/// actually checked, then reads the invocation-state cell at address 0, dirties
/// it, and returns what it READ. A caller therefore learns, from the return
/// value alone, whether the store it was handed carried a previous
/// invocation's guest state.
const GUEST: &str = r#"
    (component
        (import "tick" (func $tick))
        (core func $tick-lowered (canon lower (func $tick)))
        (core module $guest
            (import "" "tick" (func $tick))
            (memory (export "memory") 1)
            (global $next (mut i32) (i32.const 1024))
            (func $realloc
                (export "realloc")
                (param $old i32)
                (param $old-size i32)
                (param $align i32)
                (param $new-size i32)
                (result i32)
                (local $result i32)
                global.get $next
                local.tee $result
                local.get $new-size
                i32.add
                global.set $next
                local.get $result)
            (func $epoch-checkpoint
                (local $return i32)
                loop $checkpoint
                    local.get $return
                    if
                        return
                    end
                    i32.const 1
                    local.set $return
                    br $checkpoint
                end)
            (func (export "run")
                (param i32 i32 i32 i32)
                (result i32)
                (local $seen i32)
                call $tick
                call $epoch-checkpoint
                i32.const 0
                i32.load
                local.set $seen
                i32.const 0
                i32.const 305419896
                i32.store
                i32.const 64
                i32.const 0
                i32.store
                i32.const 68
                local.get $seen
                i32.store
                i32.const 72
                i32.const 0
                i32.store
                i32.const 64)
            )
        (core instance $guest
            (instantiate $guest
                (with "" (instance
                    (export "tick" (func $tick-lowered))))))
        (func (export "run")
            (param "run-id" string)
            (param "payload" string)
            (result (result u32 (error string)))
            (canon lift
                (core func $guest "run")
                (memory $guest "memory")
                (realloc (func $guest "realloc"))))
    )
"#;

/// One guest attempt, in the units `ExecutionHost` uses.
const ATTEMPT_MS: u64 = 40;

/// The window one checkout arms, derived from the production tick period the
/// same way `ExecutionHost::deadline_ticks` derives it.
fn window_ticks() -> u64 {
    ATTEMPT_MS.div_ceil(DEFAULT_EPOCH_TICK.as_millis() as u64)
}

/// One prewarmed store, exclusively owned for the length of a checkout.
struct PooledGuest {
    store: Store<SharedCtx>,
    run: TypedFunc<(String, String), (Result<u32, String>,)>,
    /// The tenant this store currently resolves to, if any. This file is about
    /// GUEST state rather than identity — `execution_pool_checkout_identity.rs`
    /// owns that proof — but the fixture carries the field so it cannot quietly
    /// claim an identity-free store while holding one.
    bound: Option<String>,
}

impl std::fmt::Debug for PooledGuest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PooledGuest")
            .finish_non_exhaustive()
    }
}

/// A component instance cannot be reset in place from the host.
///
/// The host holds no handle on a component's linear memory, globals, or tables
/// — a component exports none of them — so the only host-side zeroing of guest
/// state is destroying the store. A guest-cooperative `reset` export could do
/// it, but [`ReusableExecutionInstance::reset_invocation_state`] is synchronous
/// and a component export is only callable through `call_async`. This fixture
/// therefore fails closed, which is the same answer the real seam has today and
/// the reason [`INVOCATIONS_PER_INSTANCE`] is 1.
#[derive(Debug)]
struct HostSideResetUnavailable;

impl std::fmt::Display for HostSideResetUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a component instance cannot be reset in place from the host")
    }
}

impl std::error::Error for HostSideResetUnavailable {}

impl ReusableExecutionInstance for PooledGuest {
    type Identity = String;
    type BindError = std::convert::Infallible;
    type ResetError = HostSideResetUnavailable;

    fn reserved_bytes(&self) -> usize {
        // One wasm page, the guest's declared minimum.
        64 * 1024
    }

    fn bind_identity(&mut self, tenant: &String) -> Result<(), Self::BindError> {
        self.bound = Some(tenant.clone());
        Ok(())
    }

    fn revoke_identity(&mut self) {
        self.bound = None;
    }

    fn reset_invocation_state(&mut self) -> Result<(), Self::ResetError> {
        Err(HostSideResetUnavailable)
    }
}

/// Check out under a single throwaway tenant: these proofs vary the invocation,
/// not the identity.
fn checkout(
    pool: &ExecutionInstancePool<PooledGuest>,
    key: &ExecutionPoolKey,
) -> Option<ExecutionLease<PooledGuest>> {
    pool.checkout(key, &"pool-gate-tenant".to_string())
        .expect("binding a pool-gate identity cannot fail")
}

/// Compile the guest on the production engine and pre-instantiate it once.
///
/// `increments[n]` is how many epochs the host tick advances on the n-th guest
/// call across every store built from this `InstancePre` — the shared engine
/// epoch is exactly what makes a stale, unre-armed deadline observable.
fn pre_instantiated(
    engine: &Engine,
    bytes: &[u8],
    increments: Arc<[u64]>,
) -> (InstancePre<SharedCtx>, Arc<AtomicUsize>) {
    let raw = engine.inner();
    let component = Component::new(raw, bytes).expect("compile the pool gate component");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    linker
        .root()
        .func_wrap("tick", {
            let calls = calls.clone();
            let raw = raw.clone();
            move |_caller, (): ()| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                for _ in 0..increments.get(call).copied().unwrap_or_default() {
                    raw.increment_epoch();
                }
                Ok(())
            }
        })
        .expect("link the pool gate tick");
    (
        linker
            .instantiate_pre(&component)
            .expect("pre-instantiate the pool gate component"),
        calls,
    )
}

/// Build one warm store from the per-digest `InstancePre`.
///
/// Deliberately does NOT arm an epoch deadline: arming is the checkout's job,
/// and a prewarm that armed at construction would hand the second checkout a
/// window the first had already spent.
async fn prewarm(engine: &Engine, pre: &InstancePre<SharedCtx>) -> PooledGuest {
    let ctx = Ctx::builder("pool-gate".to_string(), "pool-gate".to_string()).build();
    let mut store = Store::new(engine.inner(), SharedCtx::new(ctx));
    let instance = pre
        .instantiate_async(&mut store)
        .await
        .expect("instantiate a prewarmed store");
    let run = instance
        .get_typed_func(&mut store, "run")
        .expect("the prewarmed store exports run");
    PooledGuest {
        store,
        run,
        bound: None,
    }
}

/// One checkout's work, mirroring `ExecutionHost::call_run`: re-arm this
/// store's epoch window, then call the guest exactly once.
async fn drive(guest: &mut PooledGuest, run_id: &str) -> anyhow::Result<u32> {
    guest.store.set_epoch_deadline(window_ticks());
    let (result,) = guest
        .run
        .call_async(&mut guest.store, (run_id.to_owned(), "{}".to_owned()))
        .await?;
    result.map_err(|error| anyhow::anyhow!("run: {error}"))
}

/// Reuse-off limits: the shipped policy, with room for two prewarmed stores.
fn limits() -> ExecutionPoolLimits {
    ExecutionPoolLimits {
        max_instances: 2,
        max_reserved_bytes: 1 << 20,
        max_idle_per_digest: 2,
        max_invocations_per_instance: INVOCATIONS_PER_INSTANCE,
        max_idle_age: Duration::from_secs(60),
    }
}

/// The pool key is the host-derived digest of the exact component bytes — the
/// same derivation the execution host performs on the flowrunner it loads.
fn digest_key(bytes: &[u8]) -> ExecutionPoolKey {
    let revision = TrustedExecutionRuntimeRevision::from_flowrunner_bytes(bytes);
    ExecutionPoolKey::new(revision.flowrunner_component_digest())
}

/// Two invocations, ONE pool key, on the production pooling engine.
///
/// A dirties its guest-state cell and spends two of its four ticks. Under
/// [`INVOCATIONS_PER_INSTANCE`] A's store is destroyed at the end of its
/// checkout rather than repooled, so B is handed the other prewarmed store and
/// observes zero. B then spends THREE ticks: it survives only because its
/// checkout re-armed a full window. Both mutants die here — raising
/// `INVOCATIONS_PER_INSTANCE` hands B A's dirty store, and arming at
/// construction instead of at checkout leaves B the two ticks A did not spend.
#[tokio::test]
async fn second_invocation_on_one_pool_key_sees_zeroed_guest_state_and_a_fresh_epoch_window() {
    let engine = build_engine(&[]).expect("the production pooling engine");
    let bytes = wat::parse_str(GUEST).expect("encode the pool gate component");
    let (pre, calls) = pre_instantiated(&engine, &bytes, Arc::from([2u64, 3]));

    let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
    let key = digest_key(&bytes);
    for _ in 0..2 {
        pool.insert(key.clone(), prewarm(&engine, &pre).await)
            .expect("prewarm one store from the per-digest InstancePre");
    }

    let mut first = checkout(&pool, &key).expect("a warm store is available");
    assert_eq!(
        drive(first.instance_mut(), "run-a")
            .await
            .expect("A stays inside its four-tick window after spending two"),
        0,
        "a never-invoked store carries no guest state"
    );
    first
        .finish(InvocationDisposition::Reusable)
        .expect("A is destroyed at the end of its checkout, so no reset is attempted");
    assert_eq!(
        pool.snapshot()
            .retirements
            .get(&RetirementReason::MaxInvocations),
        Some(&1),
        "reuse is off: the instance does not survive the checkout it served"
    );

    let mut second = checkout(&pool, &key).expect("the same key still has a warm store");
    assert_eq!(
        drive(second.instance_mut(), "run-b")
            .await
            .expect("B has four fresh ticks, not the two A left behind"),
        0,
        "B observes zeroed guest state, never A's sentinel"
    );
    second
        .finish(InvocationDisposition::Reusable)
        .expect("B is destroyed at the end of its checkout too");

    let snapshot = pool.snapshot();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        snapshot.reused_checkouts, 0,
        "no instance served two invocations"
    );
    assert_eq!(snapshot.live_instances, 0);
    assert_eq!(
        snapshot.retirements.get(&RetirementReason::MaxInvocations),
        Some(&2)
    );
}

/// The instance destroyed at the end of a checkout hands its pooling slot back
/// to the allocator with a tenant's bytes still written in it. The next
/// instance built under the same key takes that slot and must see zeros.
///
/// `build_engine` sets only `max_memory_size` and the four `total_*` counts, so
/// `linear_memory_keep_resident` and `table_keep_resident` keep their 0 default
/// (`wasmtime-47.0.3/src/config.rs:3883-3884`) and deallocation decommits. This
/// asserts that behaviourally rather than trusting the default: with one module
/// in flight the allocator's affinity puts the freed slot back in play.
#[tokio::test]
async fn a_recycled_pooling_slot_carries_no_bytes_from_the_destroyed_instance() {
    let engine = build_engine(&[]).expect("the production pooling engine");
    let bytes = wat::parse_str(GUEST).expect("encode the pool gate component");
    let (pre, _calls) = pre_instantiated(&engine, &bytes, Arc::from([0u64, 0]));

    let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
    let key = digest_key(&bytes);

    pool.insert(key.clone(), prewarm(&engine, &pre).await)
        .expect("prewarm one store");
    let mut first = checkout(&pool, &key).expect("a warm store is available");
    assert_eq!(
        drive(first.instance_mut(), "dirty")
            .await
            .expect("an unpressured invocation completes"),
        0
    );
    first
        .finish(InvocationDisposition::Reusable)
        .expect("destroyed at the end of its checkout");
    assert_eq!(
        pool.snapshot().live_instances,
        0,
        "the slot is back with the allocator, carrying the sentinel A wrote"
    );

    pool.insert(key.clone(), prewarm(&engine, &pre).await)
        .expect("prewarm into the freed slot");
    let mut second = checkout(&pool, &key).expect("the recycled store");
    assert_eq!(
        drive(second.instance_mut(), "recycled")
            .await
            .expect("an unpressured invocation completes"),
        0,
        "a recycled pooling slot carries none of the freed instance's bytes"
    );
    second
        .finish(InvocationDisposition::Reusable)
        .expect("destroyed at the end of its checkout too");
}

/// The window assertion above discriminates only if the window can actually
/// close. Same engine, same guest: an invocation that advances the epoch past
/// its own deadline must be interrupted — which is also the proof that
/// [`build_engine`]'s `epoch_interruption(true)` reaches this store.
#[tokio::test]
async fn an_over_budget_invocation_is_interrupted_on_the_production_engine() {
    let engine = build_engine(&[]).expect("the production pooling engine");
    let bytes = wat::parse_str(GUEST).expect("encode the pool gate component");
    let (pre, _calls) = pre_instantiated(&engine, &bytes, Arc::from([5u64]));

    let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
    let key = digest_key(&bytes);
    pool.insert(key.clone(), prewarm(&engine, &pre).await)
        .expect("prewarm one store");

    let mut lease = checkout(&pool, &key).expect("a warm store is available");
    let trapped = drive(lease.instance_mut(), "over-budget")
        .await
        .expect_err("five ticks exceeds the four-tick window");
    assert_eq!(
        trapped.downcast_ref::<wash_runtime::wasmtime::Trap>(),
        Some(&wash_runtime::wasmtime::Trap::Interrupt),
        "the epoch deadline interrupts the guest; any other trap is a different bug: {trapped:?}"
    );
    lease
        .finish(InvocationDisposition::Trap)
        .expect("a trapped instance is destroyed, not reset");
    assert_eq!(
        pool.snapshot().retirements.get(&RetirementReason::Trap),
        Some(&1)
    );
}

/// The engine these proofs run on is the production pooling allocator, not the
/// on-demand default.
///
/// `build_engine` caps every memory at [`MEMORY_CAP_BYTES`] through
/// `PoolingAllocationConfig::max_memory_size`, and only the pooling allocator
/// enforces such a cap at all — the on-demand default accepts both modules
/// below. Bracketing the boundary one page either side pins the engine to the
/// pooling strategy AND to this exact ceiling, so dropping
/// `with_pooling_config` fails the second half and moving the cap without
/// moving the platform ceiling fails one half or the other.
#[test]
fn the_pool_gate_runs_on_the_production_build_engine_pooling_config() {
    const PAGE: usize = 64 * 1024;
    assert_eq!(
        MEMORY_CAP_BYTES % PAGE,
        0,
        "the platform ceiling is a whole page count"
    );
    let ceiling_pages = MEMORY_CAP_BYTES / PAGE;

    let engine = build_engine(&[]).expect("the production pooling engine");
    let at_ceiling = wat::parse_str(format!("(module (memory {ceiling_pages}))"))
        .expect("encode a module at the ceiling");
    wash_runtime::wasmtime::Module::new(engine.inner(), &at_ceiling)
        .expect("a memory of exactly the platform ceiling is admitted");

    let over_ceiling = wat::parse_str(format!("(module (memory {}))", ceiling_pages + 1))
        .expect("encode a module one page over the ceiling");
    let rejection = wash_runtime::wasmtime::Module::new(engine.inner(), &over_ceiling)
        .expect_err("one page over the ceiling is refused, which only pooling does");

    assert!(
        format!("{rejection:?}").contains("exceeds the limit"),
        "the refusal is the allocator's memory limit, not another compile error: {rejection:?}"
    );
}
