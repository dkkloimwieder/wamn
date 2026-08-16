//! Shared native runtime configuration and wasmCloud host capability adapters.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.

pub mod connection_authority;
pub mod connection_generation;
pub mod engine;
pub mod flow_invocation;
pub mod memory_metrics;
pub mod plugins;

use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;

pub use engine::{
    DEFAULT_EPOCH_TICK, MEMORY_CAP_BYTES, advertise_memory_ceiling, build_engine,
    spawn_epoch_ticker,
};

/// Compile a component and return its ordered top-level world imports.
pub fn component_imports(
    engine: &Engine,
    wasm: &[u8],
    label: &str,
) -> anyhow::Result<wamn_component_policy::ComponentImports> {
    let component = Component::new(engine.inner(), wasm)
        .map_err(|error| anyhow::anyhow!("compile {label}: {error}"))?;
    let raw = component.engine();
    let component_type = component.component_type();
    let imports = component_type
        .imports(raw)
        .map(|(name, _)| name.to_string());
    Ok(wamn_component_policy::ComponentImports::new(imports))
}
