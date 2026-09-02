//! Migration-IR projections materialized in the Receiving package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `purchase_order` projection and statement digests.
    pub(crate) mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/purchase_order.rs"
        ));
    }

    /// Generated `receipt` projection and statement digests.
    pub(crate) mod receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/receipt.rs"
        ));
    }

    /// Generated `receiving.record_receipt` transaction accessors.
    pub(crate) mod receiving_record_receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/receiving_record_receipt.rs"
        ));
    }
}
