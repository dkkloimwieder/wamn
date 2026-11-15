//! The statement accessor of the `sample.record` command, materialized in the
//! package. The generated operation includes its own, in [`crate::data::generated`].

/// Generated accessors carrying SQL for the host transaction.
pub(crate) mod wamn {
    /// Generated `sample.record` accessor.
    pub(crate) mod sample_record {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wamn/edge_samples/wamn/sample_record.rs"
        ));
    }
}
