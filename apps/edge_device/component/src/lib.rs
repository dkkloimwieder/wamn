#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting the edge device operation. It
//! imports no `wamn:postgres`, so an edge box can load it.
#![cfg_attr(
    target_arch = "wasm32",
    expect(unsafe_code, reason = "wit-bindgen generated FFI glue")
)]
mod sample;

wit_bindgen::generate!({
    world: "edge-device:component/device@0.1.0",
    path: [
        "../../../wit/wamn-node",
        "../../target/wamn/edge_device/wit",
        "wit/authored.wit",
    ],
    generate_all,
    async: true,
});

struct Component;

#[cfg(target_arch = "wasm32")]
export!(Component);
