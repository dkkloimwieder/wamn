use super::{AppliedPackage, PackageDirectory, PackageMigrationErrorType, plan_package_migrations};

/// The digest of the `wamn.json` bytes of a working tree, which the local
/// target comment records.
pub fn manifest_digest(directory: &PackageDirectory) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(
            &ring::digest::SHA256,
            &directory.manifest_bytes
        ))
    )
}

/// The refusal for an applied migration that a package directory edits, removes, or reorders.
///
/// The local apply takes a changed tree, so only the applied migrations are
/// compared.
pub fn applied_migration_drift(
    directory: &PackageDirectory,
    applied: &AppliedPackage,
) -> anyhow::Result<Option<String>> {
    Ok(migration_changes(directory, Some(applied))?.recreate_reason)
}

pub(super) fn migration_changes(
    directory: &PackageDirectory,
    applied: Option<&AppliedPackage>,
) -> anyhow::Result<LocalTargetChanges> {
    let presented = plan_package_migrations(directory, None)?;
    let accepted = applied.map(|applied| AppliedPackage {
        artifact_digest: presented.artifact_digest,
        ..applied.clone()
    });
    match plan_package_migrations(directory, accepted.as_ref()) {
        Ok(plan) => Ok(LocalTargetChanges {
            recreate_reason: None,
            pending_migrations: !plan.pending.is_empty(),
        }),
        Err(error) if error.error_type() == PackageMigrationErrorType::MigrationDrift => {
            Ok(LocalTargetChanges {
                recreate_reason: Some(error.to_string()),
                pending_migrations: false,
            })
        }
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Default)]
pub struct LocalTargetChanges {
    pub recreate_reason: Option<String>,
    pub(super) pending_migrations: bool,
}

impl LocalTargetChanges {
    pub fn requires_stop(&self) -> bool {
        self.recreate_reason.is_some() || self.pending_migrations
    }
}
