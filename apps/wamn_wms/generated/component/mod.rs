// @generated from the package manifest; do not edit.

// The generated handlers. The including module names its data crate's
// generated module as `data`.

/// The generated `inventory_transaction` handlers.
pub mod inventory_transaction {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/inventory_transaction.rs"
    ));
}

/// The generated `location` handlers.
pub mod location {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/location.rs"
    ));
}

/// The generated `packaging` handlers.
pub mod packaging {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/packaging.rs"
    ));
}

/// The generated `packaging_quantity` handlers.
pub mod packaging_quantity {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/packaging_quantity.rs"
    ));
}

/// The generated `product` handlers.
pub mod product {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/product.rs"
    ));
}
