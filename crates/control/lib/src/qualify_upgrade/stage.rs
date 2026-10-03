//! Admission boundary while the online stage executor is introduced.

use anyhow::ensure;
use wamn_schema_generator::PackageManifest;

/// Never treat a declared stage as an ordinary additive migration.
pub(crate) fn require_executor(manifest: &PackageManifest) -> anyhow::Result<()> {
    ensure!(
        manifest.upgrade_stage.is_none(),
        "package {}@{} declares an online upgrade stage; the stage executor is not available",
        manifest.package.id,
        manifest.package.version
    );
    Ok(())
}
