//! Migration-IR projections materialized in the WMS package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `inventory.adjust` transaction accessors.
    pub(crate) mod inventory_adjust {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/wms/generated/wamn/inventory_adjust.rs"
        ));
    }

    /// Generated `inventory.aggregate` projection.
    pub(crate) mod inventory_aggregate {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/wms/generated/wamn/inventory_aggregate.rs"
        ));
    }

    /// Generated `inventory.merge` transaction accessors.
    pub(crate) mod inventory_merge {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/wms/generated/wamn/inventory_merge.rs"
        ));
    }

    /// Generated `inventory.move` transaction accessors.
    pub(crate) mod inventory_move {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/wms/generated/wamn/inventory_move.rs"
        ));
    }

    /// Generated `inventory.split` transaction accessors.
    pub(crate) mod inventory_split {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/wms/generated/wamn/inventory_split.rs"
        ));
    }

    /// Generated `pallet` projection and statement digests.
    pub(crate) mod pallet {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/wms/generated/wamn/pallet.rs"
        ));
    }
}
