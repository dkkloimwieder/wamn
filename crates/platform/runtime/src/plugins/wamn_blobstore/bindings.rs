//! Host-side bindings for `wasmcloud:blobstore@0.1.0`.
//!
//! Generated from the WIT vendored at `runtime/wit/deps/wasmcloud-blobstore/`,
//! following the `wamn_postgres` pattern: WAMN owns this interface, generates
//! its own bindings, and needs nothing from upstream's `wasi_blobstore`
//! module — which is not even compiled, since the `wasi-blobstore` cargo
//! feature stays off (wamn-jpxo).
//!
//! # The generated surface, measured
//!
//! `bindgen!` with `imports: { default: async }` splits the 16 contract
//! methods across three traits. Recorded here because the split is not
//! obvious from the WIT, and the next implementer should not have to
//! rediscover it by probing the compiler:
//!
//! * `container::HostContainer` — `drop` only, and it is **async**.
//! * `container::HostContainerWithStore<D>` — the ten container methods:
//!   `name`, `info`, `get_data`, `write_data`, `list_objects`,
//!   `delete_object`, `delete_objects`, `has_object`, `object_info`, `clear`.
//! * `blobstore::HostWithStore<D>` — the six store methods:
//!   `create_container`, `get_container`, `delete_container`,
//!   `container_exists`, `copy_object`, `move_object`.
//!
//! The two `*WithStore<D>` traits carry the concurrent, stream-bearing methods
//! and require `Self: HasData`, so the implementation attaches to the store
//! context the way `wamn_postgres` attaches to `ActiveCtx`.
//!
//! `copy_object` and `move_object` are the **refused verbs** (owner ruling):
//! they take bare `object-id` strings carrying no backend or binding
//! discriminator, so bucket/prefix confinement cannot hold across them. If
//! demand appears they return as binding-scoped variants, never these
//! signatures.
#![allow(dead_code)]

wash_runtime::wasmtime::component::bindgen!({
    world: "blobstore-plugin",
    imports: { default: async | trappable | tracing },
    with: {
        "wasmcloud:blobstore/container.container": super::store::BoundContainer,
    },
    wasmtime_crate: wash_runtime::wasmtime,
});
