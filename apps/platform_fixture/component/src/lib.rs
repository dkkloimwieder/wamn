#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every platform fixture operation.

mod widget;
mod widget_maker;
mod widget_tag;

use wamn_platform_fixture_data_access::AccessError;

wit_bindgen::generate!({
    world: "platform-fixture:component/fixture@0.1.0",
    inline: r#"
        package platform-fixture:component@0.1.0;

        world fixture {
          import wamn:postgres/types@0.1.0;
          import wamn:postgres/statements@0.1.0;
          export platform-fixture:widget/archive@1.0.0;
          export platform-fixture:widget/create@1.0.0;
          export platform-fixture:widget/delete@1.0.0;
          export platform-fixture:widget/get@1.0.0;
          export platform-fixture:widget/%list@1.0.0;
          export platform-fixture:widget/query@1.0.0;
          export platform-fixture:widget/record-batch@1.0.0;
          export platform-fixture:widget/update@1.0.0;
          export platform-fixture:widget-maker/get@1.0.0;
          export platform-fixture:widget-maker/%list@1.0.0;
          export platform-fixture:widget-maker/query@1.0.0;
          export platform-fixture:widget-tag/update@1.0.0;
        }
    "#,
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres",
        "../generated/wit/deps/platform-fixture-widget",
        "../generated/wit/deps/platform-fixture-widget-maker",
        "../generated/wit/deps/platform-fixture-widget-tag",
    ],
    generate_all,
    async: true,
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
