//! The package stage of `apply` (docs/plan/platform-deploy.md §10.1 step 5,
//! §10.2), run in the project database before any Kubernetes write.
//!
//! Under the lifecycle lock, idempotently: apply the pending platform
//! migrations of the project database (`upgrade-schema`), or refuse and name
//! the file; install each package the
//! environment never had from its verified artifact; verify the predecessor
//! (the installed versions are the ones analysis read); apply the stage of
//! each staged successor the release names; write the release cache row;
//! materialize the release's connection requirements from the admitted
//! descriptors the release manifest carries (contract D, R2 (3)); reconcile
//! the packages' data access; create the number sequence row of each
//! package default that the environment does not have yet. A failed stage
//! stops here, and
//! nothing was written to Kubernetes. A backfill that stops on a lock is
//! resumed by applying again.

use std::collections::BTreeMap;

use anyhow::{Context as _, bail, ensure};
use wamn_schema_generator::PackageManifest;

use super::Platform;
use super::analyse::{self, Installed, connect};
use super::document::EnvironmentDocument;
use crate::package_artifact::{OpenedPackage, PackageRegistry, PackageSource, open_package_source};

/// Run the package stage of the release `digest`, whose manifest is
/// `manifest`, on the installed packages `analysed` that analysis read.
/// Returns the manifests of its packages.
///
/// # Errors
///
/// When an artifact cannot be opened, the installed versions changed since
/// analysis, or a package application refuses.
pub async fn stage(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysed: &BTreeMap<String, Installed>,
    digest: &str,
    manifest: &wamn_catalog::ServingManifest,
    project_url: &str,
) -> anyhow::Result<Vec<PackageManifest>> {
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);

    // 0. The platform schema the packages run on: the pending project
    // migrations of this binary, in one transaction. A drifted or failing file
    // refuses here and names itself.
    crate::upgrade_schema::upgrade_schema(&schema_request(
        &platform.system_database_url,
        project_url,
    ))
    .await
    .context("apply the pending project migrations")?;

    // 1. The predecessor: the installed versions are those analysis judged.
    let observed = installed(platform, document).await?;
    let mut changed = Vec::new();
    for (package, installed) in analysed {
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
    // content). An overlay applies after the bases it extends, which own the
    // relations its migrations add definitions to.
    let mut opened = Vec::new();
    for package in &manifest.release.packages {
        let (id, version) = (package.package_id(), package.package_version());
        let package = open(platform, id, version).await?;
        let path = wamn_schema_generator::package_manifest_path(package.root());
        let package_manifest = PackageManifest::from_slice(
            &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        opened.push((id.to_owned(), version.to_owned(), package, package_manifest));
    }
    let order = apply_order(
        &opened
            .iter()
            .map(|(id, _, _, package_manifest)| {
                (
                    id.as_str(),
                    package_manifest
                        .base_dependencies
                        .values()
                        .map(|dependency| dependency.package.as_str())
                        .collect(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    let mut opened = opened.into_iter().map(Some).collect::<Vec<_>>();
    let mut packages = Vec::new();
    for index in order {
        let (id, version, opened, package_manifest) =
            opened[index].take().context("each package applies once")?;
        let applies = match analysed.get(&id) {
            None => true,
            Some(installed) => {
                installed.current != version
                    && installed.lineage.get(&version).and_then(Option::as_deref)
                        == Some(installed.current.as_str())
            }
        };
        if applies {
            crate::apply_package::apply_package(crate::apply_package::ApplyPackageRequest {
                package: opened.package_root(),
                database_url: project_url.to_owned(),
                tenant: tenant.clone(),
            })
            .await
            .with_context(|| format!("apply package {id}@{version}"))?;
        }
        packages.push((id, version, opened, package_manifest));
    }

    // 2. The release cache row, once its package versions are installed: the
    // row seals the migrations of every version it names.
    write_release(project_url, &tenant, digest, manifest).await?;
    materialize_requirements(project_url, &tenant, manifest).await?;
    materialize_application_principals(
        &platform.system_database_url,
        project_url,
        &tenant,
        manifest,
        &packages
            .iter()
            .map(|(_, _, _, manifest)| manifest.clone())
            .collect::<Vec<_>>(),
        &platform.auth_changes().await?,
    )
    .await?;
    create_number_sequences(
        project_url,
        &tenant,
        &sequence_defaults(
            manifest,
            packages.iter().map(|(_, _, _, manifest)| manifest),
        ),
    )
    .await
    .context("create the number sequences of the release's packages")?;

    // 3. The data access of the packages of the release, when each is the
    // installed version. A release on the tested predecessor keeps the
    // expanded schema's grants, which qualify-upgrade tested (§14).
    let installed = installed(platform, document).await?;
    let current = packages.iter().all(|(id, version, _, _)| {
        installed
            .get(id)
            .is_some_and(|installed| &installed.current == version)
    });
    if current && !packages.is_empty() {
        crate::reconcile_package_data_access::reconcile_package_data_access(
            crate::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
                packages: packages
                    .iter()
                    .map(|(_, _, opened, _)| opened.package_root())
                    .collect(),
                database_url: project_url.to_owned(),
                tenant: tenant.clone(),
            },
        )
        .await
        .context("reconcile the packages' data access")?;
    }
    Ok(packages
        .into_iter()
        .map(|(_, _, _, package_manifest)| package_manifest)
        .collect())
}

/// Create each scheduled or integration application principal and its declared dependency role.
/// An identical apply writes no rows.
///
/// # Errors
///
/// Returns an error if an identity is disabled or a database write fails.
pub async fn materialize_application_principals(
    system_url: &str,
    project_url: &str,
    tenant: &str,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[PackageManifest],
    changes: &wamn_platform_identity::auth_changes::AuthChanges,
) -> anyhow::Result<()> {
    use wamn_platform_identity::auth_changes::AuthDrops;
    use wamn_project_state::PlatformComponent;

    let applications = packages
        .iter()
        .filter(|package| {
            manifest
                .workflow
                .schedules
                .values()
                .any(|schedule| schedule.package_id == package.package.id)
                || crate::publish_release::composes_integration(package, &manifest.components)
        })
        .collect::<Vec<_>>();
    if applications.is_empty() {
        return Ok(());
    }
    let dependency_manifests = packages
        .iter()
        .map(|package| (package.package.id.as_str(), package))
        .collect();
    let mut system = connect(system_url).await?;
    system.batch_execute("SET ROLE wamn_system").await?;
    let domain: Option<String> = system
        .query_one("SELECT platform_domain FROM registry.meta", &[])
        .await?
        .get(0);
    let domain =
        domain.context("platform-domain-unset: application principals need the platform domain")?;
    let mut project = connect(project_url).await?;
    let transaction = project.transaction().await?;
    transaction
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        ))
        .await?;
    let mut drops = AuthDrops::default();
    for package in applications {
        let subject = wamn_platform_identity::application_subject(tenant, &package.package.id)?;
        let principal = crate::provision_project_env::resolve_or_create_service(
            &mut system,
            &subject,
            &subject,
        )
        .await?;
        ensure!(
            principal.status() == wamn_platform_identity::PrincipalStatus::Active,
            "application service principal {subject:?} is disabled"
        );
        let written = transaction
            .execute(
                "INSERT INTO platform.\"user\" (tenant_id, id, type, email, display_name) \
             VALUES ($1, $2::text::uuid, 'service', $3, $4) ON CONFLICT (tenant_id, id) DO NOTHING",
                &[
                    &tenant,
                    &principal.id().as_str(),
                    &format!("{subject}@{domain}"),
                    &subject,
                ],
            )
            .await?;
        if written != 0 {
            drops.principal(tenant, principal.id().as_str());
        }
        let role = format!("application-{}", package.package.id.replace('_', "-"));
        crate::role_permissions::create_role(&transaction, tenant, &role, &mut drops).await?;
        for permission in crate::publish_release::application_dependency_permissions(
            package,
            &dependency_manifests,
        )? {
            crate::role_permissions::grant_permission(
                &transaction,
                tenant,
                &role,
                &permission,
                &mut drops,
            )
            .await?;
        }
        wamn_platform_identity::application::grant_user_role(
            &transaction,
            tenant,
            principal.id().as_str(),
            &role,
            &mut drops,
        )
        .await?;
    }
    transaction.commit().await?;
    changes.publish(drops).await;
    Ok(())
}

/// The sequence defaults of `packages`, each under the application that
/// numbers from it: the `app.application` claim of the package's operations,
/// which is the composing application of its components in `manifest`, or
/// the package itself when the release serves none of its components.
pub fn sequence_defaults<'a>(
    manifest: &wamn_catalog::ServingManifest,
    packages: impl IntoIterator<Item = &'a PackageManifest>,
) -> Vec<wamn_platform_identity::configuration::SequenceDefault> {
    let mut defaults = Vec::new();
    for package in packages {
        let id = package.package.id.as_str();
        let application = manifest
            .components
            .iter()
            .find(|component| component.package_id == id)
            .map_or(id, |component| manifest.composing_application(component));
        defaults.extend(package.sequences.iter().map(|(name, declaration)| {
            wamn_platform_identity::configuration::SequenceDefault {
                application: application.to_owned(),
                name: name.clone(),
                pattern: declaration.pattern.clone(),
            }
        }));
    }
    defaults
}

/// Create the `platform.number_sequence` row of each default the database at
/// `project_url` does not have yet, and return how many were created. A row
/// that exists keeps its pattern, so an edited pattern survives every apply.
///
/// The write narrows the apply login to `wamn_administration`, the authority
/// of the control host's configuration writes, so the row security of the
/// table applies; it binds `wamn:provisioning` as the actor and operation.
///
/// # Errors
///
/// When the database refuses a statement.
pub async fn create_number_sequences(
    project_url: &str,
    tenant: &str,
    defaults: &[wamn_platform_identity::configuration::SequenceDefault],
) -> anyhow::Result<u64> {
    if defaults.is_empty() {
        return Ok(0);
    }
    let mut project = connect(project_url).await?;
    let transaction = project.transaction().await?;
    transaction
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_project_state::PlatformComponent::Provisioning,
        ))
        .await
        .context("bind wamn:provisioning as the actor and operation")?;
    transaction
        .batch_execute(&format!(
            "SET LOCAL ROLE \"{}\"",
            wamn_control_provision::ADMINISTRATION_ROLE
        ))
        .await
        .context("narrow the apply login to the administration authority")?;
    let created = wamn_platform_identity::configuration::create_default_sequences(
        &transaction,
        tenant,
        defaults,
    )
    .await?;
    transaction.commit().await?;
    Ok(created)
}

/// The `upgrade-schema` run of the stage on the project database at
/// `project_url`: applied, never a first run with a baseline.
pub(super) fn schema_request(
    system_database_url: &str,
    project_url: &str,
) -> crate::upgrade_schema::UpgradeSchemaRequest {
    crate::upgrade_schema::UpgradeSchemaRequest {
        system_database_url: system_database_url.to_owned(),
        admin_database_url: Some(project_url.to_owned()),
        baseline: None,
        confirm: true,
    }
}

/// The order in which the stage applies the release's `packages`, each an id
/// and the base packages it extends: every package after the release
/// packages it extends.
///
/// # Errors
///
/// When the packages extend one another in a cycle.
fn apply_order(packages: &[(&str, Vec<&str>)]) -> anyhow::Result<Vec<usize>> {
    let mut order = Vec::with_capacity(packages.len());
    while order.len() < packages.len() {
        let ready = (0..packages.len())
            .filter(|index| !order.contains(index))
            .filter(|&index| {
                packages[index].1.iter().all(|base| {
                    packages
                        .iter()
                        .position(|(id, _)| id == base)
                        .is_none_or(|base| order.contains(&base))
                })
            })
            .collect::<Vec<_>>();
        ensure!(
            !ready.is_empty(),
            "the release's packages extend one another in a cycle"
        );
        order.extend(ready);
    }
    Ok(order)
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

/// The package artifact `<id>-<version>`, pulled from the registry.
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
    manifest: &wamn_catalog::ServingManifest,
) -> anyhow::Result<()> {
    let requirements = release_requirements(manifest);
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
pub(super) fn release_requirements(
    manifest: &wamn_catalog::ServingManifest,
) -> Vec<wamn_catalog::ComponentConnectionRequirement> {
    manifest
        .components
        .iter()
        .flat_map(|component| {
            component.descriptor.connections.iter().map(|connection| {
                crate::component_admission::portable_requirement(
                    component.descriptor.component_digest.as_str(),
                    connection,
                )
            })
        })
        .collect()
}

/// Write `catalog.releases (manifest_digest, manifest)` for the release if
/// absent (R1). The row is immutable, and its bytes hash to its key. This is
/// the one writer of the row (R1 (3), R2); publish writes OCI only.
///
/// # Errors
///
/// When `manifest` does not hash to `digest`, or the write fails.
pub async fn write_release(
    project_url: &str,
    tenant: &str,
    digest: &str,
    manifest: &wamn_catalog::ServingManifest,
) -> anyhow::Result<()> {
    let bytes = manifest.canonical_bytes();
    ensure!(
        manifest.digest().as_str() == digest,
        "release manifest {digest} hashes to {}",
        manifest.digest().as_str()
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
            &[&tenant, &digest, &bytes],
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
        ComponentConnection, ComponentConnectionType, ComponentDescriptor,
        ConnectionTypeDescriptor, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION,
        ServingComponent, ServingManifest, ServingRelease, WorkflowSection,
    };

    use super::{apply_order, release_requirements, schema_request};

    /// The stage applies the pending project migrations of the binary to
    /// the project database, never as a plan and never with a baseline
    /// (wamn-flygs).
    #[test]
    fn the_stage_applies_the_pending_project_migrations() {
        let request = schema_request(
            "postgres://postgres@db/wamn_system",
            "postgres://postgres@db/wamn_dkk_receiving_dev_x",
        );
        assert_eq!(
            request.system_database_url,
            "postgres://postgres@db/wamn_system"
        );
        assert_eq!(
            request.admin_database_url.as_deref(),
            Some("postgres://postgres@db/wamn_dkk_receiving_dev_x")
        );
        assert!(request.confirm);
        assert_eq!(request.baseline, None);
    }

    /// An overlay applies after the base it extends, whatever the manifest
    /// order (wamn-k8127.5).
    #[test]
    fn an_overlay_applies_after_its_base() {
        let packages = [
            ("client_acme_receiving", vec!["wamn_receiving"]),
            ("wamn_receiving", Vec::new()),
            ("other", vec!["outside_the_release"]),
        ];
        assert_eq!(apply_order(&packages).unwrap(), [1, 2, 0]);
        assert!(apply_order(&[("a", vec!["b"]), ("b", vec!["a"])]).is_err());
    }

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
            runtime_schema: None,
            statement_instances: std::collections::BTreeMap::new(),
            package_id: "orders".to_owned(),
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
            composed_entry: None,
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
                schedules: BTreeMap::new(),
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
