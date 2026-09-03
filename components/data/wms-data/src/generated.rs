//! Migration-IR projections materialized in the WMS package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `inventory.move` transaction accessors.
    pub(crate) mod inventory_move {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/wms/generated/wamn/inventory_move.rs"
        ));
    }
}
