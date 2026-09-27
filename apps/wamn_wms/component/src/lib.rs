#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every WMS operation.

mod inventory;
mod inventory_transaction;
mod location;
mod packaging;
mod packaging_quantity;
mod product;

wit_bindgen::generate!({
    world: "wamn:wms-component/wms@0.1.0",
    inline: r#"
        package wamn:wms-component@0.1.0;

        world wms {
          import wamn:postgres/types@0.2.0;
          import wamn:postgres/statements@0.2.0;
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
          export wamn-wms:packaging/get@1.0.0;
          export wamn-wms:packaging/query@1.0.0;
          export wamn-wms:packaging-quantity/get@1.0.0;
          export wamn-wms:packaging-quantity/query@1.0.0;
          export wamn-wms:product/create@1.0.0;
          export wamn-wms:product/get@1.0.0;
          export wamn-wms:product/query@1.0.0;
          export wamn-wms:product/update@1.0.0;
        }
    "#,
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.2",
        "../generated/wit/deps/wamn-wms-inventory",
        "../generated/wit/deps/wamn-wms-inventory-transaction",
        "../generated/wit/deps/wamn-wms-location",
        "../generated/wit/deps/wamn-wms-packaging",
        "../generated/wit/deps/wamn-wms-packaging-quantity",
        "../generated/wit/deps/wamn-wms-product",
    ],
    generate_all,
    async: true,
    // Every query ends in the same `result<query-end, query-error>`, and
    // wit-bindgen emits one future payload for structurally equal types.
    merge_structurally_equal_types: true,
});

struct Component;

export!(Component);
