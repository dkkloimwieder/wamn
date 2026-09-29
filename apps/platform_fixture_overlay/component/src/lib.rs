#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component for the platform fixture overlay.
//!
//! The overlay declares one operation, `widget.get`, which reads the field the
//! overlay adds. It is generated whole, and the package has no data-access
//! crate, so the component includes the generated data functions itself.

/// The generated operations, whole, with their data functions.
mod generated {
    #[expect(
        dead_code,
        reason = "the generated refusal is the whole closed vocabulary, and a get constructs part of it"
    )]
    #[expect(
        clippy::enum_variant_names,
        reason = "the generated refusal spells each variant as its contract literal, and here it is private"
    )]
    mod data {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/data/mod.rs"
        ));
    }
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "platform-fixture-overlay:component/fixture-overlay@0.1.0",
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
