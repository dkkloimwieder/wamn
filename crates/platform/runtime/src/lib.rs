//! Shared native runtime configuration and wasmCloud host capability adapters.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.

pub mod component_admission;
pub mod component_artifact;
pub mod component_artifact_source;
pub mod connection_authority;
pub mod connection_generation;
pub mod engine;
pub mod plugins;
pub mod registry_credentials;
pub mod release_manifest;
pub mod release_manifest_artifact;
pub mod release_manifest_source;
pub mod wiring_doorbell;
pub mod wiring_lowering;

use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;

pub use engine::{
    DEFAULT_CORE_INSTANCES, HostMemoryBudgets, MEMORY_CAP_BYTES, build_engine,
    build_engine_with_host_memory, default_host_memory_budgets, host_memory_budgets,
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
