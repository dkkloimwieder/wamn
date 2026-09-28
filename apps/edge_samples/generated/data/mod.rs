// @generated from the package manifest and migration IR; do not edit.

// The generated data functions and their refusal.

/// The one refusal of every generated operation.
pub mod error {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/error.rs"
    ));
}

/// The generated `sample` operations.
pub mod sample {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/sample.rs"
    ));
}
