//! Step 9 of `apply`: contract (docs/plan/platform-deploy.md §10.1 step 9,
//! §10.4, R13, R16, R22 (3), (4), (7), epic decision D8).
//!
//! Step 8 returned, so no pod of another release remains and no run is
//! stranded. The verb removes the bindings of every release outside the live
//! set, and disables each instance the document no longer names that no
//! surviving binding uses. An instance is never deleted. Under
//! `release = none` no release is live, so every binding goes, and every
//! declared instance stays enabled.
//!
//! A declared floor that differs from the recorded one is contracted only
//! when the live set is exactly one release: a version the environment has
//! not installed is applied from its verified artifact, which must be a
//! contract-phase version whose predecessor is the installed version; then
//! the floor is recorded. Otherwise the verb says the floor is waiting and
//! exits non-zero.

use std::collections::BTreeMap;

use anyhow::{Context as _, bail, ensure};
use wamn_schema_control::connections::{
    disable_unbound_connection_instances_sql, remove_connection_bindings_outside_live_set_sql,
};
use wamn_schema_generator::{PackageManifest, UpgradeStagePhase};

use super::Platform;
use super::analyse::{Analysis, connect};
use super::document::EnvironmentDocument;

/// Record a floor, or advance it.
const RECORD_FLOOR_SQL: &str = "\
INSERT INTO catalog.package_floors (tenant_id, package_id, version) VALUES ($1, $2, $3) \
ON CONFLICT (tenant_id, package_id) \
DO UPDATE SET version = EXCLUDED.version, recorded_at = now()";

/// Step 9. Returns the plan lines of what it did.
///
/// # Errors
///
/// When a write fails, a floor's artifact is refused, or a floor is waiting.
/// The bindings and instances are contracted before a floor waits.
pub async fn contract(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
    live: &BTreeMap<String, usize>,
) -> anyhow::Result<Vec<String>> {
    let mut lines = Vec::new();
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);
    let project_url = super::ensure::project_url(platform, &document.triple()).await?;
    let live_digests: Vec<String> = live.keys().cloned().collect();
    let declared: Vec<String> = document.connections.keys().cloned().collect();

    let mut client = connect(&project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the connection contract")?;
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .context("claim the tenant")?;
    let removed = transaction
        .query(
            remove_connection_bindings_outside_live_set_sql(),
            &[&tenant, &document.env, &live_digests],
        )
        .await
        .context("remove the bindings of releases outside the live set")?;
    let disabled = transaction
        .query(
            disable_unbound_connection_instances_sql(),
            &[&tenant, &document.env, &declared],
        )
        .await
        .context("disable the instances the document no longer names")?;
    transaction
        .commit()
        .await
        .context("commit the connection contract")?;
    if !removed.is_empty() {
        lines.push(format!(
            "removed {} bindings of releases outside the live set",
            removed.len()
        ));
    }
    for row in disabled {
        lines.push(format!("disabled connection {}", row.get::<_, String>(0)));
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
                &tenant,
                package,
                version,
                &current.current,
            )
            .await?;
            lines.push(format!("applied the contract of {package}@{version}"));
        }
        record_floor(&project_url, &tenant, package, version).await?;
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
    tenant: &str,
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
        package: opened.root().to_owned(),
        database_url: project_url.to_owned(),
        tenant: tenant.to_owned(),
    })
    .await
    .with_context(|| format!("apply the contract of {package}@{version}"))
    .map(drop)
}

async fn record_floor(
    project_url: &str,
    tenant: &str,
    package: &str,
    version: &str,
) -> anyhow::Result<()> {
    let mut client = connect(project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the floor record")?;
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .context("claim the tenant")?;
    transaction
        .execute(RECORD_FLOOR_SQL, &[&tenant, &package, &version])
        .await
        .with_context(|| format!("record the floor {package}@{version}"))?;
    transaction
        .commit()
        .await
        .context("commit the floor record")
}
