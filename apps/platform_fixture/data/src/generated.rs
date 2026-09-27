//! Migration-IR projections materialized in the platform fixture package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `widget` model accessors.
    #[expect(dead_code, reason = "delete maps no constraint")]
    pub(crate) mod widget {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/widget.rs"
        ));
    }

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

    /// Generated `widget_maker` model accessors.
    pub(crate) mod widget_maker {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/widget_maker.rs"
        ));
    }

    /// Generated `widget_maker.list` accessor.
    pub(crate) mod widget_maker_list {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/widget_maker_list.rs"
        ));
    }

    /// Generated `widget_tag` model accessors.
    pub(crate) mod widget_tag {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/widget_tag.rs"
        ));
    }
}
