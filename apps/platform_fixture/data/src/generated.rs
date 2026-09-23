//! Migration-IR projections materialized in the platform fixture package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `widget` model accessors.
    #[expect(
        dead_code,
        reason = "no fixture operation selects a transaction participant, delete maps no constraint, and create and update map no exclusion constraint"
    )]
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

    /// Generated `widget.record_batch` claim accessors.
    #[expect(
        dead_code,
        reason = "record_batch selects no transaction participant, and it answers with the id that finalize_batch returns, not the one claim_batch returns"
    )]
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
}
