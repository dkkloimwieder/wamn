#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every platform fixture operation.

mod widget;
mod widget_maker;

use wamn_platform_fixture_data_access::AccessError;

/// The generated operations, whole.
mod generated {
    use wamn_platform_fixture_data_access::generated as data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "platform-fixture:component/fixture@0.1.0",
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
