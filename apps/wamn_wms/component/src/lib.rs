#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every WMS operation.

mod inventory;
mod inventory_transaction;
mod location;
mod packaging;
mod product;

wit_bindgen::generate!({
    world: "wamn:wms-component/wms@0.1.0",
    inline: r#"
        package wamn:wms-component@0.1.0;

        world wms {
          import wamn:postgres/types@0.1.0;
          import wamn:postgres/statements@0.1.0;
          export wamn-wms:inventory/adjust@1.0.0;
          export wamn-wms:inventory/aggregate@1.0.0;
          export wamn-wms:inventory/merge@1.0.0;
          export wamn-wms:inventory/move@1.0.0;
          export wamn-wms:inventory/split@1.0.0;
          export wamn-wms:inventory-transaction/get@1.0.0;
          export wamn-wms:inventory-transaction/query@1.0.0;
          export wamn-wms:location/create@1.0.0;
          export wamn-wms:location/get@1.0.0;
          export wamn-wms:location/query@1.0.0;
          export wamn-wms:location/update@1.0.0;
          export wamn-wms:packaging/create@1.0.0;
          export wamn-wms:packaging/close@1.0.0;
          export wamn-wms:packaging/relocate@1.0.0;
          export wamn-wms:packaging/get@1.0.0;
          export wamn-wms:packaging/query@1.0.0;
          export wamn-wms:inventory/get@1.0.0;
          export wamn-wms:inventory/query@1.0.0;
          export wamn-wms:product/create@1.0.0;
          export wamn-wms:product/get@1.0.0;
          export wamn-wms:product/query@1.0.0;
          export wamn-wms:product/update@1.0.0;
        }
    "#,
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres",
        "../generated/wit/deps/wamn-wms-inventory",
        "../generated/wit/deps/wamn-wms-inventory-transaction",
        "../generated/wit/deps/wamn-wms-location",
        "../generated/wit/deps/wamn-wms-packaging",
        "../generated/wit/deps/wamn-wms-product",
    ],
    generate_all,
    async: true,
});

struct Component;

export!(Component);

fn detail(error: &wamn_wms_data_access::AccessError, key: &str) -> Option<String> {
    let value = error.detail().get(key)?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
}
