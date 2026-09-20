//! Application integration tests.

#[cfg(test)]
mod acme_overlay_publication;

#[test]
fn committed_sqlx_metadata_compiles_offline() {
    wamn_schema_generator::verify_sqlx_metadata(
        wamn_schema_generator::SqlxMetadataMode::Compile,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap(),
    )
    .expect("the application SQL corpus compiles against committed metadata");
}
