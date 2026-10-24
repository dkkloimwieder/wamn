//! The generated host side of `wamn:operation/operation@0.1.0`.
//!
//! The http-route guest calls this interface for each route that names a
//! sealed operation. The engine owns the binding, so every host shares one
//! [`Refusal`](operation::Refusal) and one caller type. Each host implements
//! [`operation::HostWithStore`] on its own [`HasData`] marker over
//! [`ActiveCtx`], and links it with [`operation::add_to_linker`]: the cloud
//! over PostgreSQL in `wamn-runtime`, the edge in `wamn-edge`.
//!
//! [`HasData`]: wash_runtime::wasmtime::component::HasData

use wash_runtime::engine::ctx::ActiveCtx;

pub use crate::route_bindings::wamn::operation::operation;
/// The `wamn:postgres/types` package that `operation` names for its
/// `postgres` refusal. Hosts that bind `wamn:postgres` reuse it.
pub use crate::route_bindings::wamn::postgres::types as postgres_types;

impl operation::Host for ActiveCtx<'_> {}

impl postgres_types::Host for ActiveCtx<'_> {}

// `wamn:postgres` hosts return `pg-error` as a function error, and their own
// binding displayed it this way. This world names it only inside `refusal`, so
// bindgen writes neither impl here.
impl std::fmt::Display for postgres_types::PgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for postgres_types::PgError {}
