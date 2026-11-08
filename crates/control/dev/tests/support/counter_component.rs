#![expect(unsafe_code, reason = "wit-bindgen generated FFI glue")]
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

mod data {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/probe/data/mod.rs"
    ));
}

mod generated {
    use crate::data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/probe/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "probe:component/fixture@0.1.0",
    path: [
        "../../../wit/wamn-node",
        "../../../wit/wamn-postgres-0.4",
        "../../target/wamn/probe/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;

export!(Component);
