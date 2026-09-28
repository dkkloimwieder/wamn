//! The statement accessors of the Receiving custom operations, materialized in
//! the package. The generated operations include their own, in
//! [`crate::generated`].

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `location.list` projection.
    pub(crate) mod location_list {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/location_list.rs"
        ));
    }

    /// Generated `receiving.record_receipt` transaction accessors.
    pub(crate) mod receiving_record_receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/receiving_record_receipt.rs"
        ));
    }

    /// Generated `receiving.load_receipt_screen` projection.
    pub(crate) mod receiving_load_receipt_screen {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/receiving_load_receipt_screen.rs"
        ));
    }

    /// Generated `receiving.load_purchase_order_history` projection.
    pub(crate) mod receiving_load_purchase_order_history {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/receiving_load_purchase_order_history.rs"
        ));
    }
}
