// Receiving environment tests and helpers.

use super::*;

pub(super) fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving")
}

pub(super) fn overlay_package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/client_acme_receiving")
}

pub(super) fn repository_root() -> anyhow::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .context("resolve the repository root for the product command")
}
