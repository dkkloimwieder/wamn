//! Step 9 of `apply`: contract
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! Step 8 returned, so no pod of another release remains and no run is
//! stranded. The verb removes the bindings of every release outside the live
//! set. Connection lifecycle remains administration.
//!
//! A declared floor that differs from the recorded one is contracted only
//! when the live set is exactly one release: a version the environment has
//! not installed is applied from its verified artifact, which must be a
//! contract-phase version whose predecessor is the installed version; then
//! the floor is recorded. Otherwise the verb says the floor is waiting and
//! exits non-zero.

use std::collections::BTreeMap;

use anyhow::{Context as _, bail, ensure};
use wamn_schema_generator::{PackageManifest, UpgradeStagePhase};

use super::Platform;
use super::analyse::{Analysis, connect};
use super::document::EnvironmentDocument;

/// Record a floor, or advance it.
const RECORD_FLOOR_SQL: &str = "\
INSERT INTO catalog.package_floors (env_key, package_id, version) VALUES ($1, $2, $3) \
ON CONFLICT (env_key, package_id) \
DO UPDATE SET version = EXCLUDED.version, recorded_at = now()";

/// Step 9. Returns the plan lines of what it did.
///
/// # Errors
///
/// When a write fails, a floor's artifact is refused, or a floor is waiting.
/// The release versions and bindings are contracted before a floor waits.
pub async fn contract(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
    live: &BTreeMap<String, usize>,
) -> anyhow::Result<Vec<String>> {
    let mut lines = Vec::new();
    let env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    let project_url = super::ensure::project_url(platform, &document.triple()).await?;
    let live_digests: Vec<String> = live.keys().cloned().collect();

    let mut client = connect(&project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the connection contract")?;
    transaction
        .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
        .await
        .context("claim the tenant")?;
    transaction
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await?;
    let removed = transaction.execute(
        "DELETE FROM catalog.connection_binding WHERE env_key=$1 AND NOT (manifest_digest=ANY($2::text[]))",
        &[&env_key,&live_digests],
    ).await?;
    transaction.execute(
        "DELETE FROM catalog.release_connection WHERE env_key=$1 AND NOT (manifest_digest=ANY($2::text[]))",
        &[&env_key,&live_digests],
    ).await?;
    transaction.commit().await?;
    if removed > 0 {
        lines.push(format!(
            "removed {removed} bindings of releases outside the live set"
        ));
    }

    let recorded = analysis
        .authorities
        .project
        .as_ref()
        .map(|project| project.floors.clone())
        .unwrap_or_default();
    let mut waiting = Vec::new();
    for (package, version) in &document.floors {
        if recorded.get(package) == Some(version) {
            continue;
        }
        if live.len() != 1 {
            waiting.push(format!("{package}@{version}"));
            continue;
        }
        let installed = super::stage::installed(platform, document).await?;
        let current = installed
            .get(package)
            .with_context(|| format!("floor {package}@{version} names a package not installed"))?;
        if !current.lineage.contains_key(version) {
            apply_contract(
                platform,
                &project_url,
                document,
                package,
                version,
                &current.current,
            )
            .await?;
            lines.push(format!("applied the contract of {package}@{version}"));
        }
        record_floor(&project_url, &env_key, package, version).await?;
        lines.push(format!("recorded the floor {package}@{version}"));
    }
    if !waiting.is_empty() {
        bail!(
            "the floors {} are waiting: the live set is {:?}, not exactly one release",
            waiting.join(", "),
            live
        );
    }
    Ok(lines)
}

/// Apply the contract-phase version `package@version` from its verified
/// artifact. Its predecessor must be the installed version.
async fn apply_contract(
    platform: &Platform,
    project_url: &str,
    document: &EnvironmentDocument,
    package: &str,
    version: &str,
    current: &str,
) -> anyhow::Result<()> {
    let opened = super::stage::open(platform, package, version).await?;
    let path = wamn_schema_generator::package_manifest_path(opened.root());
    let manifest = PackageManifest::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    ensure!(
        manifest
            .upgrade_stage
            .as_ref()
            .is_some_and(|stage| stage.phase == UpgradeStagePhase::Contract),
        "floor {package}@{version} is not a contract-phase version"
    );
    ensure!(
        manifest.package.predecessor_version.as_deref() == Some(current),
        "floor {package}@{version}: its predecessor {:?} is not the installed version {current}",
        manifest.package.predecessor_version
    );
    crate::apply_package::apply_package(crate::apply_package::ApplyPackageRequest {
        component_source: platform.component_source()?,
        changes: platform.auth_changes().await?,
        package: opened.package_root(),
        database_url: project_url.to_owned(),
        org: document.org.clone(),
        project: document.project.clone(),
        environment: document.env.clone(),
    })
    .await
    .with_context(|| format!("apply the contract of {package}@{version}"))
    .map(drop)
}

async fn record_floor(
    project_url: &str,
    env_key: &str,
    package: &str,
    version: &str,
) -> anyhow::Result<()> {
    let mut client = connect(project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the floor record")?;
    transaction
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .context("bind wamn:provisioning as the actor and operation")?;
    transaction
        .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
        .await
        .context("claim the tenant")?;
    transaction
        .execute(RECORD_FLOOR_SQL, &[&env_key, &package, &version])
        .await
        .with_context(|| format!("record the floor {package}@{version}"))?;
    transaction
        .commit()
        .await
        .context("commit the floor record")
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn floor_writer_stamps_provisioning_on_insert_and_advance() {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_project_state::test_database::tenant_platform();
        database.execute(&[&format!(
            "BEGIN; {} INSERT INTO catalog.packages (env_key,package_id,package_version,predecessor_version) VALUES ('history','fixture','1',NULL),('history','fixture','2','1'); COMMIT;",
            wamn_control_provision::platform_principals_sql("example.invalid").unwrap()
        )]).unwrap();
        let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
        for version in ["1", "2"] {
            super::record_floor(database.url(), "history", "fixture", version)
                .await
                .unwrap();
            let sql = format!(
                "SELECT created_by = '{actor}' AND updated_by = '{actor}' AND version = '{version}' \
                 AND created_at <= updated_at FROM catalog.package_floors"
            );
            assert_eq!(database.execute(&[&sql]).unwrap().trim(), "t");
        }
        assert_eq!(
            database
                .execute(&["SELECT created_at < updated_at FROM catalog.package_floors"])
                .unwrap()
                .trim(),
            "t"
        );
    }
}
