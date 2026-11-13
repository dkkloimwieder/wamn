#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every WMS operation.
#![expect(unsafe_code, reason = "wit-bindgen generated FFI glue")]
#[expect(
    dead_code,
    reason = "private data modules retain the complete generated and authored refusal vocabulary"
)]
mod data;

mod inventory;

/// The generated operations, whole.
mod generated {
    use crate::data::generated as data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/wamn_wms/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "wamn:wms-component/wms@0.1.0",
    path: [
        "../../../wit/wamn-node",
        "../../../wit/wamn-postgres-0.4",
        "../../target/wamn/wamn_wms/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;

impl generated::inventory_transaction::get::Action for Component {}
impl generated::inventory_transaction::query::Action for Component {}
impl generated::location::get::Action for Component {}
impl generated::location::query::Action for Component {}
impl generated::location::create::Action for Component {}
impl generated::location::update::Action for Component {}
impl generated::packaging::get::Action for Component {}
impl generated::packaging::query::Action for Component {}
impl generated::packaging::create::Action for Component {}
impl generated::packaging_quantity::get::Action for Component {}
impl generated::packaging_quantity::query::Action for Component {}
impl generated::product::get::Action for Component {}
impl generated::product::query::Action for Component {}
impl generated::product::create::Action for Component {}
impl generated::product::update::Action for Component {}

export!(Component);
