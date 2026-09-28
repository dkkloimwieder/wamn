// @generated from the package manifest and migration IR; do not edit.

// The generated data functions and their refusal.

/// The one refusal of every generated operation.
pub mod error {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/error.rs"
    ));
}

/// The generated `inventory_transaction` operations.
pub mod inventory_transaction {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/inventory_transaction.rs"
    ));
}

/// The generated `location` operations.
pub mod location {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/location.rs"
    ));
}

/// The generated `packaging` operations.
pub mod packaging {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/packaging.rs"
    ));
}

/// The generated `packaging_quantity` operations.
pub mod packaging_quantity {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/packaging_quantity.rs"
    ));
}

/// The generated `product` operations.
pub mod product {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/product.rs"
    ));
}
