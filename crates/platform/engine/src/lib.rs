//! The native Wasmtime and wash-runtime engine, with no network-service plugin.
//!
//! This crate exports:
//!
//! - [`engine`]: the production engine builder, its pooling and host-memory
//!   budgets, and the socket policy every guest runs under.
//! - [`lifecycle`]: bounded service shutdown and native liveness supervision.
//! - [`HostPlugin`]: the plugin trait. It is wash-runtime's trait, re-exported.
//!   The engine owns no plugin trait of its own.
//! - [`component_admission`]: pure byte admission for tenant components.
//! - [`component_artifact`]: the wire contract for digest-addressed component
//!   artifacts.
//! - [`release_manifest`]: a release manifest loaded once for the process.
//! - [`invocation_trace`]: restores host-captured tracing context when native
//!   dispatch polls host callbacks.
//! - [`component_imports`]: a compiled component's ordered world imports.
//!
//! This crate refuses to depend on:
//!
//! - Postgres: `tokio-postgres`, `deadpool-postgres`, `postgres-types`.
//! - Network services: `async-nats`, `object_store`, `redis`.
//! - OCI: `oci-client`, `oci-wasm`.
//! - HTTP clients: `hyper-util`, `hyper-rustls`, `reqwest`.
//! - OTLP gRPC: `opentelemetry-otlp`, `tonic`.
//! - Upper layers: `wamn-router`, `wamn-runtime`.
//!
//! The edge links this crate and never `wamn-runtime`, so every crate on that
//! list is a cost the edge would pay. `wamn-router` is on it because
//! routes-router rule 7 keeps the router out of the base platform and the edge.
//! wash-runtime itself links `async-nats`, `redis`, `tonic`,
//! `opentelemetry-otlp`, `hyper-util`, and `hyper-rustls` with every feature
//! off. The owner accepted that list (finding `wamn-qt1t`), so the dependency
//! test ignores what is reached only through wash-runtime and pins that list.

pub mod component_admission;
pub mod component_artifact;
pub mod engine;
pub mod invocation_trace;
pub mod lifecycle;
pub mod release_manifest;

pub use engine::{
    DEFAULT_CORE_INSTANCES, HostMemoryBudgets, MEMORY_CAP_BYTES, build_engine,
    build_engine_with_host_memory, default_host_memory_budgets, host_memory_budgets,
};
pub use wash_runtime::plugin::HostPlugin;

use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;

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
