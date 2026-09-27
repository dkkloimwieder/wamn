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

    /// Generated `sample.record` accessor.
    pub(crate) mod sample_record {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/sample_record.rs"
        ));
    }
}
