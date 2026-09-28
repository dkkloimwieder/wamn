//! The statement accessor of the `sample.record` command, materialized in the
//! package. The generated operation includes its own, in [`crate::generated`].

/// Runtime projections carrying only admitted statement digests.
pub(crate) mod wamn {
    /// Generated `sample.record` accessor.
    pub(crate) mod sample_record {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../generated/wamn/sample_record.rs"
        ));
    }
}
