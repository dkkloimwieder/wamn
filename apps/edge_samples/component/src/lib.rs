#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every edge samples operation.

#[expect(
    dead_code,
    reason = "private data modules retain the complete generated and authored refusal vocabulary"
)]
mod data;

mod sample;

/// The generated operations, whole.
mod generated {
    use crate::data::generated as data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/edge_samples/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "edge-samples:component/samples@0.1.0",
    path: [
        "../../../wit/wamn-node",
        "../../../wit/wamn-postgres-0.4",
        "../../target/wamn/edge_samples/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;

#[expect(
    unsafe_code,
    reason = "wit-bindgen generated bindings and exports contain unsafe FFI glue"
)]
mod component_exports {
    use super::{Component, export, exports};

    export!(Component);
}
