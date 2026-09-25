//! Generated SQL accessors.
pub(crate) mod wamn {
    #[expect(
        dead_code,
        reason = "generated SQL rows include fields used only by other access paths"
    )]
    pub(crate) mod inventory_move {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_move.rs"
        ));
    }
    #[expect(
        dead_code,
        reason = "generated SQL rows include fields used only by other access paths"
    )]
    pub(crate) mod inventory_adjust {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_adjust.rs"
        ));
    }
    #[expect(
        dead_code,
        reason = "generated SQL rows include fields used only by other access paths"
    )]
    pub(crate) mod inventory_split {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_split.rs"
        ));
    }
    #[expect(
        dead_code,
        reason = "generated SQL rows include fields used only by other access paths"
    )]
    pub(crate) mod inventory_merge {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_merge.rs"
        ));
    }
    #[expect(
        dead_code,
        reason = "generated SQL rows include fields used only by other access paths"
    )]
    pub(crate) mod packaging_create {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/packaging_create.rs"
        ));
    }
    #[expect(
        dead_code,
        reason = "generated SQL rows include fields used only by other access paths"
    )]
    pub(crate) mod packaging_close {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/packaging_close.rs"
        ));
    }
    pub(crate) mod inventory_aggregate {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_aggregate.rs"
        ));
    }
    pub(crate) mod inventory {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory.rs"
        ));
    }
    pub(crate) mod packaging {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/packaging.rs"
        ));
    }
    pub(crate) mod inventory_transaction {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/inventory_transaction.rs"
        ));
    }
    #[expect(
        dead_code,
        reason = "generated SQL rows include fields used only by other access paths"
    )]
    pub(crate) mod location {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/location.rs"
        ));
    }
    #[expect(
        dead_code,
        reason = "generated SQL rows include fields used only by other access paths"
    )]
    pub(crate) mod product {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/product.rs"
        ));
    }
}
