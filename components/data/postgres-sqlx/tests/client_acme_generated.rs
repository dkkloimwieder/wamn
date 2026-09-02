//! External-consumer compile proof for Acme's generated `WamnPostgres` accessors.

#[expect(
    dead_code,
    reason = "compile-only proof owns generated rows and typed accessor functions"
)]
mod generated {
    pub mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/client_acme_receiving/generated/wamn/purchase_order.rs"
        ));
    }

    pub mod quality_approve_inspection {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/client_acme_receiving/generated/wamn/quality_approve_inspection.rs"
        ));
    }

    pub mod quality_create_inspection {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/client_acme_receiving/generated/wamn/quality_create_inspection.rs"
        ));
    }

    pub mod quality_load_purchase_order_detail {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/client_acme_receiving/generated/wamn/quality_load_purchase_order_detail.rs"
        ));
    }
}

#[test]
fn generated_wamn_accessors_compile_as_an_external_consumer() {
    let _: &str = generated::purchase_order::GET_SQL;
    let _: &str = generated::purchase_order::UPDATE_SQL;
    let _: &str = generated::quality_load_purchase_order_detail::LOAD_PURCHASE_ORDER_DETAIL_SQL;
    let _: &str = generated::quality_approve_inspection::APPROVE_INSPECTION_SQL;
    let _: &str = generated::quality_create_inspection::INSERT_INSPECTION_SQL;
    let _: &str = generated::quality_create_inspection::LOAD_INSPECTION_SQL;
}
