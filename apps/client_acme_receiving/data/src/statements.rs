//! Package-generator output carrying only admitted statement digests, for the
//! custom operations. The generated operations include their own, in
//! [`crate::generated`].

pub(crate) mod quality_approve_inspection {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/quality_approve_inspection.rs"
    ));
}

pub(crate) mod quality_create_inspection {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/quality_create_inspection.rs"
    ));
}

pub(crate) mod quality_load_purchase_order_detail {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/quality_load_purchase_order_detail.rs"
    ));
}

pub(crate) mod receiving_record_receipt_participant {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/receiving_record_receipt_participant.rs"
    ));
}
