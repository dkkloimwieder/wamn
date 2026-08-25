//! Runtime proof of the no-trap discipline for the four executor-sandbox
//! plugins (`wamn-0h0g.15.53`).
//!
//! # Why this exists
//!
//! Gate B (`wamn-0h0g.15.6`) established the no-trap discipline by READING Rust
//! signatures against WIT contracts. A `cargo check` cannot establish runtime
//! behaviour, and the gate said so. These tests drive a real wasm component
//! through a real store into each plugin's host error path and assert the error
//! comes back as DATA — a mapped WIT error, or a survivable no-op where the WIT
//! declares no error channel — rather than as a wasm trap. This COMPLEMENTS the
//! static audit; it replaces nothing.
//!
//! # The four
//!
//! `crates/execution/host/src/lib.rs` registers exactly four plugins on the
//! executor's runner store: `WAMN_POSTGRES_ID`, `RUNNER_EGRESS_ID`,
//! `WAMN_LOGGING_ID`, `CONNECTION_HTTP_ID`. Those are gate B's retained four and
//! the four proved here. `wamn_jetstream`, `flow_http_routing` and
//! `wamn_credentials` are deliberately out of scope: the first two are not on
//! the executor store, and the third
//! implements no `HostPlugin` and has no WIT surface at all.
//!
//! # Why the guests are WAT, not built fixtures
//!
//! The host functions these plugins install live behind each plugin's PRIVATE
//! `mod bindings`, so no `Host` trait is nameable from outside the crate — the
//! only way to reach them is a real guest. A guest compiled from
//! `components/fixtures/` would make these tests depend on a `wasm32-wasip2`
//! build of a separate workspace, and a test that silently skips when that
//! artifact is missing is exactly the vacuous pass this bead exists to prevent.
//! So each guest is authored as component-model WAT and encoded in-process,
//! following the one hermetic precedent in the tree (`deadline_gate` in
//! `crates/execution/host/src/lib.rs`). No cluster, no daemon, no network, no
//! prebuilt artifact.
//!
//! # How a trap is distinguished from a mapped error
//!
//! Every guest imports a test-owned `verdict` marker and calls it
//! [`MARK_ENTER`] before the plugin call. What follows the marker is what
//! discriminates the three outcomes:
//!
//! - trail `[MARK_ENTER, …]` with a further entry ⇒ the host returned and the
//!   guest RESUMED. No trap.
//! - trail `[MARK_ENTER]` plus an `Err` from the guest call ⇒ the host
//!   propagated at the wasmtime level. A TRAP. Every test here fails.
//! - a result discriminant of `0` ⇒ the host returned `Ok`, i.e. the error path
//!   was never reached and the test proves nothing. Asserted against
//!   POSITIVELY, so this fails too.
//!
//! For the two plugins whose WIT declares no error channel the trail proves
//! survival and a plugin-owned public observation proves the host path actually
//! ran.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::connection_http::{self, CONNECTION_HTTP_ID, ConnectionHttp};
use wamn_runtime::plugins::runner_egress::{self, RUNNER_EGRESS_ID, RunnerEgressPolicy};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{self, WAMN_LOGGING_ID, WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{
    self, WAMN_POSTGRES_ID, WamnPostgres, WamnPostgresConfig,
};
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component as WasmtimeComponent, Linker};

/// Pushed by every guest immediately BEFORE the plugin call. Its presence
/// proves the guest reached the call site; a trail containing only this proves
/// the host trapped instead of returning.
const MARK_ENTER: u32 = 238;

/// Pushed AFTER a plugin call whose WIT declares no error channel. Its presence
/// is the whole no-trap claim for those two plugins.
const MARK_RESUMED: u32 = 255;

/// The `component_id` every store is built with. Plugins key their per-component
/// state on it (`ActiveCtx::component_id`), so the test's own registrations —
/// the postgres tenant claim, the egress declaration read-back — must use the
/// same string.
const GUEST_ID: &str = "no-trap-guest";

// ---------------------------------------------------------------------------
// The shared guest memory map
// ---------------------------------------------------------------------------
//
// Every WAT guest below uses one 64 KiB page laid out identically. The regions
// are disjoint and the two `_ZERO` regions are never written by the guest, so
// wasm's zero-initialized memory is what the host reads there.
//
//   192  ..  256   list-descriptor scratch, 8-aligned (`(ptr, len)` pairs)
//   256  ..  512   the guest's data segment (string bytes)
//   512  ..  768   PARAMS_ZERO: indirect-parameter block, 8-aligned, all zero
//   768  .. 1024   RET: canonical-ABI return area, 8-aligned
//   1024 ..        bump-allocator arena handed to the host as `realloc`
//
// 8 is the alignment that matters: `invocation-context` carries a `u64`
// (`frame-id`) and `sql-value` carries `s64`/`f64`, so those types' memory
// alignment is 8 and a misaligned lift traps before any plugin code runs.

/// Base of the return area, and therefore the address of a returned
/// `result<T, E>`'s OWN discriminant (`0` = ok, `1` = err).
///
/// The canonical ABI stores a variant's discriminant first, at offset 0, for
/// every variant regardless of payload. This is the one layout fact every test
/// here relies on, and it holds identically for all four plugins.
const RET_OK_ERR: u32 = 768;

/// The same address for a returned `result<T, E>` whose maximum case alignment
/// is 8, so the payload starts at `align_to(1, 8) == 8`. Applies to
/// `result<u64, pg-error>` — `pg-error` carries a `u64` case
/// (`row-limit-exceeded`), so its alignment is 8.
const RET_ERR_ALIGN8: u32 = RET_OK_ERR + 8;

/// Stand a real component up against a real store carrying `plugins`, link the
/// plugin capability under test, drive the guest's `drive` export, and return
/// the guest call's outcome beside the verdict trail the guest recorded.
///
/// The plugins are registered through `Ctx::with_plugins` and the capability is
/// linked by hand with the plugin module's own `add_to_linker`. That is exactly
/// what `crates/execution/host/src/lib.rs` does for all four: `with_plugins`
/// makes `ActiveCtx::try_get_plugin` resolve, and the hand link installs the
/// host functions. `HostPlugin::world` gates only `on_workload_item_bind`, which
/// the executor never relies on for these four, so this test must not either.
async fn drive_guest(
    guest_wat: &str,
    plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>>,
    link: fn(&mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()>,
) -> (wash_runtime::wasmtime::Result<()>, Vec<u32>) {
    let engine = build_engine(&[]).expect("no-trap engine builds");
    let raw = engine.inner();
    let bytes = wat::parse_str(guest_wat).expect("guest WAT encodes");
    let component = WasmtimeComponent::new(raw, &bytes).expect("guest component compiles");

    let trail: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut linker: Linker<SharedCtx> = Linker::new(raw);
    link(&mut linker).expect("plugin capability links into the guest world");
    linker
        .root()
        .func_wrap("verdict", {
            let trail = trail.clone();
            move |_caller, (code,): (u32,)| {
                trail.lock().expect("verdict trail lock").push(code);
                Ok(())
            }
        })
        .expect("verdict marker links");

    let ctx = Ctx::builder(GUEST_ID.to_string(), GUEST_ID.to_string())
        .with_plugins(plugins)
        .build();
    let mut store = Store::new(raw, SharedCtx::new(ctx));
    // `build_engine` turns epoch interruption ON. This test starts no ticker, so
    // the epoch never advances and a deadline this far out never fires — but
    // leaving the store at its default deadline of 0 would trap the guest before
    // it ever reached the host call, and every assertion below would read as a
    // trap in the plugin.
    store.set_epoch_deadline(u64::MAX / 2);
    let instance = linker
        .instantiate_async(&mut store, &component)
        .await
        .expect("guest instantiates against the plugin capability");
    let drive = instance
        .get_typed_func::<(), ()>(&mut store, "drive")
        .expect("guest exports drive");
    let outcome = drive.call_async(&mut store, ()).await;
    let trail = trail.lock().expect("verdict trail lock").clone();
    (outcome, trail)
}

/// The core module every guest shares: it owns the memory and the `realloc` the
/// host is handed, so the lowered imports can name a memory that exists before
/// the calling module is instantiated. `data` becomes the guest's one data
/// segment, always loaded at address 256.
///
/// The allocator ignores alignment because the host only reaches it to allocate
/// returned strings and lists, and every error asserted here is a unit case with
/// no payload to allocate.
fn libc_module(data: &str) -> String {
    format!(
        r#"
  (core module $libc
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 1024))
    (data (i32.const 256) "{data}")
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
"#
    )
}

// ---------------------------------------------------------------------------
// 1. runner_egress — wamn:runner/egress@0.1.0
// ---------------------------------------------------------------------------

/// `set-allowed-hosts` declares NO error channel (`func(hosts: list<string>)`,
/// no `result`), so the host's only mapping for an entry the [`AllowedHost`]
/// grammar rejects is to drop it fail-closed and keep going. The guest declares
/// the exact (good, bad) pair `runner_egress`'s own unit test pins.
///
/// Data segment: `notify.example` at 256 (len 14), `*bad-wildcard` at 270
/// (len 13).
fn egress_guest() -> String {
    format!(
        r#"
(component
  ;; wamn-0h0g.15.53
  (import "wamn:runner/egress@0.1.0" (instance $egress
    (export "set-allowed-hosts" (func (param "hosts" (list string))))))
  (import "verdict" (func $verdict (param "code" u32)))
{libc}
  (core func $set-hosts-lowered
    (canon lower (func $egress "set-allowed-hosts")
      (memory $libc "memory")
      (realloc (func $libc "realloc"))))
  (core func $verdict-lowered (canon lower (func $verdict)))

  (core module $main
    (import "libc" "memory" (memory 1))
    (import "host" "set-hosts" (func $set-hosts (param i32 i32)))
    (import "host" "verdict" (func $verdict (param i32)))
    (func (export "drive")
      i32.const {enter}
      call $verdict
      ;; hosts[0] = "notify.example" (parses)
      i32.const 192
      i32.const 256
      i32.store
      i32.const 196
      i32.const 14
      i32.store
      ;; hosts[1] = "*bad-wildcard" (the grammar rejects it)
      i32.const 200
      i32.const 270
      i32.store
      i32.const 204
      i32.const 13
      i32.store
      i32.const 192
      i32.const 2
      call $set-hosts
      i32.const {resumed}
      call $verdict))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "set-hosts" (func $set-hosts-lowered))
      (export "verdict" (func $verdict-lowered))))))
  (func (export "drive") (canon lift (core func $main "drive")))
)
"#,
        libc = libc_module("notify.example*bad-wildcard"),
        enter = MARK_ENTER,
        resumed = MARK_RESUMED,
    )
}

#[tokio::test]
async fn runner_egress_drops_an_unparseable_declaration_without_trapping_the_guest() {
    let policy = Arc::new(RunnerEgressPolicy::default());
    let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
    plugins.insert(
        RUNNER_EGRESS_ID,
        policy.clone() as Arc<dyn HostPlugin + Send + Sync>,
    );

    let (outcome, trail) = drive_guest(
        &egress_guest(),
        plugins,
        runner_egress::add_runner_to_linker,
    )
    .await;

    outcome.expect("the guest survived the rejected allowed-host entry");
    assert_eq!(
        trail,
        vec![MARK_ENTER, MARK_RESUMED],
        "the guest must resume after the host rejected an entry"
    );
    // POSITIVE evidence the fail-closed parse path actually ran: the good entry
    // landed and the bad one did not. A declaration that never reached the
    // plugin would leave this `None`, and one that accepted both would be 2.
    let declared = policy
        .declared(GUEST_ID)
        .expect("the declaration reached the plugin");
    assert_eq!(
        declared.len(),
        1,
        "the unparseable entry dropped fail-closed and the valid one survived"
    );
}

// ---------------------------------------------------------------------------
// 2. wamn_logging — wasi:logging/logging@0.1.0-draft
// ---------------------------------------------------------------------------

/// `log` declares NO return value at all, so the only way it can trap is to
/// panic on the guest's stack. `WamnLogging::ingest` parses the guest-supplied
/// `context` JSON synchronously inside the call, and the drain task then parses
/// the `traceparent` it found, so this guest hands both parsers garbage: one
/// call with a context that is not JSON, one with valid JSON carrying a
/// malformed `traceparent`.
///
/// Data segment: `{` at 256 (len 1, not JSON);
/// `{"traceparent":"nope"}` at 257 (len 22, JSON with a garbage traceparent);
/// `no-trap` at 279 (len 7, the message). Level `2` is `info`, the third case of
/// the `wasi:logging` level enum.
fn logging_guest() -> String {
    format!(
        r#"
(component
  ;; wamn-0h0g.15.53
  (import "wasi:logging/logging@0.1.0-draft" (instance $logging
    (type $level' (enum "trace" "debug" "info" "warn" "error" "critical"))
    (export "level" (type $level (eq $level')))
    (export "log" (func
      (param "level" $level)
      (param "context" string)
      (param "message" string)))))
  (import "verdict" (func $verdict (param "code" u32)))
{libc}
  (core func $log-lowered
    (canon lower (func $logging "log")
      (memory $libc "memory")
      (realloc (func $libc "realloc"))))
  (core func $verdict-lowered (canon lower (func $verdict)))

  (core module $main
    (import "libc" "memory" (memory 1))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32)))
    (import "host" "verdict" (func $verdict (param i32)))
    (func (export "drive")
      i32.const {enter}
      call $verdict
      ;; context that is not JSON at all
      i32.const 2
      i32.const 256
      i32.const 1
      i32.const 279
      i32.const 7
      call $log
      ;; valid JSON, malformed W3C traceparent
      i32.const 2
      i32.const 257
      i32.const 22
      i32.const 279
      i32.const 7
      call $log
      i32.const {resumed}
      call $verdict))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "log" (func $log-lowered))
      (export "verdict" (func $verdict-lowered))))))
  (func (export "drive") (canon lift (core func $main "drive")))
)
"#,
        libc = libc_module(r#"{{\"traceparent\":\"nope\"}no-trap"#),
        enter = MARK_ENTER,
        resumed = MARK_RESUMED,
    )
}

#[tokio::test]
async fn wamn_logging_absorbs_a_garbage_guest_context_without_trapping_the_guest() {
    let (logging, capture) =
        WamnLogging::new_with_capture(WamnLoggingConfig::default()).expect("logging plugin builds");
    let logging = Arc::new(logging);
    let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
    plugins.insert(
        WAMN_LOGGING_ID,
        logging.clone() as Arc<dyn HostPlugin + Send + Sync>,
    );

    let (outcome, trail) =
        drive_guest(&logging_guest(), plugins, wamn_logging::add_to_linker).await;

    outcome.expect("the guest survived two malformed log contexts");
    assert_eq!(
        trail,
        vec![MARK_ENTER, MARK_RESUMED],
        "the guest must resume after both malformed records"
    );
    // POSITIVE evidence both calls reached the plugin and neither parser
    // panicked on the guest's stack: `ingest` counted both.
    assert_eq!(
        logging.accepted(),
        2,
        "both malformed records were enqueued, not refused"
    );

    // The drain task is where the traceparent is parsed. Wait for it, bounded,
    // so a wiring regression fails this test instead of hanging it.
    for _ in 0..200 {
        if logging.emitted() == logging.accepted() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let records = capture.snapshot();
    assert_eq!(
        records.len(),
        2,
        "the drain task emitted both records; a panic there would lose them"
    );
    for record in &records {
        assert_eq!(
            record.trace_id, None,
            "a malformed traceparent resolves to no trace id, never a bogus one"
        );
        assert_eq!(record.message, "no-trap");
    }
}

// ---------------------------------------------------------------------------
// 3. wamn_postgres — wamn:postgres/client@0.1.0
// ---------------------------------------------------------------------------

/// The plugin's own contract: a `WamnPostgresConfig` with no `database_url`
/// registers, and every call reports `connection-unavailable`.
fn offline_postgres() -> WamnPostgres {
    WamnPostgres::new(WamnPostgresConfig {
        database_url: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 100,
        statement_timeout_ms: 100,
        row_limit: 10,
    })
    .expect("offline postgres plugin builds")
}

/// `execute` is the smallest `result`-shaped function on the frozen postgres
/// surface — `query` adds `row-set`/`column` for no extra coverage, and the
/// transaction resources add none either, since the failure is upstream of any
/// of them.
///
/// The test registers a tenant first, deliberately: without one the call would
/// stop at the identity gate (`query-error`) and never exercise the resource
/// path. With one, the call runs the whole way to credential resolution, which
/// has nothing to resolve, and comes back `connection-unavailable`.
///
/// `result<u64, pg-error>` flattens to 6 core values, so the return is indirect:
/// the core signature is `(sql ptr, sql len, params ptr, params len, retptr)`.
/// The params list is EMPTY, which is what keeps the guest from having to encode
/// a `sql-value` at all; its pointer is still 8-aligned because `sql-value`
/// carries `s64`/`f64` cases. Data segment: `SELECT 1` at 256 (len 8).
fn postgres_guest() -> String {
    format!(
        r#"
(component
  ;; wamn-0h0g.15.53
  (import "wamn:postgres/types@0.1.0" (instance $types
    (type $sql-value' (variant
      (case "null")
      (case "boolean" bool)
      (case "int32" s32)
      (case "int64" s64)
      (case "float64" f64)
      (case "text" string)
      (case "bytes" (list u8))
      (case "numeric" string)
      (case "timestamptz" string)
      (case "json" string)
      (case "uuid" string)))
    (export "sql-value" (type $sql-value (eq $sql-value')))
    (type $pg-error' (variant
      (case "serialization-failure")
      (case "connection-unavailable")
      (case "statement-timeout")
      (case "row-limit-exceeded" u64)
      (case "unique-violation" string)
      (case "foreign-key-violation" string)
      (case "check-violation" string)
      (case "permission-denied")
      (case "query-error" (tuple string string))))
    (export "pg-error" (type $pg-error (eq $pg-error')))))
  (alias export $types "sql-value" (type $sql-value))
  (alias export $types "pg-error" (type $pg-error))
  (import "wamn:postgres/client@0.1.0" (instance $client
    (export "sql-value" (type (eq $sql-value)))
    (export "pg-error" (type (eq $pg-error)))
    (export "execute" (func
      (param "sql" string)
      (param "params" (list $sql-value))
      (result (result u64 (error $pg-error)))))))
  (import "verdict" (func $verdict (param "code" u32)))
{libc}
  (core func $execute-lowered
    (canon lower (func $client "execute")
      (memory $libc "memory")
      (realloc (func $libc "realloc"))))
  (core func $verdict-lowered (canon lower (func $verdict)))

  (core module $main
    (import "libc" "memory" (memory 1))
    (import "host" "execute" (func $execute (param i32 i32 i32 i32 i32)))
    (import "host" "verdict" (func $verdict (param i32)))
    (func (export "drive")
      i32.const {enter}
      call $verdict
      i32.const 256
      i32.const 8
      i32.const 192
      i32.const 0
      i32.const 768
      call $execute
      ;; ok/err discriminant, then the pg-error case
      i32.const {ok_err}
      i32.load8_u
      call $verdict
      i32.const {err_case}
      i32.load8_u
      call $verdict))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "execute" (func $execute-lowered))
      (export "verdict" (func $verdict-lowered))))))
  (func (export "drive") (canon lift (core func $main "drive")))
)
"#,
        libc = libc_module("SELECT 1"),
        enter = MARK_ENTER,
        ok_err = RET_OK_ERR,
        err_case = RET_ERR_ALIGN8,
    )
}

#[tokio::test]
async fn wamn_postgres_maps_an_unresolvable_project_to_connection_unavailable_not_a_trap() {
    let postgres = Arc::new(offline_postgres());
    postgres
        .set_tenant(GUEST_ID, "acme")
        .expect("tenant claim registers");
    let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
    plugins.insert(
        WAMN_POSTGRES_ID,
        postgres as Arc<dyn HostPlugin + Send + Sync>,
    );

    let (outcome, trail) =
        drive_guest(&postgres_guest(), plugins, wamn_postgres::add_to_linker).await;

    outcome.expect("the guest survived the postgres refusal");
    // `1` is `err`, and `1` again is `connection-unavailable` — the second case
    // of `pg-error`. Both being non-zero is what rules out an unreached path.
    assert_eq!(
        trail,
        vec![MARK_ENTER, 1, 1],
        "an unresolvable project reports connection-unavailable through the WIT error channel"
    );
}

// ---------------------------------------------------------------------------
// 4. connection_http — wamn:runner/http-effect@0.1.0
// ---------------------------------------------------------------------------

/// `ConnectionHttp` implements the TRUSTED `wamn:runner/http-effect` surface,
/// not the portable `wamn:connection/http` one (its `HostPlugin::world` names
/// `wamn:runner/http-effect@0.1.0`), and `send` is its only guest-visible
/// function.
///
/// `validate_claims` is the first statement in `ConnectionHttp::send` and
/// refuses a context whose `version` is not `"0.1"`, so this guest passes an
/// ALL-ZERO parameter block: every string lifts empty, `version != "0.1"`, and
/// the call returns `invalid-context` without reaching the vault, the database
/// or the wire. Zeros are also what makes the block layout-independent — the
/// guest only has to get the size and the 8-alignment right, not the offsets.
///
/// The two records flatten to 25 core values, past the 16-value limit, so the
/// parameters are indirect too: the core signature is `(params ptr, retptr)`.
fn http_effect_guest() -> String {
    format!(
        r#"
(component
  ;; wamn-0h0g.15.53
  (import "wamn:runner/http-effect@0.1.0" (instance $effect
    (type $invocation-context' (record
      (field "version" string)
      (field "run-id" string)
      (field "root-plan-hash" string)
      (field "current-plan-hash" string)
      (field "frame-id" u64)
      (field "local-node-id" string)
      (field "occurrence" u32)
      (field "source-artifact-hash" string)
      (field "requirement-name" string)))
    (export "invocation-context" (type $invocation-context (eq $invocation-context')))
    (type $header' (record
      (field "name" string)
      (field "value" (list u8))))
    (export "header" (type $header (eq $header')))
    (type $relative-request' (record
      (field "method" string)
      (field "path-and-query" string)
      (field "headers" (list $header))
      (field "body" (option (list u8)))))
    (export "relative-request" (type $relative-request (eq $relative-request')))
    (type $response' (record
      (field "status" u16)
      (field "headers" (list $header))
      (field "body" (list u8))))
    (export "response" (type $response (eq $response')))
    (type $effect-error' (variant
      (case "invalid-context")
      (case "undeclared-requirement")
      (case "node-not-permitted")
      (case "unbound")
      (case "inactive-generation")
      (case "incompatible")
      (case "authority-denied")
      (case "credential-unavailable")
      (case "timeout")
      (case "transport" string)))
    (export "effect-error" (type $effect-error (eq $effect-error')))
    (export "send" (func
      (param "context" $invocation-context)
      (param "request" $relative-request)
      (result (result $response (error $effect-error)))))))
  (import "verdict" (func $verdict (param "code" u32)))
{libc}
  (core func $send-lowered
    (canon lower (func $effect "send")
      (memory $libc "memory")
      (realloc (func $libc "realloc"))))
  (core func $verdict-lowered (canon lower (func $verdict)))

  (core module $main
    (import "libc" "memory" (memory 1))
    (import "host" "send" (func $send (param i32 i32)))
    (import "host" "verdict" (func $verdict (param i32)))
    (func (export "drive")
      i32.const {enter}
      call $verdict
      i32.const 512
      i32.const 768
      call $send
      i32.const {ok_err}
      i32.load8_u
      call $verdict))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "send" (func $send-lowered))
      (export "verdict" (func $verdict-lowered))))))
  (func (export "drive") (canon lift (core func $main "drive")))
)
"#,
        libc = libc_module("unused"),
        enter = MARK_ENTER,
        ok_err = RET_OK_ERR,
    )
}

#[tokio::test]
async fn connection_http_maps_an_invalid_context_to_a_wit_error_not_a_trap() {
    let postgres = Arc::new(offline_postgres());
    // Empty is deny-all, but the refusal lands upstream of the egress check.
    let allowed_hosts: Arc<[AllowedHost]> = Vec::new().into();
    let effect = Arc::new(ConnectionHttp::new(
        postgres,
        Arc::new(WamnCredentials::empty()),
        "acme",
        "receiving",
        allowed_hosts,
        None,
    ));
    let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
    plugins.insert(
        CONNECTION_HTTP_ID,
        effect as Arc<dyn HostPlugin + Send + Sync>,
    );

    let (outcome, trail) = drive_guest(
        &http_effect_guest(),
        plugins,
        connection_http::add_to_linker,
    )
    .await;

    outcome.expect("the guest survived the effect refusal");
    // Only the ok/err discriminant is asserted. `invalid-context` is case ZERO
    // of `effect-error` and the return area is zero-initialized, so reading the
    // inner byte would pass whether or not the host ever wrote it — a
    // tautological assertion, which is worse than none. The refusal identity is
    // covered by `refusal_precedence_is_explicit_and_typed` in the plugin;
    // what this test adds is that it crosses the ABI as data.
    assert_eq!(
        trail,
        vec![MARK_ENTER, 1],
        "an invalid invocation context returns through the WIT error channel"
    );
}
