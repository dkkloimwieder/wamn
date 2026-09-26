#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every edge samples operation.

mod sample;

wit_bindgen::generate!({
    world: "edge-samples:component/samples@0.1.0",
    inline: r#"
        package edge-samples:component@0.1.0;

        world samples {
          import wamn:postgres/types@0.2.0;
          import wamn:postgres/statements@0.2.0;
          export edge-samples:sample/get@1.0.0;
          export edge-samples:sample/read@1.0.0;
          export edge-samples:sample/%record@1.0.0;
        }
    "#,
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.2",
        "../generated/wit/deps/edge-samples-sample",
    ],
    generate_all,
    async: true,
});

struct Component;

export!(Component);
