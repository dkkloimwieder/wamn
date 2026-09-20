//! Application integration tests.

#[cfg(test)]
mod wms_publication;
#[cfg(test)]
mod wms_runtime_live;
#[cfg(test)]
mod wms_wiring_shape;

#[cfg(test)]
mod cluster;
#[cfg(test)]
mod delivery;
#[cfg(test)]
mod environment;

#[cfg(test)]
mod business_fixture;

#[cfg(test)]
mod local_business;

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
