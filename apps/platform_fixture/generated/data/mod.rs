// @generated from the package manifest and migration IR; do not edit.

// The generated data functions and their refusal.

/// The one refusal of every generated operation.
pub mod error {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/error.rs"
    ));
}

/// The generated `widget` operations.
pub mod widget {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/widget.rs"
    ));
}

/// The generated `widget_maker` operations.
pub mod widget_maker {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/widget_maker.rs"
    ));
}

/// The generated `widget_tag` operations.
pub mod widget_tag {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/data/widget_tag.rs"
    ));
}
