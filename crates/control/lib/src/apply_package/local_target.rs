//! The local-target exception to manifest drift and to the release seal.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use tokio_postgres::{NoTls, Transaction};
use wamn_record_history::history_table_name;
use wamn_runtime::local_application::{LocalTargetComment, read_local_target_comment};
use wamn_schema_control::{
    AppliedPackage, PackageDirectory, PackageMigrationErrorKind, plan_package_migrations,
};
use wamn_schema_generator::PackageManifest;

use super::definition_ownership::SELECT_RELATION_PRESENT_SQL;
use super::roles::{reset_host_role, set_package_owner_role};
use super::{CLAIM_TENANT_SQL, load_applied_package, read_package_directory};

/// The release seal trigger compares this setting with the full database comment.
const LIFT_RELEASE_SEAL_SQL: &str =
    "SELECT pg_catalog.set_config('wamn.local_target_comment', $1, true)";
const COMMENT_ON_TARGET_SQL: &str = "SELECT pg_catalog.format(\
     'COMMENT ON DATABASE %I IS %L', pg_catalog.current_database(), $1::text)";

/// Check the marker inside the apply transaction and lift the seal for that transaction.
pub(super) async fn lift_release_seal(
    tx: &Transaction<'_>,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<LocalTargetComment> {
    let comment = read_local_target_comment(tx, tenant, environment).await?;
    tx.query_one(LIFT_RELEASE_SEAL_SQL, &[&comment.to_string()])
        .await
        .context("lift the release seal for this local target transaction")?;
    Ok(comment)
}

/// Record the current manifest hash of one package coordinate in the local target comment.
pub(super) async fn record_manifest(
    tx: &Transaction<'_>,
    mut comment: LocalTargetComment,
    coordinate: &str,
    manifest_sha256: &str,
) -> anyhow::Result<bool> {
    if comment.manifests.get(coordinate).map(String::as_str) == Some(manifest_sha256) {
        return Ok(false);
    }
    comment
        .manifests
        .insert(coordinate.to_owned(), manifest_sha256.to_owned());
    let statement: String = tx
        .query_one(COMMENT_ON_TARGET_SQL, &[&comment.to_string()])
        .await
        .context("render the local target comment")?
        .get(0);
    tx.batch_execute(&statement)
        .await
        .context("record the current manifest hash in the local target comment")?;
    Ok(true)
}

/// The refusal for an applied migration that a package directory edits, removes, or reorders.
///
/// The local apply takes a changed wamn.json, so only the applied migrations
/// are compared.
pub fn applied_migration_drift(
    directory: &PackageDirectory,
    applied: &AppliedPackage,
) -> anyhow::Result<Option<String>> {
    let presented = plan_package_migrations(directory, None)?;
    let accepted = AppliedPackage {
        manifest_sha256: presented.manifest_sha256,
        ..applied.clone()
    };
    match plan_package_migrations(directory, Some(&accepted)) {
        Ok(_) => Ok(None),
        Err(error) if error.kind() == PackageMigrationErrorKind::MigrationDrift => {
            Ok(Some(error.to_string()))
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
    tenant: &str,
    packages: &[PathBuf],
) -> anyhow::Result<Option<String>> {
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
        tx.query_one(CLAIM_TENANT_SQL, &[&tenant])
            .await
            .context("claim package tenant")?;
        let mut reason = None;
        for root in packages {
            reason = package_recreate_reason(&tx, tenant, root).await?;
            if reason.is_some() {
                break;
            }
        }
        tx.rollback()
            .await
            .context("finish the local target check")?;
        Ok(reason)
    }
    .await;
    drop(client);
    connection.abort();
    result
}

async fn package_recreate_reason(
    tx: &Transaction<'_>,
    tenant: &str,
    root: &Path,
) -> anyhow::Result<Option<String>> {
    let directory = read_package_directory(root)?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for the local target check")?;
    let package = &manifest.package;
    if let Some(applied) = load_applied_package(tx, tenant, &package.id, &package.version).await?
        && let Some(drift) = applied_migration_drift(&directory, &applied)?
    {
        return Ok(Some(drift));
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
        set_package_owner_role(tx).await?;
        let holds_rows = tx
            .query_one(rows_present_sql.as_str(), &[])
            .await
            .with_context(|| format!("read rows of history table {}.{history}", model.schema))?
            .get::<_, bool>(0);
        reset_host_role(tx).await?;
        if holds_rows {
            return Ok(Some(format!(
                "history table {}.{history} holds rows, and its model no longer keeps a log",
                model.schema
            )));
        }
    }
    Ok(None)
}
