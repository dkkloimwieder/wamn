//! Prove one installed package successor on an owned copy before production mutation.

pub(crate) mod overlay;
mod scratch;
pub(crate) mod stage;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, IsolationLevel, NoTls, Transaction};
use wamn_catalog::{PackageCoordinate, ServingManifest};
use wamn_schema_control::{
    AppliedPackage, PackageDirectory, RecordedMigration, plan_package_migrations,
};
use wamn_schema_generator::PackageManifest;
use wamn_schema_introspection::ir::View;
use wamn_schema_introspection::postgres::read_views;

use crate::apply_package::{
    ApplyPackageRequest, PackageRoot, apply_qualification_package, read_package_root,
};
use crate::reconcile_package_data_access::ReconcilePackageDataAccessRequest;
use crate::reconcile_package_data_access::upgrade::{
    UpgradePrivileges, read_upgrade_privileges, reconcile_for_upgrade, restore_upgrade_privileges,
};

/// Source project and complete candidate roots.
#[derive(Debug)]
pub struct QualifyUpgradeRequest {
    pub component_source:
        Option<wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig>,
    pub database_url: String,
    pub org: String,
    pub project: String,
    pub environment: String,
    /// Manifest digest of the release the environment serves. It must be a
    /// row of `catalog.releases` and name the installed package versions
    /// (docs/plan/platform-deploy.md §13).
    pub predecessor_release: String,
    pub package: PackageRoot,
    pub presented_packages: Vec<PackageRoot>,
    /// Complete frozen original roots for an installed package upgrade.
    pub predecessor_packages: Vec<PackageRoot>,
    /// Exact successor base artifact to which affected overlays re-pin.
    pub base_component: Option<PathBuf>,
    pub result: PathBuf,
}

impl QualifyUpgradeRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn env_key(&self) -> String {
        wamn_control_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// The new immutable result file and its canonical content digest.
#[derive(Debug)]
pub struct UpgradeQualificationOutcome {
    pub result: PathBuf,
    pub sha256: String,
    pub package_id: String,
    pub package_version: String,
}

/// The evidence format of a plain upgrade. Versions 1 to 3 carried the
/// `wamn.json` hash and the digest of the generated package metadata in place
/// of the package artifact digest (wamn-vavs4.4). Versions 4 to 6 carried
/// the presented roots with a statement corpus digest and a data access
/// digest beside the artifact digest. Versions 7 to 9 carried the serving
/// workload selectors and the observed workload schemas. None decodes.
pub(crate) const PLAIN_FORMAT: u32 = 10;
/// The evidence format of an upgrade with an overlay.
pub(crate) const OVERLAY_FORMAT: u32 = 11;
/// The evidence format of an upgrade with an online stage.
pub(crate) const STAGE_FORMAT: u32 = 12;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MigrationIdentity {
    pub(crate) ordinal: u32,
    pub(crate) relative_path: String,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageIdentity {
    pub(crate) package_id: String,
    pub(crate) package_version: String,
    pub(crate) predecessor_version: Option<String>,
    pub(crate) artifact_digest: String,
    pub(crate) migrations: Vec<MigrationIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpgradeQualification {
    pub(crate) format_version: u32,
    pub(crate) env_key: String,
    pub(crate) environment: String,
    pub(crate) predecessor_manifest_digest: String,
    pub(crate) predecessor_package: PackageIdentity,
    pub(crate) candidate_package: PackageIdentity,
    pub(crate) candidate_suffix: Vec<MigrationIdentity>,
    pub(crate) predecessor_packages: Vec<PackageIdentity>,
    pub(crate) presented_packages: Vec<PackageIdentity>,
    pub(crate) schemas: Vec<String>,
    pub(crate) predecessor_privileges: UpgradePrivileges,
    pub(crate) post_privileges: UpgradePrivileges,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) overlay: Option<overlay::OverlayEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) upgrade_stage: Option<wamn_schema_generator::UpgradeStage>,
}

struct PresentedPackage {
    root: PackageRoot,
    directory: PackageDirectory,
    manifest: PackageManifest,
    identity: PackageIdentity,
}

/// Qualify a direct successor without writing to the source project database.
pub async fn qualify_upgrade(
    request: QualifyUpgradeRequest,
) -> anyhow::Result<UpgradeQualificationOutcome> {
    qualify_upgrade_before_publication(request, || {}).await
}

/// Qualify, running `before_publication` after the proof and before the
/// inputs are rechecked and the evidence is written.
async fn qualify_upgrade_before_publication(
    request: QualifyUpgradeRequest,
    before_publication: impl FnOnce() + Send,
) -> anyhow::Result<UpgradeQualificationOutcome> {
    ensure!(
        ![&request.org, &request.project, &request.environment]
            .iter()
            .any(|part| part.is_empty()),
        "upgrade org, project and environment must not be empty"
    );
    ensure!(
        !request.environment.is_empty(),
        "upgrade environment must not be empty"
    );
    ensure!(
        !request.result.exists(),
        "upgrade result must be a new file"
    );
    let packages = read_presented_packages(&request.presented_packages)?;
    let predecessors = if request.predecessor_packages.is_empty() {
        Vec::new()
    } else {
        read_presented_packages(&request.predecessor_packages)?
    };
    let candidate_root = request
        .package
        .path
        .canonicalize()
        .context("resolve candidate package root")?;
    let candidate = packages
        .iter()
        .find(|package| package.root.path == candidate_root)
        .context("candidate package must occur in the complete presented root set")?;
    for package in &packages {
        stage::require_executor(&package.manifest, &package.directory)?;
    }
    let schemas = package_schemas(&packages)?;

    let (mut source, connection) = tokio_postgres::connect(&request.database_url, NoTls)
        .await
        .context("connect to the installed predecessor project")?;
    let source_task = tokio::spawn(connection);
    let copied = copy_predecessor(
        &mut source,
        &request,
        &packages,
        &predecessors,
        candidate,
        &schemas,
    )
    .await;
    drop(source);
    if copied.is_err() {
        source_task.abort();
    } else {
        source_task
            .await
            .context("join predecessor snapshot connection")?
            .context("drive predecessor snapshot connection")?;
    }
    let (scratch, mut evidence, predecessor_manifest) = copied?;
    let proof = prove_copy(
        scratch.url(),
        &request,
        &packages,
        &predecessors,
        &predecessor_manifest,
        &mut evidence,
    )
    .await;
    let cleanup = scratch.finish().await;
    if let Err(cleanup) = cleanup {
        return Err(match proof {
            Ok(()) => cleanup,
            Err(proof) => proof.context(format!("scratch cleanup also failed: {cleanup:#}")),
        });
    }
    proof?;
    before_publication();
    // Both callers and the scratch executors consume paths. Recheck their exact
    // migration/manifest inputs before publishing an identity for those bytes.
    for package in packages.iter().chain(&predecessors) {
        ensure!(
            read_package_root(&package.root)? == package.directory,
            "presented package changed during qualification: {}",
            package.identity.package_id
        );
    }
    require_predecessor_statement_facts(&predecessors, &predecessor_manifest)?;
    if let Some(qualified) = &evidence.overlay {
        let predecessor_base = predecessors
            .iter()
            .find(|package| package.identity.package_id == candidate.identity.package_id)
            .context("predecessor roots omit the upgraded base")?;
        let rechecked = overlay::validate_transition(
            predecessor_base,
            candidate,
            &predecessors,
            &packages,
            &predecessor_manifest,
            request
                .base_component
                .as_deref()
                .context("coordinated upgrade requires the exact base component artifact")?,
        )?;
        ensure!(
            rechecked == *qualified,
            "consumed contracts or base component bytes changed during qualification"
        );
    }
    let value = serde_json::to_value(&evidence).context("serialize upgrade qualification")?;
    let bytes = wamn_execution_contract::canonical_json_bytes(&value);
    let sha256 = wamn_execution_contract::canonical_json_sha256(&value);
    write_result(&request.result, &bytes)?;
    Ok(UpgradeQualificationOutcome {
        result: request.result,
        sha256,
        package_id: evidence.candidate_package.package_id,
        package_version: evidence.candidate_package.package_version,
    })
}

async fn copy_predecessor(
    source: &mut Client,
    request: &QualifyUpgradeRequest,
    packages: &[PresentedPackage],
    predecessors: &[PresentedPackage],
    candidate: &PresentedPackage,
    schemas: &[String],
) -> anyhow::Result<(
    scratch::ScratchDatabase,
    UpgradeQualification,
    ServingManifest,
)> {
    let tx = source
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .context("begin the read-only predecessor snapshot")?;
    let carrier_installed: bool = tx
        .query_one(
            "SELECT to_regclass('catalog.package_upgrade_qualifications') IS NOT NULL",
            &[],
        )
        .await?
        .get(0);
    ensure!(
        carrier_installed,
        "upgrade-schema must install the package qualification carrier before qualification"
    );
    let predecessor_packages = read_current_packages(&tx, &request.env_key()).await?;
    let manifest_digest = request.predecessor_release.clone();
    let manifest = read_release_manifest(&tx, &request.env_key(), &manifest_digest).await?;
    require_installed_release(
        &manifest_digest,
        manifest
            .release
            .packages
            .iter()
            .map(|package| (package.package_id(), package.package_version())),
        &predecessor_packages,
    )?;
    let predecessor = predecessor_packages
        .iter()
        .find(|package| package.package_id == candidate.identity.package_id)
        .context("candidate package has no installed predecessor")?;
    ensure!(
        candidate.identity.predecessor_version.as_deref()
            == Some(predecessor.package_version.as_str()),
        "candidate must name the current installed leaf as its direct predecessor"
    );
    let plan = plan_package_migrations(&candidate.directory, Some(&applied_package(predecessor)?))
        .context("verify the inherited migration prefix")?;
    if candidate
        .manifest
        .upgrade_stage
        .as_ref()
        .is_some_and(|stage| stage.phase == wamn_schema_generator::UpgradeStagePhase::Backfill)
    {
        ensure!(
            plan.pending.is_empty(),
            "backfill stage cannot carry a DDL suffix; apply an expand version first"
        );
    }
    ensure!(
        !plan.pending.is_empty()
            || candidate.manifest.upgrade_stage.as_ref().is_some_and(
                |stage| stage.phase == wamn_schema_generator::UpgradeStagePhase::Backfill
            ),
        "upgrade qualification requires a successor suffix or a backfill stage"
    );
    let candidate_schemas =
        wamn_schema_generator::data_access_schemas(&candidate.directory.manifest_bytes)?;
    let policy_schemas = candidate_schemas
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    for pending in &plan.pending {
        let migration = candidate
            .directory
            .migrations
            .iter()
            .find(|migration| migration.relative_path == pending.relative_path)
            .context("planned successor migration is missing")?;
        stage::validate_successor_migration(
            candidate.root.path.join(&migration.relative_path),
            &migration.bytes,
            &policy_schemas,
            candidate.manifest.upgrade_stage.as_ref(),
        )?;
    }
    let mut expected_roots = predecessor_packages.clone();
    let upgraded = expected_roots
        .iter_mut()
        .find(|package| package.package_id == candidate.identity.package_id)
        .context("installed predecessor disappeared from the snapshot")?;
    *upgraded = candidate.identity.clone();
    if !predecessors.is_empty() || request.base_component.is_some() {
        ensure!(
            predecessors.len() == predecessor_packages.len(),
            "predecessor package count mismatch: supplied {}, installed {}",
            predecessors.len(),
            predecessor_packages.len()
        );
        for package in predecessors {
            let original = &package.identity;
            let installed = predecessor_packages
                .iter()
                .find(|installed| installed.package_id == original.package_id)
                .with_context(|| {
                    format!(
                        "predecessor package {} is not installed",
                        original.package_id
                    )
                })?;
            ensure!(
                original.package_version == installed.package_version,
                "predecessor version mismatch for {}: supplied {}, installed {}",
                original.package_id,
                original.package_version,
                installed.package_version
            );
            ensure!(
                original.artifact_digest == installed.artifact_digest,
                "predecessor artifact digest mismatch for {}@{}: supplied {}, installed {}",
                original.package_id,
                original.package_version,
                original.artifact_digest,
                installed.artifact_digest
            );
            ensure!(
                original.migrations.len() == installed.migrations.len(),
                "predecessor migration count mismatch for {}@{}: supplied {}, installed {}",
                original.package_id,
                original.package_version,
                original.migrations.len(),
                installed.migrations.len()
            );
            for (supplied, recorded) in original.migrations.iter().zip(&installed.migrations) {
                ensure!(
                    supplied == recorded,
                    "predecessor migration identity mismatch for {}@{} at ordinal {}: supplied {} {}, installed {} {}",
                    original.package_id,
                    original.package_version,
                    recorded.ordinal,
                    supplied.relative_path,
                    supplied.sha256,
                    recorded.relative_path,
                    recorded.sha256
                );
            }
            ensure!(
                original == installed,
                "predecessor lineage identity mismatch for {}@{}",
                original.package_id,
                original.package_version
            );
        }
    }
    require_predecessor_statement_facts(predecessors, &manifest)?;
    let overlay = if request.base_component.is_none() {
        for package in packages {
            ensure!(
                !package
                    .manifest
                    .base_dependencies
                    .values()
                    .any(|pin| pin.package == candidate.identity.package_id),
                "base-only upgrade of {} is refused: installed overlay {} requires a coordinated successor qualification",
                candidate.identity.package_id,
                package.identity.package_id
            );
        }
        ensure!(
            candidate.manifest.base_dependencies.is_empty(),
            "overlay successors require coordinated base upgrade qualification"
        );
        None
    } else {
        let predecessor_base = predecessors
            .iter()
            .find(|package| package.identity.package_id == candidate.identity.package_id)
            .context("predecessor roots omit the upgraded base")?;
        let proof = overlay::validate_transition(
            predecessor_base,
            candidate,
            predecessors,
            packages,
            &manifest,
            request
                .base_component
                .as_deref()
                .context("coordinated upgrade requires the exact base component artifact")?,
        )?;
        for transition in &proof.overlays {
            let installed = expected_roots
                .iter_mut()
                .find(|package| package.package_id == transition.predecessor.package_id)
                .context("qualified overlay is not installed")?;
            ensure!(
                *installed == transition.predecessor,
                "qualified overlay predecessor differs from installed identity"
            );
            *installed = transition.candidate.clone();
        }
        Some(proof)
    };
    ensure!(
        expected_roots
            == packages
                .iter()
                .map(|package| package.identity.clone())
                .collect::<Vec<_>>(),
        "presented roots must contain exactly the installed leaves with only the qualified base and overlay successors advanced"
    );
    ensure!(
        manifest
            .release
            .packages
            .iter()
            .any(|package| package.package_id() == predecessor.package_id
                && package.package_version() == predecessor.package_version),
        "selected release does not contain the installed predecessor coordinate"
    );
    require_runtime_schemas(packages, &manifest)?;
    let predecessor_privileges = read_upgrade_privileges(&tx, schemas).await?;
    let snapshot: String = tx
        .query_one("SELECT pg_export_snapshot()", &[])
        .await
        .context("export the predecessor database snapshot")?
        .get(0);
    let scratch = scratch::copy_database(&request.database_url, &snapshot).await?;
    let evidence = UpgradeQualification {
        format_version: if candidate.manifest.upgrade_stage.is_some() {
            STAGE_FORMAT
        } else if overlay.is_some() {
            OVERLAY_FORMAT
        } else {
            PLAIN_FORMAT
        },
        upgrade_stage: candidate.manifest.upgrade_stage.clone(),
        env_key: request.env_key(),
        environment: request.environment.clone(),
        predecessor_manifest_digest: manifest_digest,
        predecessor_package: predecessor.clone(),
        candidate_package: candidate.identity.clone(),
        candidate_suffix: plan
            .pending
            .into_iter()
            .map(|migration| MigrationIdentity {
                ordinal: migration.ordinal,
                relative_path: migration.relative_path,
                sha256: migration.sha256,
            })
            .collect(),
        overlay,
        predecessor_packages,
        presented_packages: packages
            .iter()
            .map(|package| package.identity.clone())
            .collect(),
        schemas: schemas.to_vec(),
        predecessor_privileges,
        post_privileges: UpgradePrivileges::default(),
    };
    tx.rollback()
        .await
        .context("finish the read-only predecessor snapshot")?;
    Ok((scratch, evidence, manifest))
}

async fn prove_copy(
    database_url: &str,
    request: &QualifyUpgradeRequest,
    packages: &[PresentedPackage],
    predecessors: &[PresentedPackage],
    manifest: &ServingManifest,
    evidence: &mut UpgradeQualification,
) -> anyhow::Result<()> {
    let (mut client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the owned upgrade copy")?;
    let task = tokio::spawn(connection);
    let result = prove_connected_copy(
        &mut client,
        database_url,
        request,
        packages,
        predecessors,
        manifest,
        evidence,
    )
    .await;
    drop(client);
    if result.is_err() {
        task.abort();
    } else {
        task.await
            .context("join upgrade proof connection")?
            .context("drive upgrade proof connection")?;
    }
    result
}

async fn prove_connected_copy(
    client: &mut Client,
    database_url: &str,
    request: &QualifyUpgradeRequest,
    packages: &[PresentedPackage],
    predecessors: &[PresentedPackage],
    manifest: &ServingManifest,
    evidence: &mut UpgradeQualification,
) -> anyhow::Result<()> {
    let runtime = &runtime_schemas(packages.iter().map(|package| &package.directory), manifest)?;
    let candidate = packages
        .iter()
        .find(|package| package.identity == evidence.candidate_package)
        .context("qualified candidate is absent from the captured successor roots")?;
    let tx = client
        .transaction()
        .await
        .context("begin copied predecessor verification")?;
    ensure!(
        read_current_packages(&tx, &request.env_key()).await? == evidence.predecessor_packages,
        "copied package state differs from the source snapshot"
    );
    let copied_manifest = read_release_manifest(
        &tx,
        &request.env_key(),
        &evidence.predecessor_manifest_digest,
    )
    .await?;
    ensure!(
        copied_manifest == *manifest,
        "copied release state differs from the source snapshot"
    );
    restore_upgrade_privileges(&tx, &evidence.schemas, &evidence.predecessor_privileges).await?;
    tx.commit()
        .await
        .context("commit exact predecessor scratch privileges")?;
    require_predecessor_statement_facts(predecessors, manifest)?;
    let schemas = evidence
        .schemas
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let predecessor_views = read_views(client, &schemas)
        .await
        .context("read predecessor views on the owned copy")?;
    if evidence.overlay.is_some() || stage::whole_row_grants(evidence.upgrade_stage.as_ref()) {
        let mut roots = vec![candidate.root.clone()];
        for transition in evidence
            .overlay
            .iter()
            .flat_map(|overlay| &overlay.overlays)
        {
            roots.push(
                packages
                    .iter()
                    .find(|package| package.identity == transition.candidate)
                    .context("qualified overlay root is absent")?
                    .root
                    .clone(),
            );
        }
        let complete_roots = packages
            .iter()
            .map(|package| package.root.clone())
            .collect::<Vec<_>>();
        crate::apply_package::apply_qualification_packages(
            database_url,
            &request.env_key(),
            &roots,
            &complete_roots,
            manifest,
            request.component_source.as_ref(),
        )
        .await
        .context("apply base and overlay successors atomically on the owned predecessor copy")?;
    } else {
        apply_qualification_package(ApplyPackageRequest {
            component_source: request.component_source.clone(),
            changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
            package: candidate.root.clone(),
            database_url: database_url.to_owned(),
            org: request.org.clone(),
            project: request.project.clone(),
            environment: request.environment.clone(),
        })
        .await
        .context("apply candidate suffix on the owned predecessor copy")?;
    }
    let successor_views = read_views(client, &schemas)
        .await
        .context("read successor views on the upgraded copy")?;
    require_predecessor_views(&predecessor_views, &successor_views)?;
    plan_predecessor(client, manifest, runtime)
        .await
        .context("predecessor statements fail before candidate grant reconciliation")?;
    reconcile_for_upgrade(ReconcilePackageDataAccessRequest {
        packages: packages
            .iter()
            .map(|package| package.root.clone())
            .collect(),
        database_url: database_url.to_owned(),
        org: request.org.clone(),
        project: request.project.clone(),
        environment: request.environment.clone(),
    })
    .await
    .context("reconcile the complete candidate root set on scratch")?;
    let tx = client.transaction().await?;
    evidence.post_privileges = read_upgrade_privileges(&tx, &evidence.schemas).await?;
    tx.rollback().await?;
    plan_predecessor(client, manifest, runtime)
        .await
        .context("predecessor statements fail under candidate grants")?;
    let tx = client.transaction().await?;
    ensure!(
        read_current_packages(&tx, &request.env_key()).await? == evidence.presented_packages,
        "upgraded copy does not match the qualified package leaves and migrations"
    );
    ensure!(
        read_upgrade_privileges(&tx, &evidence.schemas).await? == evidence.post_privileges,
        "candidate privileges changed during qualification checks"
    );
    tx.rollback().await?;
    Ok(())
}

fn require_predecessor_views(predecessor: &[View], successor: &[View]) -> anyhow::Result<()> {
    for view in predecessor {
        let schema = view.schema();
        let name = view.name();
        let successor = successor
            .iter()
            .find(|candidate| candidate.schema() == schema && candidate.name() == name)
            .with_context(|| format!("predecessor view {schema}.{name}: missing view"))?;
        for column in view.columns() {
            let column_name = column.name();
            let successor_column = successor
                .columns()
                .iter()
                .find(|candidate| candidate.name() == column_name)
                .with_context(|| {
                    format!("predecessor view {schema}.{name}: missing column {column_name}")
                })?;
            ensure!(
                column.column_type() == successor_column.column_type(),
                "predecessor view {schema}.{name}: column {column_name} changed type from {} to {}",
                column.column_type().as_str(),
                successor_column.column_type().as_str(),
            );
        }
    }
    Ok(())
}

fn require_predecessor_statement_facts(
    predecessors: &[PresentedPackage],
    manifest: &ServingManifest,
) -> anyhow::Result<()> {
    if predecessors.is_empty() {
        return Ok(());
    }
    let mut corpus = BTreeMap::<String, Vec<wamn_catalog::ComponentSqlStatement>>::new();
    for package in predecessors {
        let frozen = crate::component_admission::load_package_statement_facts(
            &package.root.path,
            &package.manifest,
        )
        .with_context(|| {
            format!(
                "load frozen predecessor statement facts for {}",
                package.identity.package_id
            )
        })?;
        for statements in frozen.values() {
            for (digest, statement) in statements {
                corpus
                    .entry(digest.clone())
                    .or_default()
                    .push(statement.clone());
            }
        }
        for component in manifest
            .components
            .iter()
            .filter(|component| component.package_id == package.identity.package_id)
        {
            for admitted in component.operations.values() {
                let Some(operation) = admitted.registered_operation.as_deref() else {
                    continue;
                };
                let statements = frozen.get(operation).with_context(|| {
                    format!(
                        "serving predecessor operation {operation} has no frozen artifact contract"
                    )
                })?;
                // Serving statements include the dependency call graph. Compare
                // this operation's own frozen facts without rejecting that union.
                for (digest, statement) in statements {
                    ensure!(
                        admitted.statements.get(digest) == Some(statement),
                        "frozen predecessor statement differs from serving manifest for {operation}: {} {digest}",
                        statement.path
                    );
                }
            }
        }
    }
    for component in &manifest.components {
        for (operation, admitted) in &component.operations {
            for (digest, statement) in &admitted.statements {
                ensure!(
                    corpus
                        .get(digest)
                        .is_some_and(|facts| facts.contains(statement)),
                    "serving predecessor statement is absent from frozen artifact corpus for {}/{operation}: {} {digest}",
                    component.package_id,
                    statement.path
                );
            }
        }
    }
    Ok(())
}

async fn plan_predecessor(
    client: &mut Client,
    manifest: &ServingManifest,
    runtime: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let mut transaction = client.transaction().await?;
    let result = plan_predecessor_in_transaction(&mut transaction, manifest, runtime).await;
    transaction.rollback().await?;
    result
}

pub(crate) async fn plan_predecessor_in_transaction(
    transaction: &mut Transaction<'_>,
    manifest: &ServingManifest,
    runtime: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for component in &manifest.components {
        let mut corpus = BTreeMap::new();
        for (operation, admitted) in &component.operations {
            for (digest, statement) in &admitted.statements {
                corpus.insert(
                    format!(
                        "predecessor/{}/{}/{operation}/{}/{digest}",
                        component.package_id, component.descriptor.component, statement.path
                    ),
                    statement.sql.as_bytes().to_vec(),
                );
            }
        }
        if corpus.is_empty() {
            continue;
        }
        let schema = runtime
            .get(&component.package_id)
            .context("predecessor statements have no runtime schema")?;
        wamn_schema_generator::classify_statements_with_existing_grants_in_transaction(
            transaction,
            &corpus,
            schema,
        )
        .await?;
    }
    Ok(())
}

/// The single application schema of each SQL-bearing package, by package id.
pub(crate) fn runtime_schemas<'a>(
    directories: impl IntoIterator<Item = &'a PackageDirectory>,
    release: &ServingManifest,
) -> anyhow::Result<BTreeMap<String, String>> {
    let manifests = directories
        .into_iter()
        .map(|directory| {
            let manifest = PackageManifest::from_slice(&directory.manifest_bytes)?;
            Ok((manifest.package.id.clone(), manifest))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let mut runtime = BTreeMap::new();
    for manifest in manifests.values() {
        if manifest.declares_no_sql() {
            continue;
        }
        let schema = crate::publish_release::application_runtime_schema(
            manifest,
            &manifests,
            &release.components,
        )?
        .context("SQL-bearing package has no runtime schema")?;
        let pinned = release
            .components
            .iter()
            .find(|component| component.package_id == manifest.package.id)
            .and_then(|component| component.runtime_schema.as_ref());
        if let Some(pinned) = pinned {
            ensure!(
                pinned == &schema,
                "runtime schema changed for package {}: released {pinned}, candidate {schema}",
                manifest.package.id
            );
        }
        runtime.insert(
            manifest.package.id.clone(),
            pinned.cloned().unwrap_or(schema),
        );
    }
    Ok(runtime)
}

fn require_runtime_schemas(
    packages: &[PresentedPackage],
    manifest: &ServingManifest,
) -> anyhow::Result<()> {
    runtime_schemas(packages.iter().map(|package| &package.directory), manifest)?;
    for package in packages {
        if package.manifest.declares_no_sql() {
            ensure!(
                !manifest
                    .components
                    .iter()
                    .filter(|component| component.package_id == package.identity.package_id)
                    .any(|component| component
                        .operations
                        .values()
                        .any(|operation| !operation.statements.is_empty())),
                "a SQL-bearing predecessor cannot transition to a schema-free candidate in Epic 1"
            );
        }
    }
    Ok(())
}

fn read_presented_packages(roots: &[PackageRoot]) -> anyhow::Result<Vec<PresentedPackage>> {
    ensure!(
        !roots.is_empty(),
        "upgrade requires the complete presented package root set"
    );
    let mut packages = BTreeMap::new();
    for root in roots {
        let root = PackageRoot {
            path: root
                .path
                .canonicalize()
                .context("resolve a presented package root")?,
            artifact_digest: root.artifact_digest.clone(),
        };
        let directory = read_package_root(&root)?;
        let manifest = PackageManifest::from_slice(&directory.manifest_bytes)?;
        let identity = identity_from_directory(&directory)?;
        ensure!(
            packages
                .insert(
                    identity.package_id.clone(),
                    PresentedPackage {
                        root,
                        directory,
                        manifest,
                        identity,
                    }
                )
                .is_none(),
            "presented roots repeat a package identity"
        );
    }
    Ok(packages.into_values().collect())
}

fn package_schemas(packages: &[PresentedPackage]) -> anyhow::Result<Vec<String>> {
    let mut schemas = BTreeSet::new();
    for package in packages {
        schemas.extend(wamn_schema_generator::data_access_schemas(
            &package.directory.manifest_bytes,
        )?);
    }
    Ok(schemas.into_iter().collect())
}

/// The identity of one exact package root: its package artifact digest and
/// migrations.
pub(crate) fn presented_root_identity(root: &PackageRoot) -> anyhow::Result<PackageIdentity> {
    identity_from_directory(&read_package_root(root)?)
}

pub(crate) fn identity_from_directory(
    directory: &PackageDirectory,
) -> anyhow::Result<PackageIdentity> {
    let plan = plan_package_migrations(directory, None)?;
    let artifact_digest = plan.artifact_digest.with_context(|| {
        format!(
            "{}: {}@{} is a working tree; qualify the verified package artifact",
            crate::apply_package::PACKAGE_ARTIFACT_REQUIRED_REFUSAL,
            plan.coordinate.package_id(),
            plan.coordinate.package_version()
        )
    })?;
    Ok(PackageIdentity {
        package_id: plan.coordinate.package_id().to_owned(),
        package_version: plan.coordinate.package_version().to_owned(),
        predecessor_version: plan.predecessor_version,
        artifact_digest,
        migrations: plan
            .pending
            .into_iter()
            .map(|migration| MigrationIdentity {
                ordinal: migration.ordinal,
                relative_path: migration.relative_path,
                sha256: migration.sha256,
            })
            .collect(),
    })
}

fn applied_package(identity: &PackageIdentity) -> anyhow::Result<AppliedPackage> {
    Ok(AppliedPackage {
        coordinate: PackageCoordinate::new(&identity.package_id, &identity.package_version)?,
        predecessor_version: identity.predecessor_version.clone(),
        artifact_digest: Some(identity.artifact_digest.clone()),
        migrations: identity
            .migrations
            .iter()
            .map(|migration| RecordedMigration {
                ordinal: migration.ordinal,
                relative_path: migration.relative_path.clone(),
                sha256: migration.sha256.clone(),
            })
            .collect(),
    })
}

/// Read immutable lineage leaves without locking or mutating the source database.
pub(crate) async fn read_current_packages(
    tx: &Transaction<'_>,
    env_key: &str,
) -> anyhow::Result<Vec<PackageIdentity>> {
    let rows = tx.query(
        "SELECT p.package_id, p.package_version, p.predecessor_version, p.artifact_digest \
           FROM catalog.packages p WHERE p.env_key = $1 AND NOT EXISTS \
             (SELECT FROM catalog.packages successor WHERE successor.env_key = p.env_key \
              AND successor.package_id = p.package_id AND successor.predecessor_version = p.package_version) \
          ORDER BY p.package_id COLLATE \"C\", p.package_version COLLATE \"C\"", &[&env_key],
    ).await.context("read installed package leaves")?;
    let mut packages = Vec::new();
    for row in rows {
        let package_id: String = row.get(0);
        let package_version: String = row.get(1);
        let migrations = tx
            .query(
                "SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
              WHERE env_key = $1 AND package_id = $2 AND package_version = $3 ORDER BY ordinal",
                &[&env_key, &package_id, &package_version],
            )
            .await
            .context("read installed migration identities")?
            .into_iter()
            .map(|row| {
                Ok(MigrationIdentity {
                    ordinal: u32::try_from(row.get::<_, i32>(0))?,
                    relative_path: row.get(1),
                    sha256: row.get(2),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let artifact_digest = row.get::<_, Option<String>>(3).with_context(|| {
            format!(
                "{}: {package_id}@{package_version} was applied by wamn dev from a working tree; recreate the environment and apply verified artifacts",
                wamn_schema_control::PACKAGE_COORDINATE_WITHOUT_ARTIFACT_REFUSAL
            )
        })?;
        packages.push(PackageIdentity {
            package_id,
            package_version,
            predecessor_version: row.get(2),
            artifact_digest,
            migrations,
        });
    }
    Ok(packages)
}

/// The release `digest` from the digest-keyed release cache of the project
/// database (docs/plan/platform-deploy.md §13).
pub(crate) async fn read_release_manifest(
    tx: &Transaction<'_>,
    env_key: &str,
    digest: &str,
) -> anyhow::Result<ServingManifest> {
    let row = tx
        .query_opt(
            "SELECT canonical_bytes FROM catalog.releases \
              WHERE env_key = $1 AND manifest_digest = $2",
            &[&env_key, &digest],
        )
        .await
        .context("read the predecessor release")?
        .with_context(|| format!("catalog.releases holds no release {digest}"))?;
    let (manifest, observed) = ServingManifest::from_canonical_bytes(&row.get::<_, Vec<u8>>(0))?;
    ensure!(
        observed.as_str() == digest,
        "the cached release differs from its digest"
    );
    Ok(manifest)
}

/// Refuse a predecessor release that names a package version other than the
/// installed one, or a package that is not installed.
fn require_installed_release<'a>(
    digest: &str,
    released: impl IntoIterator<Item = (&'a str, &'a str)>,
    installed: &[PackageIdentity],
) -> anyhow::Result<()> {
    for (package_id, package_version) in released {
        let installed = installed
            .iter()
            .find(|installed| installed.package_id == package_id)
            .with_context(|| {
                format!(
                    "predecessor release {digest} names {package_id}@{package_version}, which is not installed"
                )
            })?;
        ensure!(
            installed.package_version == package_version,
            "predecessor release {digest} names {package_id}@{package_version}, but {package_id}@{} is installed",
            installed.package_version
        );
    }
    Ok(())
}

/// Admit only the canonical upgrade result format, distinct from release qualification.
pub(crate) fn read_qualification(
    path: &Path,
) -> anyhow::Result<(UpgradeQualification, Vec<u8>, String)> {
    let bytes =
        fs::read(path).with_context(|| format!("read upgrade qualification {}", path.display()))?;
    let evidence = decode_qualification(&bytes)?;
    let digest = bytes_digest(&bytes);
    Ok((evidence, bytes, digest))
}

pub(crate) fn decode_qualification(bytes: &[u8]) -> anyhow::Result<UpgradeQualification> {
    let evidence: UpgradeQualification =
        serde_json::from_slice(bytes).context("parse upgrade qualification")?;
    ensure!(
        (evidence.format_version == PLAIN_FORMAT
            && evidence.overlay.is_none()
            && evidence.upgrade_stage.is_none())
            || (evidence.format_version == OVERLAY_FORMAT
                && evidence.overlay.is_some()
                && evidence.upgrade_stage.is_none())
            || (evidence.format_version == STAGE_FORMAT && evidence.upgrade_stage.is_some()),
        "unsupported upgrade qualification format"
    );
    if let Some(stage) = &evidence.upgrade_stage {
        wamn_schema_generator::validate_upgrade_stage(stage)?;
        ensure!(
            evidence.candidate_package.predecessor_version.as_deref()
                == Some(evidence.predecessor_package.package_version.as_str())
                && evidence.candidate_package.package_id == evidence.predecessor_package.package_id,
            "upgrade stage does not name the exact predecessor package"
        );
    }
    let value = serde_json::to_value(&evidence)?;
    ensure!(
        wamn_execution_contract::canonical_json_bytes(&value) == bytes,
        "upgrade qualification is not canonical"
    );
    Ok(evidence)
}

fn write_result(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create new upgrade qualification {}", path.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error).context("write complete upgrade qualification");
    }
    Ok(())
}

fn bytes_digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
    )
}
