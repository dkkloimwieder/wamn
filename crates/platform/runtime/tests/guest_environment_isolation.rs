//! The runtime floor beneath the tenant allowlist: a guest never sees the host
//! process environment (`wamn-3ynz`, replacing `wamn-mrfr`'s source scan).
//!
//! # Why this is load-bearing and not hypothetical
//!
//! `deploy/platform/executor.yaml` and `deploy/platform/values-host-default.yaml`
//! inject a credentialed `WAMN_PG_URL` into the HOST PROCESS environment via
//! `secretKeyRef`. If the runtime handed a guest the process environment, a
//! component would read the platform's database credential directly. So the
//! `WasiCtx` construction is a genuine second control.
//!
//! # This is the floor, not the whole story
//!
//! Tenant admission separately DENIES `wasi:cli/environment` to tenant code —
//! the closed allowlist in `crates/platform/component-policy`, reached from
//! `services/ctl/src/push_component.rs` through `validate_component_admission`.
//! That is the first door. This test proves the guarantee that holds for a guest
//! which imports the interface ANYWAY: defence in depth, measured rather than
//! asserted.
//!
//! # Why a behavioural test and not a source scan
//!
//! `wamn-mrfr` proved this property by reading the pinned fork checkout as text
//! and grepping for `inherit_env` plus two pinned `WasiCtxBuilder` literals.
//! `wamn-hopk` R5 retired that whole technique: a scan cannot tell a reference
//! from a mention, and it passes vacuously the moment upstream spells the same
//! behaviour differently — which the mrfr lane itself demonstrated by writing a
//! mutant that leaked the environment WITHOUT using the word `inherit_env`.
//! Running a guest answers the question the scan only approximated.
//!
//! # Why the guest is WAT and not a `components/fixtures` crate
//!
//! `executor_plugin_no_trap_live.rs` records the reason and it applies here
//! unchanged: a guest compiled from `components/fixtures/` makes this test depend
//! on a `wasm32-wasip2` build of a SEPARATE WORKSPACE, and a test that silently
//! skips when that artifact is missing is exactly the vacuous pass this file
//! exists to prevent. The `components/fixtures` guests (`sockprobe`, `busyloop`,
//! `connection-http-standard`) are driven by IN-CLUSTER gates through mounted
//! volumes. The hermetic precedent for an in-process guest is component-model WAT
//! encoded at run time. No cluster, no daemon, no prebuilt artifact, no skip.

use std::sync::{Arc, Mutex};

use wamn_runtime::engine::build_engine;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder};

/// The component id the fork's context builder is handed. Nothing keys on it —
/// this test binds no workload and resolves no template.
const GUEST_ID: &str = "guest-environment-isolation";

/// The variable the POSITIVE CONTROL hands the guest through an explicit
/// context. Its name is deliberately unlike any real configuration key, so an
/// entry the probe reports can only have come from there.
const SENTINEL_KEY: &str = "WAMN_GUEST_ENV_ISOLATION_SENTINEL";
const SENTINEL_VALUE: &str = "the-guest-must-never-read-this";

/// The guest: imports `wasi:cli/environment`, calls `get-environment`, and hands
/// the host the NUMBER OF ENTRIES it was given.
///
/// The canonical ABI returns a `list<tuple<string, string>>` through a return
/// pointer, so the lowered import takes one `i32` retptr and writes `(ptr, len)`
/// there. The guest reads the length at `retptr + 4` and reports it. Counting is
/// the whole signal: an inheriting runtime hands over a populated environment,
/// and any non-zero count fails.
fn envprobe_wat() -> String {
    format!(
        r#"
(component
  (import "wasi:cli/environment@0.2.3" (instance $env
    (export "get-environment" (func (result (list (tuple string string)))))))
  (import "verdict" (func $verdict (param "count" u32)))

  (core module $libc
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc")
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
      local.get $result))
  (core instance $libc (instantiate $libc))

  (core func $get-environment-lowered
    (canon lower (func $env "get-environment")
      (memory $libc "memory")
      (realloc (func $libc "realloc"))))
  (core func $verdict-lowered (canon lower (func $verdict)))

  (core module $main
    (import "libc" "memory" (memory 1))
    (import "host" "get-environment" (func $get_environment (param i32)))
    (import "host" "verdict" (func $verdict (param i32)))
    (func (export "drive")
      ;; The canonical return area. 512 is past the allocator's base, so the
      ;; realloc bump arena at 1024 cannot overwrite it.
      i32.const {RETPTR}
      call $get_environment
      ;; (ptr, len) was written at RETPTR; the entry count is the second word.
      i32.const {RETPTR}
      i32.load offset=4
      call $verdict))

  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "get-environment" (func $get-environment-lowered))
      (export "verdict" (func $verdict-lowered))))))

  (func (export "drive") (canon lift (core func $main "drive")))
)
"#,
        RETPTR = 512
    )
}

/// Instantiate the guest through the FORK'S OWN context builder and return the
/// number of environment entries it observed.
///
/// `Ctx::builder(..).build()` with no `with_wasi_ctx` is the fork's FALLBACK
/// path — the one `wamn-mrfr` pinned as text and the one every executor guest
/// takes, because the executor's context builder never calls `with_wasi_ctx`.
/// Building a `WasiCtx` here instead would prove something about this test's
/// builder and nothing about the runtime.
async fn observed_environment_entries(wasi: Option<WasiCtx>) -> u32 {
    let engine = build_engine(&[]).expect("isolation engine builds");
    let raw = engine.inner();
    let bytes = wat::parse_str(envprobe_wat()).expect("envprobe WAT encodes");
    let component = WasmtimeComponent::new(raw, &bytes).expect("envprobe compiles");

    let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).expect("wasi links");
    linker
        .root()
        .func_wrap("verdict", {
            let seen = Arc::clone(&seen);
            move |_caller, (count,): (u32,)| {
                seen.lock().expect("verdict lock").push(count);
                Ok(())
            }
        })
        .expect("verdict marker links");

    let builder = Ctx::builder(GUEST_ID.to_string(), GUEST_ID.to_string());
    let ctx = match wasi {
        Some(wasi) => builder.with_wasi_ctx(wasi).build(),
        None => builder.build(),
    };
    let mut store = Store::new(raw, SharedCtx::new(ctx));
    // `build_engine` turns epoch interruption on and this test starts no ticker,
    // so the epoch never advances; the default deadline of 0 would otherwise trap
    // the guest before it reached the import.
    store.set_epoch_deadline(u64::MAX / 2);

    let instance = linker
        .instantiate_async(&mut store, &component)
        .await
        .expect("envprobe instantiates");
    let drive = instance
        .get_typed_func::<(), ()>(&mut store, "drive")
        .expect("envprobe exports drive");
    drive
        .call_async(&mut store, ())
        .await
        .expect("envprobe runs without trapping");

    let seen = seen.lock().expect("verdict lock").clone();
    assert_eq!(
        seen.len(),
        1,
        "the guest must report exactly once; got {seen:?}"
    );
    seen[0]
}

/// POSITIVE CONTROL, and the reason the negative below is not vacuous.
///
/// Hands the guest a context carrying ONE sentinel variable and asserts it
/// reports exactly one entry. If the WAT ever stopped calling `get-environment`
/// — or started reporting a constant — this test fails, so the negative can
/// never pass by accident. The sentinel is supplied EXPLICITLY rather than by
/// mutating the test process's own environment: `std::env::set_var` is `unsafe`
/// in Rust 2024 precisely because it races every concurrent reader, and a
/// guarantee about isolation should not be proven with a data race.
#[tokio::test]
async fn a_guest_reads_the_environment_its_context_carries() {
    let mut wasi = WasiCtxBuilder::new();
    wasi.envs(&[(SENTINEL_KEY, SENTINEL_VALUE)]);

    let observed = observed_environment_entries(Some(wasi.build())).await;

    assert_eq!(
        observed, 1,
        "a guest handed a context carrying {SENTINEL_KEY} reported {observed} \
         entries; the probe is not reading the real environment, so the \
         isolation assertion below would pass vacuously"
    );
}

/// The property: taking the fork's FALLBACK context — the path every executor
/// guest takes, because the executor's context builder never calls
/// `with_wasi_ctx` — a guest observes NOTHING.
#[tokio::test]
async fn a_guest_never_sees_the_host_process_environment() {
    // The host process running this test has a populated environment of its own
    // (PATH at minimum), which is what the guest must not be handed. In
    // production that environment carries a credentialed WAMN_PG_URL.
    let host_entries = std::env::vars().count();
    assert!(
        host_entries > 0,
        "the host process must hold an environment for this test to mean anything"
    );

    let observed = observed_environment_entries(None).await;

    assert_eq!(
        observed, 0,
        "a guest instantiated through the fork's own fallback context observed \
         {observed} environment entries while the host process held \
         {host_entries}; it must observe NONE. In production that host \
         environment carries the credentialed WAMN_PG_URL that \
         deploy/platform/executor.yaml injects via secretKeyRef"
    );
}
