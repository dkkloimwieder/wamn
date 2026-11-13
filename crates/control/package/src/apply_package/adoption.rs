//! Record a user assertion through the installed schema and migration owners.

use sqlx::Row as _;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use wamn_schema_control::{MigrationOutcome, plan_package_migrations};

use super::{
    PackageRoot, load_applied_package, migration, read_package_migrations, read_package_root,
};

/// Adopt selected recipes atomically without executing their SQL.
/// Matching records remain unchanged, including their retained source artifacts.
/// The result names asserted files, never executed files.
pub async fn adopt_package_schemas(
    database_url: &str,
    project_env_key: &str,
    initialization_identity: &str,
    roots: &[&PackageRoot],
    local_environment: Option<&str>,
) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    let mut client = wamn_postgres::connect(database_url).await?;
    let mut tx = migration::begin(&mut client, project_env_key).await?;
    sqlx::query(super::LOCK_SQL).fetch_one(&mut *tx).await?;
    let actual: String =
        sqlx::query("SELECT installed_at::text FROM platform.schema_baseline WHERE singleton")
            .fetch_one(&mut *tx)
            .await?
            .get(0);
    ensure!(
        actual == initialization_identity,
        "database initialization changed before adoption: recorded={initialization_identity}, actual={actual}"
    );
    if let Some(environment) = local_environment {
        wamn_runtime::local_application::read_local_target_comment(
            &mut tx,
            project_env_key,
            environment,
        )
        .await?;
    }
    let mut result = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        ensure!(
            local_environment.is_some() || root.artifact_digest.is_some(),
            "deployed adoption requires published schema inputs"
        );
        let directory = read_package_root(root)?;
        let complete = plan_package_migrations(&directory, None, &[])?;
        let id = complete.coordinate.package_id();
        ensure!(seen.insert(id.to_owned()), "adoption repeats package {id}");
        sqlx::query(super::LOCK_PACKAGE_SQL)
            .bind(project_env_key)
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let existing = load_applied_package(&mut tx, project_env_key, id).await?;
        let records = read_package_migrations(&mut tx, project_env_key, id).await?;
        ensure!(
            !records
                .iter()
                .any(|file| file.outcome == MigrationOutcome::Started),
            "package {id} has uncertain SQL; resolve it before adoption"
        );
        // An assertion cannot erase history that its selected artifact cannot reconstruct.
        ensure!(
            records.len() <= complete.pending.len(),
            "package {id} omits recorded files; select a complete source, repair it, or reset an authorized disposable target"
        );
        let fingerprint = wamn_compiler::build_record::physical_schema_fingerprint(
            &wamn_compiler::output_root(&root.path),
        )?;
        let mut asserted = Vec::new();
        for file in &complete.pending {
            if records.iter().any(|record| {
                record.ordinal == file.ordinal
                    && record.relative_path == file.relative_path
                    && record.sha256 == file.sha256
            }) {
                continue;
            }
            sqlx::query("INSERT INTO catalog.package_migrations \
                 (project_env_key, package_id, package_version, ordinal, relative_path, sha256, outcome) \
                 VALUES ($1,$2,$3,$4,$5,$6,'adopted') \
                 ON CONFLICT (project_env_key, package_id, ordinal) DO UPDATE \
                 SET package_version=EXCLUDED.package_version, relative_path=EXCLUDED.relative_path, \
                     sha256=EXCLUDED.sha256, outcome='adopted', applied_at=NULL").bind(project_env_key).bind(id).bind(complete.coordinate.package_version()).bind(i32::try_from(file.ordinal)?).bind(&file.relative_path).bind(&file.sha256).execute(&mut *tx).await.map(|result| result.rows_affected()).with_context(|| format!("adopt {id}: {}", file.relative_path))?;
            asserted.push(file.relative_path.clone());
        }
        if existing
            .as_ref()
            .is_none_or(|record| record.physical_schema_fingerprint != fingerprint)
        {
            super::package_version::record_package_schema(
                &mut tx,
                project_env_key,
                &complete.coordinate,
                directory.artifact_digest.as_deref(),
                &fingerprint,
            )
            .await?;
        }
        result.insert(id.to_owned(), asserted);
    }
    tx.commit().await.context("commit asserted schema state")?;
    Ok(result)
}
