use super::{PackageManifest, ensure};

/// Select SQL verification from declarations rather than package identity.
pub fn requires_verifier(manifest: &PackageManifest) -> bool {
    manifest
        .models
        .values()
        .any(|model| !model.operations.is_empty())
        || manifest
            .custom_operations
            .values()
            .any(|operation| !operation.statements.is_empty())
}

/// Require the pinned CLI version before preparing metadata.
pub fn require_cli_version(output: &[u8]) -> anyhow::Result<()> {
    ensure!(
        matches!(
            String::from_utf8_lossy(output).trim(),
            "sqlx-cli 0.9.0" | "sqlx-cli-sqlx 0.9.0"
        ),
        "SQLx CLI 0.9.0 is required"
    );
    Ok(())
}
