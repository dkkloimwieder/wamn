// @generated from the package manifest; do not edit.

// The generated handlers. The including module names its data crate's
// generated module as `data`.

/// The generated `widget` handlers.
pub mod widget {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/widget.rs"
    ));
}

/// The generated `widget_maker` handlers.
pub mod widget_maker {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/widget_maker.rs"
    ));
}

/// The generated `widget_tag` handlers.
pub mod widget_tag {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/widget_tag.rs"
    ));
}
