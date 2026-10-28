//! The statement accessors of the WMS commands, materialized in the package.
//! The generated operations include their own, in [`crate::generated`].

/// Runtime projections carrying only admitted statement paths.
pub(crate) mod wamn {
    /// Generated `inventory.adjust` transaction accessors.
    pub(crate) mod inventory_adjust {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wamn/wamn_wms/wamn/inventory_adjust.rs"
        ));
    }

    /// Generated `inventory.aggregate` projection.
    pub(crate) mod inventory_aggregate {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wamn/wamn_wms/wamn/inventory_aggregate.rs"
        ));
    }

    /// Generated `inventory.merge` transaction accessors.
    pub(crate) mod inventory_merge {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wamn/wamn_wms/wamn/inventory_merge.rs"
        ));
    }

    /// Generated `inventory.move` transaction accessors.
    #[expect(
        dead_code,
        reason = "validate_location is an existence probe: `SELECT id FROM location WHERE id = $1` must name a column, but the move asks only whether the destination is there and tests the Option, never the id. The move also leaves the status its update returns"
    )]
    pub(crate) mod inventory_move {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wamn/wamn_wms/wamn/inventory_move.rs"
        ));
    }

    /// Generated `inventory.split` transaction accessors.
    pub(crate) mod inventory_split {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wamn/wamn_wms/wamn/inventory_split.rs"
        ));
    }
}
