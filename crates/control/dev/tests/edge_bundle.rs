//! The edge bundle carries the composed entry, pinned by its digest.
//!
//! Run `wamn build apps/edge_device` first, and set WAMN_FLOW_HTTP_COMPONENT
//! to the pinned http-route guest. See docs/operations/running-tests.md.

use std::path::{Path, PathBuf};

use wamn_catalog::edge_bundle::{BUNDLE_FILE_NAME, EdgeBundle, INGRESS_FILE_NAME};
use wamn_dev::dev::edge_bundle::{EdgeTarget, write_package};
use wamn_digest::bytes_sha256;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
#[ignore = "requires: env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_EDGE_DEVICE_COMPONENT"]
fn the_bundle_pins_the_separate_route_component() {
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_FLOW_HTTP_COMPONENT, env:WAMN_EDGE_DEVICE_COMPONENT",
    );
    let route = PathBuf::from(std::env::var_os("WAMN_FLOW_HTTP_COMPONENT").unwrap());
    let component = PathBuf::from(
        std::env::var_os("WAMN_EDGE_DEVICE_COMPONENT")
            .expect("set WAMN_EDGE_DEVICE_COMPONENT to the built edge_device component"),
    );
    assert!(component.is_file(), "run wamn build apps/edge_device first");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("edge-bundle-entry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    write_package(
        &repository().join("apps/edge_device"),
        "device",
        &component,
        &route,
        &EdgeTarget {
            org: "acme",
            project: "plant",
            environment: "dev",
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
