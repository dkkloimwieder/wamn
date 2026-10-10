//! The package stage of `apply` (docs/plan/platform-deploy.md §10.1 step 5,
//! §10.2), run in the project database before any Kubernetes write.
//!
//! Under the lifecycle lock, idempotently: install each package the
//! environment never had from its verified artifact; verify the predecessor
//! (the installed versions are the ones analysis read); apply the stage of
//! each staged successor the release names; write the release cache row;
//! materialize the release's connection requirements from the admitted
//! descriptors the release manifest carries (contract D, R2 (3)); reconcile
//! the packages' data access. A failed stage stops here, and
//! nothing was written to Kubernetes. A backfill that stops on a lock is
//! resumed by applying again.

use std::collections::BTreeMap;

use anyhow::{Context as _, bail, ensure};
use wamn_schema_generator::PackageManifest;

use super::Platform;
use super::analyse::{self, Analysis, Installed, ReleaseFacts, connect};
use super::document::EnvironmentDocument;
use crate::package_artifact::{OpenedPackage, PackageRegistry, PackageSource, open_package_source};

/// Run the package stage of `release`. Returns the manifests of its packages.
///
/// # Errors
///
/// When an artifact cannot be opened, the installed versions changed since
/// analysis, or a package application refuses.
pub async fn stage(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
    release: &ReleaseFacts,
    project_url: &str,
) -> anyhow::Result<Vec<PackageManifest>> {
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);
    let analysed: BTreeMap<String, Installed> = analysis
        .authorities
        .project
        .as_ref()
        .map(|project| project.installed.clone())
        .unwrap_or_default();

    // 1. The predecessor: the installed versions are those analysis judged.
    let observed = installed(platform, document).await?;
    let mut changed = Vec::new();
    for (package, installed) in &analysed {
        let now = observed.get(package).map(|now| now.current.as_str());
        if now != Some(installed.current.as_str()) {
            changed.push(format!(
                "{package}: analysed {}, installed {}",
                installed.current,
                now.unwrap_or("nothing")
            ));
        }
    }
    ensure!(
        changed.is_empty(),
        "the installed packages changed since analysis: {}",
        changed.join("; ")
    );

    // Each package the environment never had, and each staged successor:
    // its verified artifact, applied. A staged successor's stage runs under
    // its stage evidence (`apply_coordinated` and the backfill unchanged in
    // content).
    let mut packages = Vec::new();
    for package in &release.manifest.release.packages {
        let (id, version) = (package.package_id(), package.package_version());
        let opened = open(platform, id, version).await?;
        let applies = match analysed.get(id) {
            None => true,
            Some(installed) => {
                installed.current != version
                    && installed.lineage.get(version).and_then(Option::as_deref)
                        == Some(installed.current.as_str())
            }
        };
        if applies {
            crate::apply_package::apply_package(crate::apply_package::ApplyPackageRequest {
                package: opened.root().to_owned(),
                database_url: project_url.to_owned(),
                tenant: tenant.clone(),
            })
            .await
            .with_context(|| format!("apply package {id}@{version}"))?;
        }
        packages.push((id.to_owned(), version.to_owned(), opened));
    }

    // 2. The release cache row, once its package versions are installed: the
    // row seals the migrations of every version it names.
    write_release(project_url, &tenant, release).await?;
    materialize_requirements(project_url, &tenant, release).await?;

    // 3. The data access of the packages of the release, when each is the
    // installed version. A release on the tested predecessor keeps the
    // expanded schema's grants, which qualify-upgrade tested (§14).
    let installed = installed(platform, document).await?;
    let current = packages.iter().all(|(id, version, _)| {
        installed
            .get(id)
            .is_some_and(|installed| &installed.current == version)
    });
    if current && !packages.is_empty() {
        crate::reconcile_package_data_access::reconcile_package_data_access(
            crate::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
                packages: packages
                    .iter()
                    .map(|(_, _, opened)| opened.root().to_owned())
                    .collect(),
                database_url: project_url.to_owned(),
                tenant: tenant.clone(),
            },
        )
        .await
        .context("reconcile the packages' data access")?;
    }
    packages
        .iter()
        .map(|(_, _, opened)| {
            let path = wamn_schema_generator::package_manifest_path(opened.root());
            PackageManifest::from_slice(
                &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
            )
            .with_context(|| format!("parse {}", path.display()))
        })
        .collect()
}

/// The installed packages of the project database, read now.
pub(super) async fn installed(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<BTreeMap<String, Installed>> {
    let system = connect(&platform.system_database_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let row = analyse::read_row(&system, document)
        .await?
        .context("the environment row is missing after step 4")?;
    Ok(analyse::read_project(platform, document, &row)
        .await?
        .context("the project database is missing after step 4")?
        .installed)
}

/// The package artifact `<id>-<version>`, verified against the digest
/// `catalog.package_artifacts` records.
pub(super) async fn open(
    platform: &Platform,
    id: &str,
    version: &str,
) -> anyhow::Result<OpenedPackage> {
    let Some(base) = &platform.package_artifact_base else {
        bail!(
            "release names package {id}@{version}; set WAMN_PACKAGE_ARTIFACT_BASE to read \
             its artifact"
        );
    };
    open_package_source(PackageSource::Artifact {
        tag: format!("{id}-{version}"),
        registry: PackageRegistry {
            artifact_base: base.clone(),
            registry_auth_file: platform.registry_auth_file.clone(),
            insecure_registry: false,
            oci_ca_paths: platform.oci_ca_paths.clone(),
            control_database_url: platform.system_database_url.clone(),
        },
    })
    .await
    .with_context(|| format!("open package artifact {id}@{version}"))
}

/// Materialize `catalog.connection_requirements` for the release's
/// components in this environment: a runtime projection of the admitted
/// descriptors the release manifest carries, one row for each connection of
/// each component's descriptor (contract D, R2 (3)). An existing row with
/// other bytes refuses.
async fn materialize_requirements(
    project_url: &str,
    tenant: &str,
    release: &ReleaseFacts,
) -> anyhow::Result<()> {
    let requirements = release_requirements(&release.manifest);
    if requirements.is_empty() {
        return Ok(());
    }
    let mut client = connect(project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the connection requirement projection")?;
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .context("claim the tenant")?;
    for requirement in &requirements {
        let json = String::from_utf8(requirement.canonical_bytes())
            .context("a connection requirement is not UTF-8")?;
        let hash = requirement.requirement_hash();
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 5] = [
            &tenant,
            &requirement.component_digest(),
            &requirement.store_alias(),
            &json,
            &hash,
        ];
        transaction
            .execute(
                wamn_schema_control::connections::insert_component_connection_requirement_sql(),
                &params,
            )
            .await
            .context("materialize a connection requirement")?;
        let exact: bool = transaction
            .query_one(
                wamn_schema_control::connections::exact_component_connection_requirement_sql(),
                &params,
            )
            .await
            .context("verify a connection requirement")?
            .get(0);
        ensure!(
            exact,
            "component {} store alias {} has another connection requirement in this environment",
            requirement.component_digest(),
            requirement.store_alias()
        );
    }
    transaction
        .commit()
        .await
        .context("commit the connection requirements")
}

/// The connection requirements of the descriptors of `manifest`: exactly one
/// for each connection of each release component.
fn release_requirements(
    manifest: &wamn_catalog::ServingManifest,
) -> Vec<wamn_catalog::ComponentConnectionRequirement> {
    manifest
        .components
        .iter()
        .flat_map(|component| {
            component.descriptor.connections.iter().map(|connection| {
                crate::push_component::portable_requirement(component.digest.as_str(), connection)
            })
        })
        .collect()
}

/// Write `catalog.releases (manifest_digest, manifest)` for the release if
/// absent (R1). The row is immutable, and its bytes hash to its key.
async fn write_release(
    project_url: &str,
    tenant: &str,
    release: &ReleaseFacts,
) -> anyhow::Result<()> {
    let bytes = release.manifest.canonical_bytes();
    ensure!(
        release.manifest.digest().as_str() == release.digest,
        "release manifest {} hashes to {}",
        release.digest,
        release.manifest.digest().as_str()
    );
    let mut client = connect(project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the release cache write")?;
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .context("claim the tenant")?;
    transaction
        .execute(
            "INSERT INTO catalog.releases (tenant_id, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            &[&tenant, &release.digest, &bytes],
        )
        .await
        .context("write the release cache row")?;
    transaction
        .commit()
        .await
        .context("commit the release cache row")
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{
        ArtifactHash, ComponentConnection, ComponentConnectionType, ComponentDescriptor,
        ConnectionTypeDescriptor, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION,
        ServingComponent, ServingManifest, ServingRelease, WorkflowSection,
    };

    use super::release_requirements;

    fn component(name: &str, digest: &str, aliases: &[&str]) -> ServingComponent {
        let mut descriptor = ComponentDescriptor::named(name, "0.1", digest);
        descriptor.connections = aliases
            .iter()
            .map(|alias| ComponentConnection {
                store_alias: (*alias).to_owned(),
                requirement_type: ComponentConnectionType::Blobstore,
            })
            .collect();
        ServingComponent {
            package_id: "orders".to_owned(),
            component: name.to_owned(),
            interface_version: "0.1".to_owned(),
            digest: ArtifactHash::parse(digest.to_owned()).expect("a digest"),
            operations: BTreeMap::new(),
            descriptor,
        }
    }

    /// Apply writes one requirement for each connection of each release
    /// descriptor, and nothing else (R2 (3)).
    #[test]
    fn the_requirements_are_exactly_those_of_the_release_descriptors() {
        let first = format!("sha256:{}", "1".repeat(64));
        let second = format!("sha256:{}", "2".repeat(64));
        let manifest = ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("orders", "1.0.0").unwrap()]),
            },
            components: BTreeSet::from([
                component("blob-put", &first, &["archive", "labels"]),
                component("orders", &second, &[]),
            ]),
            routes: BTreeSet::new(),
            attachments: BTreeMap::new(),
            workflow: WorkflowSection {
                wirings: BTreeSet::new(),
                attachments: BTreeMap::new(),
                registrations: BTreeMap::new(),
            },
            host_routes: BTreeSet::new(),
        };
        let written = release_requirements(&manifest)
            .iter()
            .map(|requirement| {
                (
                    requirement.component_digest().to_owned(),
                    requirement.store_alias().to_owned(),
                    requirement.requirement().clone(),
                )
            })
            .collect::<Vec<_>>();
        let blobstore = ConnectionTypeDescriptor::blobstore_v1();
        assert_eq!(
            written,
            vec![
                (first.clone(), "archive".to_owned(), blobstore.clone()),
                (first, "labels".to_owned(), blobstore),
            ]
        );
    }
}
