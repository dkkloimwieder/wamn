//! The §3 check A7 of `docs/plan/platform-deploy.md` (`wamn-snz0.5`): one
//! release published for two environments hashes to one digest.
//!
//! The serving manifest names no tenant, environment, route host or integer
//! id (R1), so the same packages give the same bytes in every environment.
//! The kind half of A7, a request to each environment's route host, runs in
//! the `wamn-snz0.3` cluster run.

use std::path::{Path, PathBuf};
use std::process::Command;

use wamn_catalog::edge_bundle::COMPONENTS_FILE_NAME;
use wamn_catalog::{AdmittedComponent, RELEASE_MANIFEST_FILE_NAME, ServingManifest};
use wamn_control::dev::edge_bundle::{EdgeScope, PackageRelease, write_package};

const ORG: &str = "acme";
const PROJECT: &str = "plant";
const PUBLISHER: &str = "0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Build `apps/edge_device` with the repository's guest build and return its
/// component.
fn device_component() -> PathBuf {
    let repository = repository();
    let output = Command::new(repository.join("tools/build-components"))
        .current_dir(&repository)
        .args(["app", "apps/edge_device"])
        .output()
        .expect("start tools/build-components");
    assert!(
        output.status.success(),
        "tools/build-components failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    repository.join("apps/target/virtualized/std-empty-environment/device.wasm")
}

/// Admit the component and assemble the release of `apps/edge_device` for
/// `environment`, as publish projects it, and return its manifest bytes and
/// admitted component facts.
fn publish(
    environment: &str,
    component: &Path,
    ingress: &Path,
    root: &Path,
) -> (Vec<u8>, Vec<AdmittedComponent>) {
    let out = root.join(environment);
    let _ = std::fs::remove_dir_all(&out);
    write_package(
        &repository().join("apps/edge_device"),
        "device",
        component,
        ingress,
        &PackageRelease {
            scope: EdgeScope {
                org: ORG,
                project: PROJECT,
                environment,
            },
            publisher: PUBLISHER,
        },
        &out,
    )
    .expect("publish the release for the environment");
    let manifest = std::fs::read(out.join(RELEASE_MANIFEST_FILE_NAME)).expect("read the manifest");
    let facts = serde_json::from_slice(
        &std::fs::read(out.join(COMPONENTS_FILE_NAME)).expect("read the component facts"),
    )
    .expect("parse the component facts");
    (manifest, facts)
}

#[test]
fn one_release_published_for_two_environments_has_one_digest() {
    let component = device_component();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("release-one-digest-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create the scratch directory");
    // The bundle copies the ingress guest without reading it, and the manifest
    // does not depend on it.
    let ingress = root.join("ingress.wasm");
    std::fs::write(&ingress, b"ingress").expect("write the ingress stand-in");

    let (dev, dev_facts) = publish("dev", &component, &ingress, &root);
    let (prod, prod_facts) = publish("prod", &component, &ingress, &root);

    // The two publishes ran in two scopes: each admitted the component for
    // its own environment's tenant.
    assert!(
        dev_facts
            .iter()
            .all(|fact| fact.scope.tenant_id == "acme--plant--dev")
    );
    assert!(
        prod_facts
            .iter()
            .all(|fact| fact.scope.tenant_id == "acme--plant--prod")
    );

    let (dev_manifest, dev_digest) =
        ServingManifest::from_canonical_bytes(&dev).expect("the dev release is canonical");
    let (_, prod_digest) =
        ServingManifest::from_canonical_bytes(&prod).expect("the prod release is canonical");
    println!("A7 dev digest {dev_digest}");
    println!("A7 prod digest {prod_digest}");
    assert_eq!(dev_digest, prod_digest, "one release has one digest");
    assert_eq!(dev, prod, "one release has one canonical encoding");
    assert!(
        !dev_manifest.attachments.is_empty(),
        "the release serves a route, so the check covers the route host"
    );

    let text = String::from_utf8(dev).expect("canonical JSON is UTF-8");
    for absent in [
        "acme--plant",
        "\"tenant-id\"",
        "\"environment\"",
        "\"effective-release-id\"",
        "\"host\"",
    ] {
        assert!(!text.contains(absent), "the release names {absent}");
    }
    let _ = std::fs::remove_dir_all(&root);
}
