#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting the edge device operation. It
//! imports no `wamn:postgres`, so an edge box can load it.

mod sample;

wit_bindgen::generate!({
    world: "edge-device:component/device@0.1.0",
    inline: r#"
        package edge-device:component@0.1.0;

        world device {
          export edge-device:sample/read@1.0.0;
        }
    "#,
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../generated/wit/deps/edge-device-sample",
    ],
    generate_all,
    async: true,
});

struct Component;

export!(Component);
