//! Package-generator output consumed without an authored SQL copy.

pub(crate) mod purchase_order {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../packages/client_acme_receiving/generated/wamn/purchase_order.rs"
    ));
}

const _: &[&str] = purchase_order::UPDATE_UNIQUE_CONSTRAINTS;
const _: &[&str] = purchase_order::UPDATE_FOREIGN_KEY_CONSTRAINTS;
const _: &[&str] = purchase_order::UPDATE_CHECK_CONSTRAINTS;

pub(crate) mod quality_approve_inspection {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../packages/client_acme_receiving/generated/wamn/quality_approve_inspection.rs"
    ));
}

pub(crate) mod quality_create_inspection {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../packages/client_acme_receiving/generated/wamn/quality_create_inspection.rs"
    ));
}

pub(crate) mod quality_load_purchase_order_detail {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../packages/client_acme_receiving/generated/wamn/quality_load_purchase_order_detail.rs"
    ));
}

#[expect(
    dead_code,
    reason = "the shared confirmation projection returns base fields while this BFF appends only its two overlay fields"
)]
pub(crate) mod receiving_record_receipt {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../packages/client_acme_receiving/generated/wamn/receiving_record_receipt.rs"
    ));
}
