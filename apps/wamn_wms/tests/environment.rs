//! Shared target for native WMS business tests.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Context as _;
use wamn_catalog::PackageCoordinate;
use wamn_environment::provision_project_env::read_json;
use wamn_release_control::delivery::ReleaseIdentity;

pub(super) const SCHEMA: &str = "wms";

pub(crate) fn identity() -> &'static ReleaseIdentity {
    static IDENTITY: LazyLock<ReleaseIdentity> = LazyLock::new(|| ReleaseIdentity {
        org: "acme".to_owned(),
        project: "wms".to_owned(),
        environment: "dev".to_owned(),
        route_host: "wms.localhost".to_owned(),
        packages: BTreeSet::from([package_coordinate().expect("read the WMS package coordinate")]),
    });
    &IDENTITY
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("WMS test crate has an app parent")
        .to_path_buf()
}

pub(crate) fn package_coordinate() -> anyhow::Result<PackageCoordinate> {
    let manifest = read_json(&wamn_compiler::package_manifest_path(&package_root()))?;
    PackageCoordinate::new(
        manifest["package"]["id"]
            .as_str()
            .context("WMS manifest names its package")?,
        manifest["package"]["version"]
            .as_str()
            .context("WMS manifest names its version")?,
    )
    .map_err(Into::into)
}
