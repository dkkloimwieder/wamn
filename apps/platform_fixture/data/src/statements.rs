//! Migration-IR projections of the custom operations of the platform fixture.
//!
//! The generated operations include their own accessors, in
//! [`crate::generated`].

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `widget.archive` accessor.
    pub(crate) mod widget_archive {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/widget_archive.rs"
        ));
    }

    /// Generated `widget.list` accessor.
    pub(crate) mod widget_list {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/widget_list.rs"
        ));
    }

    /// Generated `widget.record_batch` accessor.
    pub(crate) mod widget_record_batch {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/widget_record_batch.rs"
        ));
    }

    /// Generated `widget_maker.list` accessor.
    pub(crate) mod widget_maker_list {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/widget_maker_list.rs"
        ));
    }
}
