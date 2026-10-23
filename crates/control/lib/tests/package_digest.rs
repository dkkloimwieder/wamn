//! Every package that a committed GKE environment names packs to the digest
//! in its committed `package.digest` (wamn-vl6im). The GKE registry tags are
//! immutable, so a package that changes under an unchanged version refuses
//! at step 4.13. This test finds it first. Run `wamn build` on each package
//! before it.

use std::path::{Path, PathBuf};

use wamn_control::package_artifact::check_package_digest;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
fn every_gke_package_packs_to_its_committed_digest() {
    let environments = repository().join("deploy/gcp/environments");
    let mut packages = Vec::new();
    for entry in std::fs::read_dir(&environments).expect("read deploy/gcp/environments") {
        let path = entry.expect("read an environment entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let file: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).expect("read the environment file"))
                    .expect("decode the environment file");
            for package in file["packages"].as_array().expect("packages is a list") {
                packages.push(package.as_str().expect("a package path").to_owned());
            }
        }
    }
    packages.sort();
    packages.dedup();
    assert!(!packages.is_empty(), "no GKE environment names a package");
    let refusals: Vec<String> = packages
        .iter()
        .filter_map(|package| {
            check_package_digest(&repository().join(package))
                .err()
                .map(|error| format!("{package}: {error:#}"))
        })
        .collect();
    assert!(refusals.is_empty(), "{}", refusals.join("\n"));
}
