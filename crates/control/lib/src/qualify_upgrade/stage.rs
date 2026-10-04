//! Admission boundary while the online stage executor is introduced.

use anyhow::ensure;
use wamn_schema_control::PackageDirectory;
use wamn_schema_generator::{PackageManifest, UpgradeStagePhase};
use wamn_schema_introspection::migration_policy::refuse_dynamic_stage_sql;

/// Never treat a declared stage as an ordinary additive migration.
pub(crate) fn require_executor(
    manifest: &PackageManifest,
    directory: &PackageDirectory,
) -> anyhow::Result<()> {
    if let Some(stage) = &manifest.upgrade_stage {
        for (section, conditions) in [
            ("preconditions", &stage.preconditions),
            ("postconditions", &stage.postconditions),
        ] {
            for (name, sql) in conditions {
                refuse_dynamic_stage_sql(
                    format!("upgrade_stage.{section}.{name}.sql"),
                    sql.as_bytes(),
                )?;
            }
        }
        if let Some(batch) = &stage.backfill {
            refuse_dynamic_stage_sql("upgrade_stage.backfill.sql", batch.sql.as_bytes())?;
        }
        for migration in &directory.migrations {
            refuse_dynamic_stage_sql(&migration.relative_path, &migration.bytes)?;
        }
    }
    ensure!(
        manifest.upgrade_stage.as_ref().is_none_or(|stage| matches!(
            stage.phase,
            UpgradeStagePhase::Expand | UpgradeStagePhase::Backfill
        )),
        "package {}@{} declares an online upgrade stage; the stage executor is not available",
        manifest.package.id,
        manifest.package.version
    );
    Ok(())
}

/// Admit the syntax of an expand trigger pair before transaction-time ownership checks.
pub(crate) fn validate_successor_migration(
    path: impl AsRef<std::path::Path>,
    bytes: &[u8],
    schemas: &[&str],
    stage: Option<&wamn_schema_generator::UpgradeStage>,
) -> anyhow::Result<()> {
    use wamn_schema_introspection::migration_policy::{
        inspect_stage_synchronization_declaration,
        validate_predecessor_compatible_migration_bytes_for_schemas,
    };
    let ordinary =
        validate_predecessor_compatible_migration_bytes_for_schemas(path.as_ref(), bytes, schemas);
    if ordinary.is_ok() {
        return Ok(());
    }
    if stage.is_some_and(|stage| stage.phase == UpgradeStagePhase::Expand)
        && let Ok(sync) = inspect_stage_synchronization_declaration(path.as_ref(), bytes)
    {
        ensure!(
            schemas.contains(&sync.schema.as_str()),
            "synchronization {}.{} is outside the package runtime schema",
            sync.schema,
            sync.trigger
        );
        return Ok(());
    }
    ordinary.map_err(Into::into)
}

/// Select the explicit exception without changing ordinary upgrade admission.
pub(crate) fn whole_row_grants(stage: Option<&wamn_schema_generator::UpgradeStage>) -> bool {
    stage.is_some_and(|stage| {
        stage
            .exceptions
            .contains(&wamn_schema_generator::UpgradeStageException::WholeRowGrants)
    })
}
