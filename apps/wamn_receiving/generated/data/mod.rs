// @generated from the package manifest and migration IR; do not edit.

// The generated data functions and their refusal.

/// The one refusal of every generated operation.
pub mod error {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/error.rs"
    ));
}

/// The generated `purchase_order` operations.
pub mod purchase_order {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/purchase_order.rs"
    ));
}

/// The generated `receipt` operations.
pub mod receipt {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/receipt.rs"
    ));
}

/// The generated `supplier` operations.
pub mod supplier {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/supplier.rs"
    ));
}
