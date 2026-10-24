//! The edge bundle carries the composed entry, pinned by its digest.
//!
//! Run `wamn build apps/edge_device` first, and build the pinned http-route
//! guest as the edge tests do.

use std::path::{Path, PathBuf};

use wamn_catalog::edge_bundle::{BUNDLE_FILE_NAME, EdgeBundle, INGRESS_FILE_NAME, file_digest};
use wamn_control::dev::edge_bundle::{EdgeScope, PackageRelease, write_package};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
fn the_bundle_pins_the_entry_composed_from_the_route_guest_and_the_application() {
    let route = repository().join("apps/target/wasm32-wasip2/release/http_route.wasm");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("edge-bundle-entry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    write_package(
        &repository().join("apps/edge_device"),
        "device",
        &repository().join("apps/target/wamn/edge_device/components/device.wasm"),
        &route,
        &PackageRelease {
            scope: EdgeScope {
                org: "acme",
                project: "plant",
                environment: "dev",
            },
            publisher: "0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b",
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
        file_digest(&entry),
        "the bundle pins the entry"
    );
    assert_ne!(
        entry,
        std::fs::read(&route).expect("read the route guest"),
        "the bundle carries the composed entry, not the route guest"
    );
    wamn_component_composer::validate_application_dispatch(&entry)
        .expect("the entry supplies application dispatch inside the composition");
    let _ = std::fs::remove_dir_all(&out);
}
