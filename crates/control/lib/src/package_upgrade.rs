//! Enforce accepted upgrade evidence at the production package and release boundaries.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_catalog::ServingManifest;
use wamn_schema_control::{PackageDirectory, PackageMigrationPlan};
use wamn_schema_introspection::migration_policy::validate_predecessor_compatible_migration_bytes_for_schemas;

use crate::qualify_upgrade::{
    self, MigrationIdentity, PackageIdentity, PresentedRootIdentity, UpgradeQualification,
    identity_from_directory, read_current_packages, read_selected_manifest,
};
use crate::reconcile_package_data_access::upgrade::read_upgrade_privileges;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub(crate) struct AcceptedUpgrade {
    evidence: UpgradeQualification,
    bytes: Vec<u8>,
    sha256: String,
    #[cfg(test)]
    observed_workloads: Option<qualify_upgrade::workload::ServingWorkloads>,
}

pub(crate) fn read_evidence(path: &Path) -> anyhow::Result<AcceptedUpgrade> {
    let (evidence, bytes, sha256) = qualify_upgrade::read_qualification(path)?;
    Ok(AcceptedUpgrade {
        evidence,
        bytes,
        sha256,
        #[cfg(test)]
        observed_workloads: None,
    })
}

/// Apply the proved base and all overlay re-pins in one production transaction.
pub(crate) async fn apply_coordinated(
    request: crate::apply_package::ApplyPackageRequest,
    roots: &[PathBuf],
    accepted: AcceptedUpgrade,
) -> anyhow::Result<Vec<crate::apply_package::ApplyOutcome>> {
    use crate::apply_package::{
        apply_in_transaction, bind_apply_package_principal, prepare_package,
    };
    use tokio_postgres::NoTls;

    let evidence = &accepted.evidence;
    let proof = evidence
        .overlay
        .as_ref()
        .context("coordinated application requires base and overlay qualification")?;
    require_prefix(evidence)?;
    ensure!(
        request.tenant == evidence.tenant,
        "qualified tenant changed"
    );
    require_candidate_root(&request.package, evidence)?;
    let mut presented = roots
        .iter()
        .map(|root| qualify_upgrade::presented_root_identity(root))
        .collect::<anyhow::Result<Vec<_>>>()?;
    presented.sort_by(|left, right| left.package.package_id.cmp(&right.package.package_id));
    ensure!(
        presented == evidence.presented_roots,
        "application roots differ from the exact qualified complete successor set"
    );
    let mut ordered = vec![prepare_package(&request.package)?];
    for overlay in &proof.overlays {
        let root = roots
            .iter()
            .find(|root| {
                qualify_upgrade::presented_root_identity(root)
                    .is_ok_and(|identity| identity.package == overlay.candidate)
            })
            .context("application roots omit a qualified overlay successor")?;
        ordered.push(prepare_package(root)?);
    }
    let (mut client, connection) = tokio_postgres::connect(&request.database_url, NoTls)
        .await
        .context("connect for coordinated package application")?;
    let driver = tokio::spawn(connection);
    let result = async {
        let tx = client
            .transaction()
            .await
            .context("begin atomic base and overlay application")?;
        tx.query_one(
            "SELECT set_config('app.tenant', $1, true)",
            &[&request.tenant],
        )
        .await?;
        bind_apply_package_principal(&tx).await?;
        tx.query_one(crate::reconcile_package_data_access::LOCK_SQL, &[])
            .await?;
        for package in &evidence.presented_packages {
            tx.query_one(
                crate::apply_package::LOCK_PACKAGE_SQL,
                &[&request.tenant, &package.package_id],
            )
            .await?;
        }
        let base = &ordered[0];
        let installed = read_current_packages(&tx, &request.tenant).await?;
        let leaf = installed
            .iter()
            .find(|package| package.package_id == evidence.candidate_package.package_id)
            .context("qualified base has no installed leaf")?;
        let applied = crate::apply_package::load_applied_package(
            &tx,
            &request.tenant,
            &leaf.package_id,
            &leaf.package_version,
        )
        .await?
        .context("qualified base has no installed coordinate")?;
        let plan = wamn_schema_control::plan_package_migrations(&base.directory, Some(&applied))?;
        let persist = require_application(
            &tx,
            &request.tenant,
            &base.root,
            &base.directory,
            &plan,
            Some(&accepted),
            true,
        )
        .await?;
        if persist.is_none() {
            ensure!(
                installed == evidence.presented_packages,
                "accepted retry differs from the complete qualified installed state"
            );
        }
        let mut outcomes = Vec::with_capacity(ordered.len());
        for package in &ordered {
            outcomes.push(apply_in_transaction(&tx, &request.tenant, package).await?);
        }
        ensure!(
            read_current_packages(&tx, &request.tenant).await? == evidence.presented_packages,
            "atomic application does not match the complete qualified successor state"
        );
        if let Some(accepted) = &persist {
            self::persist(&tx, accepted).await?;
        }
        tx.commit()
            .await
            .context("commit base, overlay successors, and qualification together")?;
        Ok(outcomes)
    }
    .await;
    drop(client);
    if result.is_err() {
        driver.abort();
    } else {
        driver
            .await
            .context("join coordinated package connection")?
            .context("drive coordinated package connection")?;
    }
    result
}

#[cfg(test)]
pub(crate) fn read_evidence_with_observation(
    path: &Path,
    observed: qualify_upgrade::workload::ServingWorkloads,
) -> anyhow::Result<AcceptedUpgrade> {
    let mut accepted = read_evidence(path)?;
    accepted.observed_workloads = Some(observed);
    Ok(accepted)
}

async fn observe_workloads(
    accepted: &AcceptedUpgrade,
) -> anyhow::Result<qualify_upgrade::workload::ServingWorkloads> {
    #[cfg(test)]
    if let Some(observed) = &accepted.observed_workloads {
        return Ok(observed.clone());
    }
    let evidence = &accepted.evidence;
    qualify_upgrade::workload::observe(
        &evidence.workload_target,
        &evidence.tenant,
        &evidence.environment,
        &evidence.predecessor_manifest_digest,
    )
    .await
    .context("recheck the qualified serving workloads before package mutation")
}

async fn read_accepted(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    version: &str,
) -> anyhow::Result<Option<AcceptedUpgrade>> {
    let Some(row) = tx.query_opt(
        "SELECT canonical_bytes, result_sha256, predecessor_release_id, predecessor_manifest_digest \
         FROM catalog.package_upgrade_qualifications \
         WHERE tenant_id = $1 AND package_id = $2 AND candidate_package_version = $3",
        &[&tenant, &package, &version],
    ).await.context("read accepted upgrade evidence; install its platform carrier with upgrade-schema first")? else {
        return Ok(None);
    };
    let bytes: Vec<u8> = row.get(0);
    let evidence = qualify_upgrade::decode_qualification(&bytes)?;
    let value = serde_json::to_value(&evidence)?;
    let sha256 = wamn_execution_contract::canonical_json_sha256(&value);
    ensure!(
        ((evidence.format_version == 1 && evidence.overlay.is_none())
            || (evidence.format_version == 2 && evidence.overlay.is_some()))
            && wamn_execution_contract::canonical_json_bytes(&value) == bytes
            && sha256 == row.get::<_, String>(1)
            && evidence.tenant == tenant
            && evidence.candidate_package.package_id == package
            && evidence.candidate_package.package_version == version
            && evidence.predecessor_release_id == row.get::<_, i32>(2)
            && evidence.predecessor_manifest_digest == row.get::<_, String>(3),
        "persisted upgrade qualification differs from its immutable carrier"
    );
    Ok(Some(AcceptedUpgrade {
        evidence,
        bytes,
        sha256,
        #[cfg(test)]
        observed_workloads: None,
    }))
}

/// Validate before the first persistent write; return only evidence to insert atomically.
pub(crate) async fn require_application(
    tx: &Transaction<'_>,
    tenant: &str,
    root: &Path,
    directory: &PackageDirectory,
    plan: &PackageMigrationPlan,
    supplied: Option<&AcceptedUpgrade>,
    coordinated: bool,
) -> anyhow::Result<Option<AcceptedUpgrade>> {
    ensure!(
        supplied.is_none_or(|accepted| coordinated == accepted.evidence.overlay.is_some()),
        "coordinated base and overlay evidence requires apply-package with the complete presented package roots"
    );
    if plan.predecessor_version.is_none() {
        ensure!(
            supplied.is_none(),
            "upgrade qualification requires an installed predecessor"
        );
        return Ok(None);
    }
    let candidate = identity_from_directory(directory)?;
    let current = tx
        .query_opt(
            crate::apply_package::SELECT_CURRENT_PACKAGE_VERSION_SQL,
            &[&tenant, &candidate.package_id],
        )
        .await?;
    if current.is_none() {
        ensure!(
            supplied.is_none(),
            "upgrade qualification requires an installed predecessor"
        );
        return Ok(None);
    }
    if let Some(accepted) = read_accepted(
        tx,
        tenant,
        &candidate.package_id,
        &candidate.package_version,
    )
    .await?
    {
        require_candidate_root(root, &accepted.evidence)?;
        ensure!(
            accepted.evidence.candidate_package == candidate && plan.pending.is_empty(),
            "accepted upgrade candidate bytes changed"
        );
        if let Some(supplied) = supplied {
            ensure!(
                accepted.bytes == supplied.bytes,
                "conflicting qualification for accepted candidate"
            );
        }
        // Candidate reconciliation may already have replaced predecessor grants
        // and the selected head. An exact accepted retry does not rewind them.
        return Ok(None);
    }
    if plan.pending.is_empty() {
        let manifest =
            wamn_schema_generator::PackageManifest::from_slice(&directory.manifest_bytes)?;
        ensure!(
            candidate.package_version
                == current
                    .as_ref()
                    .expect("installed package exists")
                    .get::<_, String>(0)
                || manifest.base_dependencies.is_empty(),
            "installed overlay successors require coordinated base upgrade qualification"
        );
        ensure!(
            supplied.is_none(),
            "upgrade qualification names no pending successor suffix"
        );
        return Ok(None);
    }
    let accepted =
        supplied.context("installed successor migrations require --upgrade-qualification")?;
    let evidence = &accepted.evidence;
    require_candidate_root(root, evidence)?;
    ensure!(
        evidence.tenant == tenant && evidence.candidate_package == candidate,
        "upgrade qualification does not identify these candidate bytes"
    );
    let suffix = plan
        .pending
        .iter()
        .map(|migration| MigrationIdentity {
            ordinal: migration.ordinal,
            relative_path: migration.relative_path.clone(),
            sha256: migration.sha256.clone(),
        })
        .collect::<Vec<_>>();
    ensure!(
        suffix == evidence.candidate_suffix,
        "qualified successor suffix changed"
    );
    require_prefix(evidence)?;
    let schemas = wamn_schema_generator::data_access_schemas(&directory.manifest_bytes)?;
    let schemas = schemas.iter().map(String::as_str).collect::<Vec<_>>();
    for migration in &plan.pending {
        let source = directory
            .migrations
            .iter()
            .find(|source| source.relative_path == migration.relative_path)
            .context("planned successor migration has no source bytes")?;
        validate_predecessor_compatible_migration_bytes_for_schemas(
            Path::new(&source.relative_path),
            &source.bytes,
            &schemas,
        )?;
    }
    ensure!(
        read_current_packages(tx, tenant).await? == evidence.predecessor_packages,
        "live predecessor package leaves or migrations changed after qualification"
    );
    let head = tx
        .query_opt(
            "SELECT effective_release_id FROM catalog.effective_release_heads \
         WHERE tenant_id = $1 AND environment = $2 FOR UPDATE",
            &[&tenant, &evidence.environment],
        )
        .await?
        .context("qualified predecessor release is no longer selected")?;
    ensure!(
        head.get::<_, i32>(0) == evidence.predecessor_release_id,
        "qualified predecessor head changed"
    );
    let (_, digest) = read_selected_manifest(tx, tenant, &evidence.environment).await?;
    ensure!(
        digest == evidence.predecessor_manifest_digest,
        "qualified predecessor manifest changed"
    );
    ensure!(
        read_upgrade_privileges(tx, &evidence.schemas).await? == evidence.predecessor_privileges,
        "predecessor privileges changed after qualification"
    );
    let serving = observe_workloads(accepted).await?;
    ensure!(
        serving == evidence.serving_workloads,
        "serving workloads changed after qualification"
    );
    Ok(Some(accepted.clone()))
}

fn require_candidate_root(root: &Path, evidence: &UpgradeQualification) -> anyhow::Result<()> {
    let observed = qualify_upgrade::presented_root_identity(root)?;
    let qualified = evidence
        .presented_roots
        .iter()
        .find(|root| root.package.package_id == evidence.candidate_package.package_id)
        .context("qualified presented roots omit the candidate")?;
    ensure!(
        observed == *qualified,
        "qualified candidate statements or generated artifacts changed"
    );
    Ok(())
}

fn require_prefix(evidence: &UpgradeQualification) -> anyhow::Result<()> {
    let predecessor = &evidence.predecessor_package;
    let candidate = &evidence.candidate_package;
    ensure!(
        candidate.package_id == predecessor.package_id
            && candidate.predecessor_version.as_deref()
                == Some(predecessor.package_version.as_str())
            && candidate.migrations.starts_with(&predecessor.migrations)
            && candidate.migrations.len() > predecessor.migrations.len()
            && candidate.migrations[predecessor.migrations.len()..] == evidence.candidate_suffix,
        "upgrade evidence does not name the exact immediate predecessor migration prefix"
    );
    let mut expected = evidence.predecessor_packages.clone();
    let installed = expected
        .iter_mut()
        .find(|package| package.package_id == predecessor.package_id)
        .context("qualified predecessor is absent from the complete source root set")?;
    ensure!(
        *installed == *predecessor,
        "qualified predecessor differs from its source root set"
    );
    *installed = candidate.clone();
    if let Some(proof) = &evidence.overlay {
        qualify_upgrade::overlay::validate_evidence(proof, predecessor, candidate)?;
        for overlay in &proof.overlays {
            let installed = expected
                .iter_mut()
                .find(|package| package.package_id == overlay.predecessor.package_id)
                .context("qualified overlay is absent from the complete source root set")?;
            ensure!(
                *installed == overlay.predecessor,
                "qualified overlay differs from its installed predecessor"
            );
            *installed = overlay.candidate.clone();
        }
    }
    ensure!(
        expected == evidence.presented_packages,
        "qualified transition changes unrelated package roots"
    );
    Ok(())
}

pub(crate) async fn persist(
    tx: &Transaction<'_>,
    accepted: &AcceptedUpgrade,
) -> anyhow::Result<()> {
    let evidence = &accepted.evidence;
    tx.execute(
        "INSERT INTO catalog.package_upgrade_qualifications \
          (tenant_id, package_id, candidate_package_version, canonical_bytes, result_sha256, \
           predecessor_release_id, predecessor_manifest_digest) VALUES ($1,$2,$3,$4,$5,$6,$7)",
        &[
            &evidence.tenant,
            &evidence.candidate_package.package_id,
            &evidence.candidate_package.package_version,
            &accepted.bytes,
            &accepted.sha256,
            &evidence.predecessor_release_id,
            &evidence.predecessor_manifest_digest,
        ],
    )
    .await
    .context("persist accepted qualification with the installed successor")?;
    Ok(())
}

/// Select proof of this complete installed world, leaving earlier worlds as history.
pub(crate) async fn reconciliation_evidence(
    tx: &Transaction<'_>,
    tenant: &str,
    roots: &[PathBuf],
) -> anyhow::Result<Option<UpgradeQualification>> {
    let installed = read_current_packages(tx, tenant).await?;
    let mut accepted = Vec::new();
    for package in &installed {
        if package.predecessor_version.is_none() {
            continue;
        }
        if let Some(evidence) =
            read_accepted(tx, tenant, &package.package_id, &package.package_version).await?
        {
            accepted.push(evidence);
        }
    }
    if accepted.is_empty() {
        return Ok(None);
    }
    let mut presented = roots
        .iter()
        .map(|root| qualify_upgrade::presented_root_identity(root))
        .collect::<anyhow::Result<Vec<_>>>()?;
    presented.sort_by(|left, right| left.package.package_id.cmp(&right.package.package_id));
    matching_reconciliation_evidence(&accepted, &installed, &presented).map(Some)
}

fn matching_reconciliation_evidence(
    accepted: &[AcceptedUpgrade],
    installed: &[PackageIdentity],
    presented: &[PresentedRootIdentity],
) -> anyhow::Result<UpgradeQualification> {
    let mut matching = accepted.iter().filter(|accepted| {
        accepted.evidence.presented_packages == installed
            && accepted.evidence.presented_roots == presented
    });
    let selected = &matching
        .next()
        .context(
            "accepted upgrade evidence does not qualify this exact complete presented-root set",
        )?
        .evidence;
    require_prefix(selected)?;
    ensure!(
        installed.contains(&selected.candidate_package),
        "qualified reconciliation candidate is not an installed leaf"
    );
    for accepted in matching {
        require_prefix(&accepted.evidence)?;
        ensure!(
            installed.contains(&accepted.evidence.candidate_package)
                && accepted.evidence.schemas == selected.schemas
                && accepted.evidence.post_privileges == selected.post_privileges,
            "matching upgrade transitions disagree on the canonical data-access post-state"
        );
    }
    Ok(selected.clone())
}

pub(crate) async fn require_reconciled_privileges(
    tx: &Transaction<'_>,
    evidence: &UpgradeQualification,
) -> anyhow::Result<()> {
    ensure!(
        read_upgrade_privileges(tx, &evidence.schemas).await? == evidence.post_privileges,
        "reconciled privileges differ from the qualified complete-root post-state"
    );
    Ok(())
}

/// The one selection/deployment predicate: identical migrations or proved direct rollback.
pub(crate) async fn require_compatible_schema(
    tx: &mut Transaction<'_>,
    manifest: &ServingManifest,
) -> anyhow::Result<()> {
    tx.query_one(crate::reconcile_package_data_access::LOCK_SQL, &[])
        .await?;
    let release = &manifest.release;
    // Acquire every requested lineage in stable order before any head row lock.
    for package in &release.packages {
        tx.query_one(
            crate::apply_package::LOCK_PACKAGE_SQL,
            &[&release.tenant_id, &package.package_id()],
        )
        .await?;
    }
    let installed = read_current_packages(tx, &release.tenant_id).await?;
    for package in &release.packages {
        let leaf = installed
            .iter()
            .find(|leaf| leaf.package_id == package.package_id())
            .context("the target lacks the selected package")?;
        let selected = crate::apply_package::load_applied_package(
            tx,
            &release.tenant_id,
            package.package_id(),
            package.package_version(),
        )
        .await?
        .context("the target lacks the selected package coordinate")?;
        let selected_migrations = selected
            .migrations
            .iter()
            .map(|migration| MigrationIdentity {
                ordinal: migration.ordinal,
                relative_path: migration.relative_path.clone(),
                sha256: migration.sha256.clone(),
            })
            .collect::<Vec<_>>();
        if selected_migrations == leaf.migrations {
            continue;
        }
        let accepted = read_accepted(tx, &release.tenant_id, &leaf.package_id, &leaf.package_version)
            .await?.context("schema-changing release selection requires persisted immediate-predecessor upgrade evidence")?;
        let evidence = &accepted.evidence;
        require_prefix(evidence)?;
        ensure!(
            evidence.environment == release.environment
                && evidence.predecessor_release_id
                    == i32::try_from(release.effective_release_id.get())?
                && evidence.predecessor_manifest_digest == manifest.digest().as_str()
                && evidence.predecessor_package.package_id == package.package_id()
                && evidence.predecessor_package.package_version == package.package_version()
                && evidence.predecessor_package.manifest_sha256 == selected.manifest_sha256
                && evidence.predecessor_package.migrations == selected_migrations
                && evidence.candidate_package == *leaf,
            "selected release is not the qualified immediate predecessor of the installed leaf"
        );
        ensure!(
            installed == evidence.presented_packages,
            "qualified installed package post-state changed"
        );
        ensure!(
            read_upgrade_privileges(tx, &evidence.schemas).await? == evidence.post_privileges,
            "qualified data-access post-state changed"
        );
        // An ungranted out-of-band column can invalidate a whole-row statement
        // without changing migration records or existing privilege facts.
        qualify_upgrade::plan_predecessor_in_transaction(tx, manifest, &evidence.serving_workloads)
            .await
            .context("qualified predecessor no longer plans against the retained schema")?;
    }
    Ok(())
}
