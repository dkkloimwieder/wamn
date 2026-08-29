//! Migration-IR projections materialized in the Receiving package.

/// Runtime projections and exact SQL references for `WamnPostgres`.
pub(crate) mod wamn {
    /// Generated `purchase_order` projection and SQL references.
    pub(crate) mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/purchase_order.rs"
        ));
    }

    /// Generated `receipt` projection and SQL references.
    pub(crate) mod receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/receiving/generated/wamn/receipt.rs"
        ));
    }
}
