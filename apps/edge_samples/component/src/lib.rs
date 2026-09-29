#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every edge samples operation.

mod sample;

/// The generated operations, whole.
mod generated {
    use wamn_edge_samples_data_access::generated as data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "edge-samples:component/samples@0.1.0",
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.3",
        "../generated/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;

export!(Component);
