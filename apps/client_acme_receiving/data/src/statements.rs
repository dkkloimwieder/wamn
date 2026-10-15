//! Package-generator output carrying only admitted statement paths, for the
//! custom operations. The generated operations include their own, in
//! [`crate::generated`].

pub(crate) mod acme_record_receipt {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/client_acme_receiving/wamn/acme_record_receipt.rs"
    ));
}

pub(crate) mod quality_approve_inspection {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/client_acme_receiving/wamn/quality_approve_inspection.rs"
    ));
}

pub(crate) mod quality_create_inspection {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/client_acme_receiving/wamn/quality_create_inspection.rs"
    ));
}

pub(crate) mod quality_load_purchase_order_detail {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/client_acme_receiving/wamn/quality_load_purchase_order_detail.rs"
    ));
}
