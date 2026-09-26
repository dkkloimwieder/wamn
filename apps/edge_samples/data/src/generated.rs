//! Migration-IR projections materialized in the edge samples package.

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `sample` projection and statement digest.
    pub(crate) mod sample {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/sample.rs"
        ));
    }

    /// Generated `sample.record` claim accessors.
    ///
    /// A claim carries `select_participant` for a nested operation. No
    /// operation nests inside `sample.record`, so that accessor stays unused.
    #[expect(
        dead_code,
        reason = "the generated claim offers participation this command does not use, and the record answers with the claimed id, not the inserted row's id"
    )]
    pub(crate) mod sample_record {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/sample_record.rs"
        ));
    }
}
