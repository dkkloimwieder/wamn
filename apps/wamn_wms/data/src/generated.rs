//! Migration-IR projections materialized in the WMS package.
//!
//! Four of the six modules carry a `dead_code` expectation. A generated row
//! mirrors the columns its statement returns, and a command does not always
//! read all of them: an existence probe must still select something, and a
//! write that ends `RETURNING` hands back a row the caller may want only in
//! part. Decoding a column the command does not read still asserts that the
//! column exists and has the declared type, which is part of what the
//! statement contract buys, so the decode stays.
//!
//! The expectation belongs here and not in the generator. Whether a field is
//! read is a fact about this crate, and the generator sees only the catalog,
//! the manifest, and the authored SQL.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `inventory.adjust` transaction accessors.
    #[expect(
        dead_code,
        reason = "an adjust changes a quantity in place, so it reads the row_version its finalize returns, the quantity its update returns, and the locked pallet's row_version and status; it leaves the movement insert's RETURNING id, the locked pallet's location_id, which only a move consults, and the updated quantity row's id"
    )]
    pub(crate) mod inventory_adjust {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_adjust.rs"
        ));
    }

    /// Generated `inventory.aggregate` projection.
    pub(crate) mod inventory_aggregate {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_aggregate.rs"
        ));
    }

    /// Generated `inventory.merge` transaction accessors.
    #[expect(
        dead_code,
        reason = "a merge writes four rows whose whole returned payload it leaves: the movement insert's id, the row_version the source pallet's tombstone returns, and the id and quantity of both the target quantity row it updates and the one it inserts. Each statement must return something, and the merge's own confirmation comes from finalize_command. Of the two pallets it locks in id order it reads id, row_version and status, not location_id"
    )]
    pub(crate) mod inventory_merge {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_merge.rs"
        ));
    }

    /// Generated `inventory.move` transaction accessors.
    #[expect(
        dead_code,
        reason = "validate_location is an existence probe: `SELECT id FROM location WHERE id = $1` must name a column, but the move asks only whether the destination is there and tests the Option, never the id. The move also leaves the movement insert's RETURNING id, the pallet_status its finalize echoes back, the locked pallet's status, and the status of each quantity row it moves"
    )]
    pub(crate) mod inventory_move {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_move.rs"
        ));
    }

    /// Generated `inventory.split` transaction accessors.
    #[expect(
        dead_code,
        reason = "the claim mints the new pallet's id, so the split already holds every column create_pallet returns and reads none of them. It leaves the movement insert's id, the id and quantity of the quantity row it takes from and the one it places, and the id its own existence probe must select to ask whether the destination location is there. Of the source pallet it locks it reads row_version and status, not location_id"
    )]
    pub(crate) mod inventory_split {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_split.rs"
        ));
    }

    /// Generated `inventory_movement` projection and statement digests.
    pub(crate) mod inventory_movement {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_movement.rs"
        ));
    }

    /// Generated `location` projection, claim and update accessors.
    ///
    /// A claim carries `select_participant` for a nested operation. No
    /// operation nests inside `location.create`, so that accessor stays unused.
    #[expect(
        dead_code,
        reason = "the generated claim offers participation this create does not use"
    )]
    pub(crate) mod location {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/location.rs"
        ));
    }

    /// Generated `pallet` projection, claim accessors and statement digests.
    ///
    /// A claim carries `select_participant` for a nested operation. No
    /// operation nests inside `pallet.create`, so that accessor stays unused.
    #[expect(
        dead_code,
        reason = "the generated claim offers participation this create does not use"
    )]
    pub(crate) mod pallet {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/pallet.rs"
        ));
    }

    /// Generated `pallet_quantity` projection and statement digests.
    pub(crate) mod pallet_quantity {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/pallet_quantity.rs"
        ));
    }

    /// Generated `product` projection, claim and update accessors.
    ///
    /// A claim carries `select_participant` for a nested operation. No
    /// operation nests inside `product.create`, so that accessor stays unused.
    #[expect(
        dead_code,
        reason = "the generated claim offers participation this create does not use"
    )]
    pub(crate) mod product {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/product.rs"
        ));
    }
}
