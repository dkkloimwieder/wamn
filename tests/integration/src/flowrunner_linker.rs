//! The ONE host-import registration for the hand-rolled `flowrunner` linkers.
//!
//! `components/execution/flowrunner/wit/world.wit` is the single source of the
//! guest's import set, and `instantiate_pre` fails outright when any import is
//! unlinked. `ExecutionHost` owns that registration on the production path; the
//! tests that build their own [`Linker`] register through this helper instead
//! of each carrying its own copy — a new world import is swept here once, for
//! every retained test.
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

use wamn_runtime::plugins::{connection_http, runner_plan_supply, wamn_logging, wamn_postgres};
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::wasmtime::component::Linker;

/// Register every host import of the `wamn:flowrunner` world on `linker`.
pub fn add_flowrunner_imports_to_linker(linker: &mut Linker<SharedCtx>) -> anyhow::Result<()> {
    wamn_postgres::add_to_linker(linker)?;
    // wamn-0h0g.5.13: link the trusted immutable plan-supply channel. These
    // F.2 harnesses do not back the plugin; an accidental call stays fail-closed.
    runner_plan_supply::add_to_linker(linker)?;
    // l5i9.12.2: the TRUSTED per-run causation channel.
    wamn_postgres::add_runner_causation_to_linker(linker)?;
    // PLAN-2B (wamn-ko5r.8): the TRUSTED one-frame portable HTTP effect the
    // `http-request` node calls.
    connection_http::add_to_linker(linker)?;
    // wamn-yf3: run-path wasi:logging emission (node/run lifecycle records).
    wamn_logging::add_to_linker(linker)?;
    Ok(())
}
