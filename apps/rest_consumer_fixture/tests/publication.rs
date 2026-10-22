//! Publication refuses a consumer release without its admitted component.
#[test]
fn consumer_publication_requires_admitted_operations() {
    let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("apps/rest_consumer_fixture");
    let request = wamn_control::publish_release::PublishReleaseRequest {
        database_url: String::new(),
        control_database_url: String::new(),
        org: "rest".into(),
        tenant: "rest-publication".into(),
        environment: "test".into(),
        verified_publisher_principal: "publisher".into(),
        run_schema: "catalog".into(),
        packages: vec![
            wamn_catalog::PackageCoordinate::new("rest_consumer_fixture", "0.1.0").unwrap(),
        ],
        wirings: Vec::new(),
    };
    assert!(
        wamn_control::publish_release::assemble_local_release(
            &request,
            &[package],
            &[],
            Vec::new()
        )
        .is_err()
    );
}
