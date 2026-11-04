//! The local-target exception to the content conflict and to the release seal.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use tokio_postgres::{NoTls, Transaction};
use wamn_catalog::manifest::PackageManifest;
use wamn_record_history::history_table_name;
use wamn_runtime::local_application::{LocalTargetComment, read_local_target_comment};
use wamn_schema_control::{
    AppliedPackage, PackageDirectory, PackageMigrationErrorType, plan_package_migrations,
};

use super::definition_ownership::SELECT_RELATION_PRESENT_SQL;
use super::package_version::read_source_package_directory;
use super::roles::{reset_host_role, set_package_owner_role};
use super::{CLAIM_ENV_KEY_SQL, load_applied_package};

/// The release seal trigger compares this setting with the full database comment.
const LIFT_RELEASE_SEAL_SQL: &str =
    "SELECT pg_catalog.set_config('wamn.local_target_comment', $1, true)";
const COMMENT_ON_TARGET_SQL: &str = "SELECT pg_catalog.format(\
     'COMMENT ON DATABASE %I IS %L', pg_catalog.current_database(), $1::text)";

/// Check the marker inside the apply transaction and lift the seal for that transaction.
pub(super) async fn lift_release_seal(
    tx: &Transaction<'_>,
    env_key: &str,
    environment: &str,
) -> anyhow::Result<LocalTargetComment> {
    let comment = read_local_target_comment(tx, env_key, environment).await?;
    tx.query_one(LIFT_RELEASE_SEAL_SQL, &[&comment.to_string()])
        .await
        .context("lift the release seal for this local target transaction")?;
    Ok(comment)
}

/// Record the current manifest digest of one package coordinate in the local target comment.
///
/// The caller owns the comment, so one transaction can record several
/// coordinates and keep every digest it already wrote. The local target applies
/// its working tree and records no artifact digest, so the comment names the
/// `wamn.json` the running application came from ([`manifest_digest`]).
pub async fn record_manifest(
    tx: &Transaction<'_>,
    comment: &mut LocalTargetComment,
    coordinate: &str,
    manifest_digest: &str,
) -> anyhow::Result<bool> {
    if comment.manifests.get(coordinate).map(String::as_str) == Some(manifest_digest) {
        return Ok(false);
    }
    comment
        .manifests
        .insert(coordinate.to_owned(), manifest_digest.to_owned());
    let statement: String = tx
        .query_one(COMMENT_ON_TARGET_SQL, &[&comment.to_string()])
        .await
        .context("render the local target comment")?
        .get(0);
    tx.batch_execute(&statement)
        .await
        .context("record the current manifest digest in the local target comment")?;
    Ok(true)
}

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

#[derive(Debug, Default)]
pub struct LocalTargetChanges {
    pub recreate_reason: Option<String>,
    pending_migrations: bool,
}

impl LocalTargetChanges {
    pub fn requires_stop(&self) -> bool {
        self.recreate_reason.is_some() || self.pending_migrations
    }
}

fn migration_changes(
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

/// Why a local target needs recreation before it takes these packages, or `None` when it keeps them.
///
/// apply-package refuses an applied migration that changed, and it never drops
/// a history table. So a changed applied migration needs a new target, and so
/// does a history table that holds rows when its model no longer keeps a log.
pub async fn local_target_recreate_reason(
    database_url: &str,
    env_key: &str,
    packages: &[PathBuf],
) -> anyhow::Result<Option<String>> {
    Ok(local_target_changes(database_url, env_key, packages)
        .await?
        .recreate_reason)
}

/// Read recreation and pending migration facts before a local candidate changes the database.
pub async fn local_target_changes(
    database_url: &str,
    env_key: &str,
    packages: &[PathBuf],
) -> anyhow::Result<LocalTargetChanges> {
    let (mut client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the local target")?;
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = async {
        // The check writes nothing. The transaction only holds the package
        // row locks that load_applied_package takes, and it rolls back.
        let tx = client
            .transaction()
            .await
            .context("begin the local target check")?;
        tx.query_one(CLAIM_ENV_KEY_SQL, &[&env_key])
            .await
            .context("claim package tenant")?;
        let mut changes = LocalTargetChanges::default();
        for root in packages {
            let package_changes = package_target_changes(&tx, env_key, root).await?;
            changes.pending_migrations |= package_changes.pending_migrations;
            changes.recreate_reason = package_changes.recreate_reason;
            if changes.recreate_reason.is_some() {
                break;
            }
        }
        tx.rollback()
            .await
            .context("finish the local target check")?;
        Ok(changes)
    }
    .await;
    drop(client);
    connection.abort();
    result
}

async fn package_target_changes(
    tx: &Transaction<'_>,
    env_key: &str,
    root: &Path,
) -> anyhow::Result<LocalTargetChanges> {
    let directory = read_source_package_directory(root)?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for the local target check")?;
    let package = &manifest.package;
    let applied = load_applied_package(tx, env_key, &package.id, &package.version).await?;
    let mut changes = migration_changes(&directory, applied.as_ref())?;
    if changes.recreate_reason.is_some() {
        return Ok(changes);
    }
    for model in manifest
        .models
        .values()
        .filter(|model| model.owner == package.id && model.log_retention().is_none())
    {
        let history = history_table_name(&model.table);
        let present = tx
            .query_one(SELECT_RELATION_PRESENT_SQL, &[&model.schema, &history])
            .await
            .with_context(|| format!("read history table {}.{history}", model.schema))?
            .get::<_, bool>(0);
        if !present {
            continue;
        }
        // The package-owner role owns the history table.
        let rows_present_sql = format!(
            "SELECT EXISTS (SELECT 1 FROM {}.{})",
            wamn_pg_core::quote_ident(&model.schema),
            wamn_pg_core::quote_ident(&history)
        );
        set_package_owner_role(tx, &package.id).await?;
        let holds_rows = tx
            .query_one(rows_present_sql.as_str(), &[])
            .await
            .with_context(|| format!("read rows of history table {}.{history}", model.schema))?
            .get::<_, bool>(0);
        reset_host_role(tx).await?;
        if holds_rows {
            changes.recreate_reason = Some(format!(
                "history table {}.{history} holds rows, and its model no longer keeps a log",
                model.schema
            ));
            break;
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_schema_control::{MigrationSource, RecordedMigration};

    fn installed_package() -> (PackageDirectory, AppliedPackage) {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../environment/tests/support/dev_package");
        let directory = super::super::read_package_directory(&root).unwrap();
        let plan = plan_package_migrations(&directory, None).unwrap();
        let applied = AppliedPackage {
            coordinate: plan.coordinate,
            predecessor_version: plan.predecessor_version,
            artifact_digest: plan.artifact_digest,
            migrations: plan
                .pending
                .into_iter()
                .map(|migration| RecordedMigration {
                    ordinal: migration.ordinal,
                    relative_path: migration.relative_path,
                    sha256: migration.sha256,
                })
                .collect(),
        };
        (directory, applied)
    }

    #[test]
    fn source_and_manifest_changes_without_pending_migrations_keep_the_host() {
        let (mut directory, applied) = installed_package();
        assert!(
            !migration_changes(&directory, Some(&applied))
                .unwrap()
                .requires_stop()
        );

        let mut manifest: serde_json::Value =
            serde_json::from_slice(&directory.manifest_bytes).unwrap();
        manifest["models"]["location"]["server_owned_fields"] =
            serde_json::json!(["location_code"]);
        directory.manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        assert!(
            !migration_changes(&directory, Some(&applied))
                .unwrap()
                .requires_stop()
        );
    }

    #[test]
    fn a_pending_migration_stops_the_host_and_keeps_the_target() {
        let (mut directory, applied) = installed_package();
        directory.migrations.push(MigrationSource {
            relative_path: "migrations/0002_location_note.sql".to_owned(),
            bytes: b"ALTER TABLE platform_fixture.location ADD COLUMN note text;".to_vec(),
        });
        let changes = migration_changes(&directory, Some(&applied)).unwrap();
        assert!(changes.requires_stop());
        assert!(changes.recreate_reason.is_none());
    }

    #[test]
    fn an_edited_applied_migration_stops_the_host_and_recreates_the_target() {
        let (mut directory, applied) = installed_package();
        directory.migrations[0]
            .bytes
            .extend_from_slice(b"\n-- changed\n");
        let changes = migration_changes(&directory, Some(&applied)).unwrap();
        assert!(changes.requires_stop());
        assert!(changes.recreate_reason.is_some());
    }

    #[test]
    fn target_recreation_stops_the_host_without_pending_migrations() {
        let changes = LocalTargetChanges {
            recreate_reason: Some("a removed history table holds rows".to_owned()),
            pending_migrations: false,
        };
        assert!(changes.requires_stop());
    }
}
