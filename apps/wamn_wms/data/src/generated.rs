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
        reason = "an adjust reads the locked packaging's row_version and status, the transaction id, the quantity its update returns, and the revision its touch returns; it leaves the locked packaging's location_id, which only a move consults, the balance its existence read selects, which the transaction insert compares in SQL, and the ids its update and delete return"
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
        reason = "a merge leaves the id and quantity of both the target quantity row it updates and the one it inserts, the ids of the source balance rows it deletes, and the row_version the consumed source returns. Each statement must return something. Of the two packagings it locks in id order it reads id, row_version and status, not location_id"
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
        reason = "validate_location is an existence probe: `SELECT id FROM location WHERE id = $1` must name a column, but the move asks only whether the destination is there and tests the Option, never the id. The move also leaves the status its update returns"
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
        reason = "of the new packaging the split reads only the id create_packaging returns. It leaves the id and quantity of the quantity row it takes from and the one it places, and the id its own existence probe must select to ask whether the destination location is there. Of the source packaging it locks it reads row_version and status, not location_id"
    )]
    pub(crate) mod inventory_split {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_split.rs"
        ));
    }

    /// Generated `inventory_transaction` projection and statement digests.
    pub(crate) mod inventory_transaction {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_transaction.rs"
        ));
    }

    /// Generated `location` projection and accessors.
    #[expect(
        dead_code,
        reason = "the create and the update map no exclusion constraint"
    )]
    pub(crate) mod location {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/location.rs"
        ));
    }

    /// Generated `packaging` projection and accessors.
    #[expect(dead_code, reason = "the create maps no exclusion constraint")]
    pub(crate) mod packaging {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/packaging.rs"
        ));
    }

    /// Generated `packaging_quantity` projection and statement digests.
    pub(crate) mod packaging_quantity {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/packaging_quantity.rs"
        ));
    }

    /// Generated `product` projection and accessors.
    #[expect(
        dead_code,
        reason = "the create and the update map no exclusion constraint"
    )]
    pub(crate) mod product {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/product.rs"
        ));
    }
}
