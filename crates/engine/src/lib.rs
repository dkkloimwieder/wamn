//! The native Wasmtime and wash-runtime engine, with no network-service plugin.
//!
//! This crate exports:
//!
//! - [`engine`]: the production engine builder, its pooling and host-memory
//!   budgets, and the socket policy every guest runs under.
//! - [`lifecycle`]: bounded service shutdown and native liveness supervision.
//! - [`HostPlugin`]: the plugin trait. It is wash-runtime's trait, re-exported.
//!   The engine owns no plugin trait of its own.
//! - [`ingress`]: HTTP authentication from workload configuration.
//! - [`native_routes`]: native host requests and operation authorization.
//! - [`entry_operation`]: the generated host side of `wamn:operation`, the
//!   entry-owned operation boundary of the ingress guest. Each host
//!   implements it over its own store.
//! - [`expected_router`]: the native HTTP router that refuses an unbound
//!   release hostname as unavailable.
//! - [`component_admission`]: pure byte admission for project components.
//! - [`invocation_trace`]: restores host-captured tracing context when native
//!   dispatch polls host callbacks.
//! - [`component_imports`]: a compiled component's ordered world imports.
//! - [`operation`]: the workload operation dispatch plugin and shared node types.
//!
//! This crate refuses to depend on:
//!
//! - Postgres: `tokio-postgres`, `deadpool-postgres`, `postgres-types`.
//! - Network services: `async-nats`, `object_store`, `redis`.
//! - OCI: `oci-client`, `oci-wasm`.
//! - HTTP clients: `hyper-util`, `hyper-rustls`, `reqwest`.
//! - OTLP gRPC: `opentelemetry-otlp`, `tonic`.
//! - Upper layers: the host service crates.
//!
//! The edge links this crate and never the host service crates, so every crate on that
//! list is a cost the edge would pay.
//! wash-runtime itself links `async-nats`, `redis`, `tonic`,
//! `opentelemetry-otlp`, `hyper-util`, and `hyper-rustls` with every feature
//! off. The owner accepted that list (finding `wamn-qt1t`), so the dependency
//! test ignores what is reached only through wash-runtime and pins that list.

pub mod application;
pub mod component_admission;
pub mod engine;
pub mod entry_operation;
pub mod expected_router;
pub mod ingress;
pub mod invocation_scope;
pub mod invocation_trace;
pub mod lifecycle;
pub mod native_routes;
pub mod operation;
mod route_bindings;

pub use engine::{
    DEFAULT_CORE_INSTANCES, HostMemoryBudgets, MEMORY_CAP_BYTES, build_engine,
    build_engine_with_host_memory, default_host_memory_budgets, host_memory_budgets,
};
pub use wash_runtime::plugin::HostPlugin;

use wash_runtime::engine::Engine;
use wash_runtime::wasmtime::component::Component;

/// Compile a component and return the interface instance type of each top-level import, in order.
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
        .map(|(name, item)| wamn_component_policy::instance_type(name, item.implements).to_owned());
    Ok(wamn_component_policy::ComponentImports::new(imports))
}
