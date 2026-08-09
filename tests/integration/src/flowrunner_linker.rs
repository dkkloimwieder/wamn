//! The ONE host-import registration for the hand-rolled `flowrunner` linkers.
//!
//! `components/execution/flowrunner/wit/world.wit` is the single source of the
//! guest's import set, and `instantiate_pre` fails outright when any import is
//! unlinked. `ExecutionHost` owns that registration on the production path; the
//! benches that build their own [`Linker`] (they inject bench-owned stores,
//! virtual clocks, and recording egress handlers that the production host does
//! not) register through this helper instead of each carrying its own copy — a
//! new world import is swept here once, for every bench.
//!
//! Linking an import is not backing it: the caller still owns its store's
//! plugin map. An effect no bench fixture reaches may stay unbacked, in which
//! case a call traps (fail-closed) instead of escaping the bench.
//!
//! wamn-fstr: this is the ONE registration point, and
//! `tests/conformance/tests/flowrunner_linker_imports.rs` diffs it against the
//! world's import set on every conformance run. A new import therefore lands
//! here AND in that guard's mapping table — the two additions that silently
//! skipped this file (9721d42, 914f661) can no longer pass.

use wamn_runtime::plugins::{
    connection_http, node_invocation, runner_egress, wamn_credentials, wamn_logging, wamn_postgres,
};
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::wasmtime::component::Linker;

/// Register every host import of the `wamn:flowrunner` world on `linker`.
pub fn add_flowrunner_imports_to_linker(linker: &mut Linker<SharedCtx>) -> anyhow::Result<()> {
    wasmtime_wasi::p2::add_to_linker_async(linker)?;
    // S6: the legacy raw `http-call` node leaves through wasi:http, so a bench
    // can interpose its own egress handler on the store.
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(linker)?;
    wamn_postgres::add_to_linker(linker)?;
    // 5.9: the runner imports wamn:node/credentials unconditionally, so the
    // interface must be linked even when the vault is empty.
    wamn_credentials::add_to_linker(linker)?;
    // cjv.3 / fqg.11 / l5i9.12.2: the TRUSTED per-run grant, egress, and
    // causation channels the runner declares before it dispatches a run.
    wamn_credentials::add_runner_to_linker(linker)?;
    runner_egress::add_runner_to_linker(linker)?;
    wamn_postgres::add_runner_causation_to_linker(linker)?;
    // PLAN-2B (wamn-ko5r.8): the TRUSTED one-frame portable HTTP effect the
    // `http-request` node calls. PLAN-0.2 (2jdm.26): the TRUSTED one-frame
    // custom-node invocation. Both are compiled into the runner's world, so
    // both must be linked whether or not a bench's flows reach them.
    connection_http::add_to_linker(linker)?;
    node_invocation::add_to_linker(linker)?;
    // wamn-yf3: run-path wasi:logging emission (node/run lifecycle records).
    wamn_logging::add_to_linker(linker)?;
    Ok(())
}
