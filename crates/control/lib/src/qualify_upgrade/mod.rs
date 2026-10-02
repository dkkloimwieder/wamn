//! Prove one installed package successor on an owned copy before production mutation.

pub(crate) mod overlay;
mod scratch;
#[cfg(test)]
mod tests;
pub mod workload;

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
use wamn_schema_generator::{MaterializeMode, PackageManifest};
use wamn_schema_introspection::migration_policy::validate_predecessor_compatible_migration_bytes_for_schemas;

use crate::apply_package::{
    ApplyPackageRequest, apply_qualification_package, read_package_directory,
};
use crate::reconcile_package_data_access::ReconcilePackageDataAccessRequest;
use crate::reconcile_package_data_access::upgrade::{
    UpgradePrivileges, read_upgrade_privileges, reconcile_for_upgrade, restore_upgrade_privileges,
};

/// Source project, complete candidate roots, and explicit serving workload selectors.
#[derive(Debug)]
pub struct QualifyUpgradeRequest {
    pub database_url: String,
    pub tenant: String,
    pub environment: String,
    pub package: PathBuf,
    pub presented_packages: Vec<PathBuf>,
    /// Complete original roots for a coordinated base and overlay upgrade.
    pub predecessor_packages: Vec<PathBuf>,
    /// Exact successor base artifact to which affected overlays re-pin.
    pub base_component: Option<PathBuf>,
    pub workload: workload::WorkloadTarget,
    pub result: PathBuf,
}

/// The new immutable result file and its canonical content digest.
#[derive(Debug)]
pub struct UpgradeQualificationOutcome {
    pub result: PathBuf,
    pub sha256: String,
    pub package_id: String,
    pub package_version: String,
}

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
    pub(crate) manifest_sha256: String,
    pub(crate) migrations: Vec<MigrationIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PresentedRootIdentity {
    pub(crate) package: PackageIdentity,
    pub(crate) statement_corpus_sha256: String,
    pub(crate) package_weld_sha256: String,
    pub(crate) data_access_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpgradeQualification {
    pub(crate) format_version: u32,
    pub(crate) tenant: String,
    pub(crate) environment: String,
    pub(crate) predecessor_release_id: i32,
    pub(crate) predecessor_manifest_digest: String,
    pub(crate) predecessor_package: PackageIdentity,
    pub(crate) candidate_package: PackageIdentity,
    pub(crate) candidate_suffix: Vec<MigrationIdentity>,
    pub(crate) predecessor_packages: Vec<PackageIdentity>,
    pub(crate) presented_packages: Vec<PackageIdentity>,
    pub(crate) presented_roots: Vec<PresentedRootIdentity>,
    pub(crate) schemas: Vec<String>,
    pub(crate) predecessor_privileges: UpgradePrivileges,
    pub(crate) post_privileges: UpgradePrivileges,
    pub(crate) workload_target: workload::WorkloadTarget,
    pub(crate) serving_workloads: workload::ServingWorkloads,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) overlay: Option<overlay::OverlayEvidence>,
}

struct PresentedPackage {
    root: PathBuf,
    directory: PackageDirectory,
    manifest: PackageManifest,
    identity: PackageIdentity,
    root_identity: PresentedRootIdentity,
}

enum WorkloadObserver {
    Live,
    #[cfg(test)]
    Captured(workload::ServingWorkloads),
}

/// Qualify a direct successor without writing to the source project database.
pub async fn qualify_upgrade(
    request: QualifyUpgradeRequest,
) -> anyhow::Result<UpgradeQualificationOutcome> {
    qualify_upgrade_with_observer(request, WorkloadObserver::Live).await
}

async fn qualify_upgrade_with_observer(
    request: QualifyUpgradeRequest,
    observer: WorkloadObserver,
) -> anyhow::Result<UpgradeQualificationOutcome> {
    ensure!(
        !request.tenant.is_empty(),
        "upgrade tenant must not be empty"
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
        .canonicalize()
        .context("resolve candidate package root")?;
    let candidate = packages
        .iter()
        .find(|package| package.root == candidate_root)
        .context("candidate package must occur in the complete presented root set")?;
    let schemas = package_schemas(&packages)?;
    let sql_packages = packages
        .iter()
        .filter(|package| !package.manifest.declares_no_sql())
        .map(|package| package.identity.package_id.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        request
            .workload
            .package_workloads
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            == sql_packages,
        "workload selectors must name exactly the SQL-bearing presented packages"
    );

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
        &observer,
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
    // Both callers and the scratch executors consume paths. Recheck their exact
    // migration/manifest inputs before publishing an identity for those bytes.
    for package in packages.iter().chain(&predecessors) {
        ensure!(
            read_package_directory(&package.root)? == package.directory,
            "presented package changed during qualification: {}",
            package.identity.package_id
        );
        ensure!(
            presented_root_identity(&package.root)? == package.root_identity,
            "presented generated or SQL bytes changed during qualification: {}",
            package.identity.package_id
        );
    }
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
    observer: &WorkloadObserver,
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
    let predecessor_packages = read_current_packages(&tx, &request.tenant).await?;
    let (manifest, manifest_digest) =
        read_selected_manifest(&tx, &request.tenant, &request.environment).await?;
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
    ensure!(
        !plan.pending.is_empty(),
        "upgrade qualification requires a non-empty successor suffix"
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
        validate_predecessor_compatible_migration_bytes_for_schemas(
            candidate.root.join(&migration.relative_path),
            &migration.bytes,
            &policy_schemas,
        )?;
    }
    let mut expected_roots = predecessor_packages.clone();
    let upgraded = expected_roots
        .iter_mut()
        .find(|package| package.package_id == candidate.identity.package_id)
        .context("installed predecessor disappeared from the snapshot")?;
    *upgraded = candidate.identity.clone();
    let overlay = if request.predecessor_packages.is_empty() && request.base_component.is_none() {
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
        ensure!(
            predecessors
                .iter()
                .map(|package| package.identity.clone())
                .collect::<Vec<_>>()
                == predecessor_packages,
            "predecessor roots must match every currently installed package and migration identity"
        );
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
    let serving_workloads = match observer {
        WorkloadObserver::Live => {
            workload::observe(
                &request.workload,
                &request.tenant,
                &request.environment,
                &manifest_digest,
            )
            .await?
        }
        #[cfg(test)]
        WorkloadObserver::Captured(serving) => serving.clone(),
    };
    ensure!(
        serving_workloads.manifest_digest == manifest_digest,
        "observed workload is not serving the selected predecessor manifest"
    );
    require_runtime_schemas(packages, &manifest, &serving_workloads)?;
    let predecessor_privileges = read_upgrade_privileges(&tx, schemas).await?;
    let snapshot: String = tx
        .query_one("SELECT pg_export_snapshot()", &[])
        .await
        .context("export the predecessor database snapshot")?
        .get(0);
    let scratch = scratch::copy_database(&request.database_url, &snapshot).await?;
    let evidence = UpgradeQualification {
        format_version: if overlay.is_some() { 2 } else { 1 },
        tenant: request.tenant.clone(),
        environment: request.environment.clone(),
        predecessor_release_id: i32::try_from(manifest.release.effective_release_id.get())?,
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
        presented_roots: packages
            .iter()
            .map(|package| package.root_identity.clone())
            .collect(),
        schemas: schemas.to_vec(),
        predecessor_privileges,
        post_privileges: UpgradePrivileges::default(),
        workload_target: request.workload.clone(),
        serving_workloads,
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
    let candidate = packages
        .iter()
        .find(|package| package.identity == evidence.candidate_package)
        .context("qualified candidate is absent from the captured successor roots")?;
    let tx = client
        .transaction()
        .await
        .context("begin copied predecessor verification")?;
    ensure!(
        read_current_packages(&tx, &request.tenant).await? == evidence.predecessor_packages,
        "copied package state differs from the source snapshot"
    );
    let (copied_manifest, copied_digest) =
        read_selected_manifest(&tx, &request.tenant, &request.environment).await?;
    ensure!(
        copied_manifest == *manifest && copied_digest == evidence.predecessor_manifest_digest,
        "copied release state differs from the source snapshot"
    );
    restore_upgrade_privileges(&tx, &evidence.schemas, &evidence.predecessor_privileges).await?;
    tx.commit()
        .await
        .context("commit exact predecessor scratch privileges")?;
    if evidence.overlay.is_some() {
        let installed = predecessors
            .iter()
            .map(|package| package.manifest.clone())
            .collect::<Vec<_>>();
        for package in predecessors {
            let catalog =
                wamn_schema_generator::introspect_package(database_url, &package.root).await?;
            let projected = wamn_schema_generator::project_package_catalog(
                &catalog,
                &package.manifest,
                &installed,
            )?;
            let schema = evidence
                .serving_workloads
                .packages
                .get(&package.identity.package_id)
                .map_or("public", |workload| workload.schema.as_str());
            wamn_schema_generator::materialize_package_verified_with_existing_grants(
                MaterializeMode::Check,
                &projected,
                database_url,
                &package.root,
                schema,
            )
            .await
            .with_context(|| {
                format!(
                    "verify predecessor generated contracts for {} on the installed copy",
                    package.identity.package_id
                )
            })?;
        }
    }
    if let Some(proof) = &evidence.overlay {
        let mut roots = vec![candidate.root.clone()];
        for transition in &proof.overlays {
            roots.push(
                packages
                    .iter()
                    .find(|package| package.identity == transition.candidate)
                    .context("qualified overlay root is absent")?
                    .root
                    .clone(),
            );
        }
        crate::apply_package::apply_qualification_packages(database_url, &request.tenant, &roots)
            .await
            .context(
                "apply base and overlay successors atomically on the owned predecessor copy",
            )?;
    } else {
        apply_qualification_package(ApplyPackageRequest {
            package: candidate.root.clone(),
            database_url: database_url.to_owned(),
            tenant: request.tenant.clone(),
        })
        .await
        .context("apply candidate suffix on the owned predecessor copy")?;
    }
    plan_predecessor(client, manifest, &evidence.serving_workloads)
        .await
        .context("predecessor statements fail before candidate grant reconciliation")?;
    reconcile_for_upgrade(ReconcilePackageDataAccessRequest {
        packages: packages
            .iter()
            .map(|package| package.root.clone())
            .collect(),
        database_url: database_url.to_owned(),
        tenant: request.tenant.clone(),
    })
    .await
    .context("reconcile the complete candidate root set on scratch")?;
    let tx = client.transaction().await?;
    evidence.post_privileges = read_upgrade_privileges(&tx, &evidence.schemas).await?;
    tx.rollback().await?;
    let installed = packages
        .iter()
        .map(|package| package.manifest.clone())
        .collect::<Vec<_>>();
    for package in packages {
        let catalog =
            wamn_schema_generator::introspect_package(database_url, &package.root).await?;
        let projected = wamn_schema_generator::project_package_catalog(
            &catalog,
            &package.manifest,
            &installed,
        )?;
        let schema = evidence
            .serving_workloads
            .packages
            .get(&package.identity.package_id)
            .map_or("public", |workload| workload.schema.as_str());
        wamn_schema_generator::materialize_package_verified_with_existing_grants(
            MaterializeMode::Check,
            &projected,
            database_url,
            &package.root,
            schema,
        )
        .await
        .with_context(|| {
            format!(
                "check generated candidate artifacts for {}",
                package.identity.package_id
            )
        })?;
        let statements =
            crate::push_component::load_package_statement_facts(&package.root, &package.manifest)?;
        let mut corpus = BTreeMap::new();
        for (operation, statements) in statements {
            for (digest, statement) in statements {
                corpus.insert(
                    format!(
                        "candidate/{}/{operation}/{}/{digest}",
                        package.identity.package_id, statement.path
                    ),
                    statement.sql.into_bytes(),
                );
            }
        }
        if !corpus.is_empty() {
            let schema = &evidence
                .serving_workloads
                .packages
                .get(&package.identity.package_id)
                .context("candidate statements have no observed runtime schema")?
                .schema;
            wamn_schema_generator::classify_statements_with_existing_grants(
                client, &corpus, schema,
            )
            .await
            .with_context(|| {
                format!(
                    "plan candidate statements for {}",
                    package.identity.package_id
                )
            })?;
        }
    }
    plan_predecessor(client, manifest, &evidence.serving_workloads)
        .await
        .context("predecessor statements fail under candidate grants")?;
    let tx = client.transaction().await?;
    ensure!(
        read_current_packages(&tx, &request.tenant).await? == evidence.presented_packages,
        "upgraded copy does not match the qualified package leaves and migrations"
    );
    ensure!(
        read_upgrade_privileges(&tx, &evidence.schemas).await? == evidence.post_privileges,
        "candidate privileges changed during qualification checks"
    );
    tx.rollback().await?;
    Ok(())
}

async fn plan_predecessor(
    client: &mut Client,
    manifest: &ServingManifest,
    serving: &workload::ServingWorkloads,
) -> anyhow::Result<()> {
    let mut transaction = client.transaction().await?;
    let result = plan_predecessor_in_transaction(&mut transaction, manifest, serving).await;
    transaction.rollback().await?;
    result
}

pub(crate) async fn plan_predecessor_in_transaction(
    transaction: &mut Transaction<'_>,
    manifest: &ServingManifest,
    serving: &workload::ServingWorkloads,
) -> anyhow::Result<()> {
    for component in &manifest.components {
        let mut corpus = BTreeMap::new();
        for (operation, admitted) in &component.operations {
            for (digest, statement) in &admitted.statements {
                corpus.insert(
                    format!(
                        "predecessor/{}/{}/{operation}/{}/{digest}",
                        component.package_id, component.component, statement.path
                    ),
                    statement.sql.as_bytes().to_vec(),
                );
            }
        }
        if corpus.is_empty() {
            continue;
        }
        let schema = &serving
            .packages
            .get(&component.package_id)
            .context("predecessor statements have no observed runtime schema")?
            .schema;
        wamn_schema_generator::classify_statements_with_existing_grants_in_transaction(
            transaction,
            &corpus,
            schema,
        )
        .await?;
    }
    Ok(())
}

fn require_runtime_schemas(
    packages: &[PresentedPackage],
    manifest: &ServingManifest,
    serving: &workload::ServingWorkloads,
) -> anyhow::Result<()> {
    for package in packages {
        let schemas =
            wamn_schema_generator::data_access_schemas(&package.directory.manifest_bytes)?;
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
            continue;
        }
        let runtime = serving
            .packages
            .get(&package.identity.package_id)
            .context("SQL-bearing package has no observed runtime workload")?;
        ensure!(
            schemas.as_slice() == [runtime.schema.as_str()],
            "Epic 1 requires one unchanged runtime schema for package {}",
            package.identity.package_id
        );
    }
    Ok(())
}

fn read_presented_packages(roots: &[PathBuf]) -> anyhow::Result<Vec<PresentedPackage>> {
    ensure!(
        !roots.is_empty(),
        "upgrade requires the complete presented package root set"
    );
    let mut packages = BTreeMap::new();
    for root in roots {
        let root = root
            .canonicalize()
            .context("resolve a presented package root")?;
        let directory = read_package_directory(&root)?;
        let manifest = PackageManifest::from_slice(&directory.manifest_bytes)?;
        let identity = identity_from_directory(&directory)?;
        let root_identity = presented_root_identity(&root)?;
        ensure!(
            root_identity.package == identity,
            "presented package changed while loading"
        );
        ensure!(
            packages
                .insert(
                    identity.package_id.clone(),
                    PresentedPackage {
                        root,
                        directory,
                        manifest,
                        identity,
                        root_identity,
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

/// Bind publication SQL and reconciliation artifacts to one exact package root.
pub(crate) fn presented_root_identity(root: &Path) -> anyhow::Result<PresentedRootIdentity> {
    let directory = read_package_directory(root)?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)?;
    let statements = crate::push_component::load_package_statement_facts(root, &manifest)?;
    let statement_corpus_sha256 =
        wamn_execution_contract::canonical_json_sha256(&serde_json::to_value(statements)?);
    let digest_file = |path: &str| -> anyhow::Result<String> {
        let bytes = fs::read(root.join(path))
            .with_context(|| format!("read presented root artifact {path}"))?;
        Ok(bytes_digest(&bytes))
    };
    Ok(PresentedRootIdentity {
        package: identity_from_directory(&directory)?,
        statement_corpus_sha256,
        package_weld_sha256: digest_file("generated/package-weld.json")?,
        data_access_sha256: digest_file(wamn_schema_generator::DATA_ACCESS_OVERLAY_PATH)?,
    })
}

pub(crate) fn identity_from_directory(
    directory: &PackageDirectory,
) -> anyhow::Result<PackageIdentity> {
    let plan = plan_package_migrations(directory, None)?;
    Ok(PackageIdentity {
        package_id: plan.coordinate.package_id().to_owned(),
        package_version: plan.coordinate.package_version().to_owned(),
        predecessor_version: plan.predecessor_version,
        manifest_sha256: plan.manifest_sha256,
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
        manifest_sha256: identity.manifest_sha256.clone(),
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
    tenant: &str,
) -> anyhow::Result<Vec<PackageIdentity>> {
    let rows = tx.query(
        "SELECT p.package_id, p.package_version, p.predecessor_version, p.manifest_sha256 \
           FROM catalog.packages p WHERE p.tenant_id = $1 AND NOT EXISTS \
             (SELECT FROM catalog.packages successor WHERE successor.tenant_id = p.tenant_id \
              AND successor.package_id = p.package_id AND successor.predecessor_version = p.package_version) \
          ORDER BY p.package_id COLLATE \"C\", p.package_version COLLATE \"C\"", &[&tenant],
    ).await.context("read installed package leaves")?;
    let mut packages = Vec::new();
    for row in rows {
        let package_id: String = row.get(0);
        let package_version: String = row.get(1);
        let migrations = tx
            .query(
                "SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
              WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 ORDER BY ordinal",
                &[&tenant, &package_id, &package_version],
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
        packages.push(PackageIdentity {
            package_id,
            package_version,
            predecessor_version: row.get(2),
            manifest_sha256: row.get(3),
            migrations,
        });
    }
    Ok(packages)
}

pub(crate) async fn read_selected_manifest(
    tx: &Transaction<'_>,
    tenant: &str,
    environment: &str,
) -> anyhow::Result<(ServingManifest, String)> {
    let row = tx.query_opt(
        "SELECT head.effective_release_id, snapshot.manifest_digest, snapshot.canonical_bytes \
           FROM catalog.effective_release_heads head \
           JOIN catalog.release_manifest_snapshots snapshot \
             ON snapshot.tenant_id = head.tenant_id AND snapshot.effective_release_id = head.effective_release_id \
          WHERE head.tenant_id = $1 AND head.environment = $2", &[&tenant, &environment],
    ).await.context("read selected predecessor serving manifest")?.context("environment has no selected canonical serving manifest")?;
    let release_id: i32 = row.get(0);
    let digest: String = row.get(1);
    let bytes: Vec<u8> = row.get(2);
    let (manifest, observed) = ServingManifest::from_canonical_bytes(&bytes)?;
    ensure!(
        observed.as_str() == digest
            && manifest.release.tenant_id == tenant
            && manifest.release.environment == environment
            && i32::try_from(manifest.release.effective_release_id.get())? == release_id,
        "selected serving manifest differs from its stored release identity"
    );
    Ok((manifest, digest))
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
        (evidence.format_version == 1 && evidence.overlay.is_none())
            || (evidence.format_version == 2 && evidence.overlay.is_some()),
        "unsupported upgrade qualification format"
    );
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
