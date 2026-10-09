//! The package stage of `apply` (docs/plan/platform-deploy.md §10.1 step 5,
//! §10.2), run in the project database before any Kubernetes write.
//!
//! Under the lifecycle lock, idempotently: install each package the
//! environment never had from its verified artifact; verify the predecessor
//! (the installed versions are the ones analysis read); apply the stage of
//! each staged successor the release names; write the release cache row;
//! materialize the release's connection requirements from the admitted
//! descriptors in its package artifacts (contract D, R3); reconcile the
//! packages' data access. A failed stage stops here, and
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
    materialize_requirements(project_url, &tenant, release, &packages).await?;

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
/// descriptors, the `connections` of each component's declaration in its
/// package artifact (contract D). A declaration whose store alias is the
/// placeholder takes the alias its package's wiring gives the component. A
/// component with no declaration in the artifact declares no connection. An
/// existing row with other bytes refuses.
async fn materialize_requirements(
    project_url: &str,
    tenant: &str,
    release: &ReleaseFacts,
    packages: &[(String, String, OpenedPackage)],
) -> anyhow::Result<()> {
    use crate::release_composition::{
        PackageInput, SelectedComponentArtifact, load_wirings, wiring_store_alias,
    };
    let inputs = packages
        .iter()
        .map(|(_, _, opened)| {
            let path = wamn_schema_generator::package_manifest_path(opened.root());
            Ok(PackageInput {
                root: opened.root().to_owned(),
                manifest: PackageManifest::from_slice(
                    &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
                )
                .with_context(|| format!("parse {}", path.display()))?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let wirings = load_wirings(&inputs)?;
    let mut requirements = Vec::new();
    for component in &release.manifest.components {
        let Some((_, version, opened)) = packages
            .iter()
            .find(|(id, _, _)| *id == component.package_id)
        else {
            continue;
        };
        let template = opened
            .root()
            .join("publication/components")
            .join(format!("{}.json.in", component.component));
        if !template.is_file() {
            continue;
        }
        let declaration: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&template).with_context(|| format!("read {}", template.display()))?,
        )
        .with_context(|| format!("parse {}", template.display()))?;
        let connections: Vec<wamn_catalog::ComponentConnection> =
            serde_json::from_value(declaration["connections"].clone())
                .with_context(|| format!("read the connections of {}", template.display()))?;
        for connection in connections {
            let alias = if connection.store_alias
                == crate::component_declaration::COMPONENT_DECLARATION_STORE_ALIAS_PLACEHOLDER
            {
                let artifact = SelectedComponentArtifact {
                    package_id: component.package_id.as_str().into(),
                    package_version: version.as_str().into(),
                    component: component.component.as_str().into(),
                    path: template.clone(),
                    digest: component.digest.as_str().into(),
                };
                match wiring_store_alias(&wirings, &artifact)? {
                    Some(alias) => alias,
                    None => continue,
                }
            } else {
                connection.store_alias.clone()
            };
            requirements.push(crate::push_component::portable_requirement(
                component.digest.as_str(),
                &wamn_catalog::ComponentConnection {
                    store_alias: alias,
                    requirement_type: connection.requirement_type,
                },
            ));
        }
    }
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
