//! The edge bundle carries the composed entry, pinned by its digest.
//!
//! Run `eval "$(cargo xtask prepare-tests)"` first. It builds the edge_device
//! component and the pinned ingress guest and exports their paths.

use std::path::{Path, PathBuf};
use wamn_edge_document::edge_bundle::BUNDLE_FILE_NAME;
use wamn_edge_document::edge_bundle::EdgeBundle;
use wamn_edge_document::edge_bundle::INGRESS_FILE_NAME;

use wamn_dev::dev::edge_bundle::{EdgeTarget, write_package};
use wamn_digest::bytes_sha256;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_bundle_pins_the_separate_route_component() {
    let route = PathBuf::from(
        std::env::var_os("WAMN_INGRESS_COMPONENT")
            .expect("set WAMN_INGRESS_COMPONENT to the built ingress component"),
    );
    let component = PathBuf::from(
        std::env::var_os("WAMN_EDGE_DEVICE_COMPONENT")
            .expect("set WAMN_EDGE_DEVICE_COMPONENT to the built edge_device component"),
    );
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("edge-bundle-entry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    write_package(
        &wamn_release::release_source::LocalReleaseFiles {
            engine: &wamn_engine::component_admission::ReleaseComponentAdmission,
        },
        &repository().join("apps/edge_device"),
        &"device".parse().unwrap(),
        &component,
        &route,
        &EdgeTarget {
            org: &"acme".parse().unwrap(),
            project: &"plant".parse().unwrap(),
            environment: &"dev".parse().unwrap(),
        },
        &out,
    )
    .expect("write the edge_device bundle");

    let bundle: EdgeBundle = serde_json::from_slice(
        &std::fs::read(out.join(BUNDLE_FILE_NAME)).expect("read the bundle"),
    )
    .expect("parse the bundle");
    let entry = std::fs::read(out.join(INGRESS_FILE_NAME)).expect("read the entry");
    assert_eq!(
        bundle.ingress,
        bytes_sha256(&entry),
        "the bundle pins the entry"
    );
    let _ = std::fs::remove_dir_all(&out);
}
