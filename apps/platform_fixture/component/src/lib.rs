#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every platform fixture operation.

#[expect(
    dead_code,
    reason = "private data modules retain the complete generated and authored refusal vocabulary"
)]
mod data;

mod number;
mod widget;
mod widget_maker;

use crate::data::AccessError;

/// The generated operations, whole.
mod generated {
    use crate::data::generated as data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/platform_fixture/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "platform-fixture:component/fixture@0.1.0",
    path: [
        "../../../wit/wamn-node",
        "../../../wit/wamn-postgres-0.4",
        "../../target/wamn/platform_fixture/wit",
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

/// One declared detail member of a refusal, spelled as the codecs read it.
///
/// `permission_denied` names the permission of the refused operation.
fn detail(error: &AccessError, permission: &str, key: &str) -> Option<String> {
    if key == "operation" {
        return Some(permission.to_owned());
    }
    let value = error.detail().get(key)?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
}
