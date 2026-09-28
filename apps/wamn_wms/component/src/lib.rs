#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every WMS operation.

mod inventory;

/// The generated operations, whole.
mod generated {
    use wamn_wms_data_access::generated as data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "wamn:wms-component/wms@0.1.0",
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.2",
        "../generated/wit",
        "wit",
    ],
    generate_all,
    async: true,
    // Every query ends in the same `result<query-end, query-error>`, and
    // wit-bindgen emits one future payload for structurally equal types.
    merge_structurally_equal_types: true,
});

struct Component;

export!(Component);
