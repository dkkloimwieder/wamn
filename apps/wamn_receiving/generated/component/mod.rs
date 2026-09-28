// @generated from the package manifest; do not edit.

// The generated handlers. The including module names its data crate's
// generated module as `data`.

/// The generated `purchase_order` handlers.
pub mod purchase_order {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/purchase_order.rs"
    ));
}

/// The generated `receipt` handlers.
pub mod receipt {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/receipt.rs"
    ));
}

/// The generated `supplier` handlers.
pub mod supplier {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/supplier.rs"
    ));
}
