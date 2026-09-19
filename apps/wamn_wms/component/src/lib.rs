#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every WMS operation.

mod inventory;
mod pallet;

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
          export wamn-wms:pallet/get@1.0.0;
          export wamn-wms:pallet/query@1.0.0;
        }
    "#,
    path: [
        "../data/wit/deps/wamn-node",
        "../data/wit/deps/wamn-postgres",
        "../generated/wit/deps/wamn-wms-inventory",
        "../generated/wit/deps/wamn-wms-pallet",
    ],
    generate_all,
    async: true,
});

struct Component;

export!(Component);
