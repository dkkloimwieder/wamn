//! Application baseline initialization and immutable release caching.

pub(super) mod plan;
pub use plan::{apply_order, sequence_defaults};

use std::collections::BTreeMap;

use anyhow::{Context as _, bail, ensure};
use wamn_catalog::manifest::PackageManifest;

use super::Platform;
use super::analyse::{self, Installed};
use super::document::EnvironmentDocument;
use wamn_package_control::database::connect;
use wamn_package_control::package_artifact::{OpenedPackage, PackageSource, open_package_source};

/// Initialize absent package baselines in dependency order, without selecting a release.
/// Existing baselines keep their SQL records even when the selected version changes.
pub async fn initialize(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysed: &BTreeMap<String, Installed>,
    manifest: &wamn_catalog::ServingManifest,
    project_url: &str,
) -> anyhow::Result<Vec<PackageManifest>> {
    let env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);

    // Refuse a changed SQL record, independently of executable versions.
    let observed = installed(platform, document).await?;
    let mut changed = Vec::new();
    for (package, installed) in analysed {
        let now = observed
            .get(package)
            .map(|now| now.physical_schema_fingerprint.as_str());
        if now != Some(installed.physical_schema_fingerprint.as_str()) {
            changed.push(format!(
                "{package}: analysed {}, installed {}",
                installed.physical_schema_fingerprint,
                now.unwrap_or("nothing")
            ));
        }
    }
    ensure!(
        changed.is_empty(),
        "the installed packages changed since analysis: {}",
        changed.join("; ")
    );

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
        let applies = !analysed.contains_key(&id);
        if applies {
            wamn_package_control::apply_package::apply_package(
                wamn_package_control::apply_package::ApplyPackageRequest {
                    component_source: platform.component_source()?,
                    changes: platform.auth_changes().await?,
                    package: opened.package_root(),
                    database_url: project_url.to_owned(),
                    org: document.org.clone(),
                    project: document.project.clone(),
                    environment: document.env.clone(),
                },
            )
            .await
            .with_context(|| format!("apply package {id}@{version}"))?;
        }
        packages.push((id, version, opened, package_manifest));
    }

    materialize_application_principals(
        &platform.system_database_url,
        project_url,
        &env_key,
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
        &sequence_defaults(
            &document.membership(),
            packages.iter().map(|(_, _, _, manifest)| manifest),
        )?,
    )
    .await
    .context("create the number sequences of the release's packages")?;

    reconcile_installed_sql_grants(platform, document, project_url).await?;
    Ok(packages
        .into_iter()
        .map(|(_, _, _, package_manifest)| package_manifest)
        .collect())
}

/// Converge shared SQL grants from the complete installed schema sources.
pub(super) async fn reconcile_installed_sql_grants(
    platform: &Platform,
    document: &EnvironmentDocument,
    project_url: &str,
) -> anyhow::Result<()> {
    // Shared SQL grants require the complete installed set, including other applications.
    let installed = installed(platform, document).await?;
    let mut grant_packages = Vec::new();
    for (id, baseline) in &installed {
        grant_packages.push(open(platform, id, &baseline.current).await?);
    }
    if !grant_packages.is_empty() {
        crate::reconcile_package_data_access::reconcile_package_data_access(
            crate::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
                packages: grant_packages
                    .iter()
                    .map(OpenedPackage::package_root)
                    .collect(),
                database_url: project_url.to_owned(),
                org: document.org.clone(),
                project: document.project.clone(),
                environment: document.env.clone(),
            },
        )
        .await
        .context("reconcile installed SQL grants")?;
    }
    Ok(())
}

/// Create each integration application principal and its declared dependency role.
/// An identical apply writes no rows.
///
/// # Errors
///
/// Returns an error if an identity is disabled or a database write fails.
pub async fn materialize_application_principals(
    system_url: &str,
    project_url: &str,
    env_key: &str,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[PackageManifest],
    changes: &wamn_platform_identity::auth_changes::AuthChanges,
) -> anyhow::Result<()> {
    use wamn_identity_protocol::PlatformComponent;
    use wamn_platform_identity::auth_changes::AuthDrops;

    let applications = packages
        .iter()
        .filter(|package| {
            wamn_release_control::publish_release::composes_integration(
                package,
                &manifest.components,
            )
        })
        .collect::<Vec<_>>();
    if applications.is_empty() {
        return Ok(());
    }
    let mut system = connect(system_url).await?;
    let domain: Option<String> = system
        .query_one("SELECT platform_domain FROM registry.meta", &[])
        .await?
        .get(0);
    let domain =
        domain.context("platform-domain-unset: application principals need the platform domain")?;
    let mut project = super::analyse::connect_project(project_url).await?;
    let transaction = project.transaction().await?;
    transaction
        .query_one(
            wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL,
            &[&env_key],
        )
        .await?;
    transaction
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        ))
        .await?;
    let mut drops = AuthDrops::default();
    for package in applications {
        let subject = wamn_platform_identity::application_subject(env_key, &package.package.id)?;
        let principal = crate::provision_project_env::resolve_or_create_service(
            &mut system,
            &subject,
            &subject,
            changes,
        )
        .await?;
        ensure!(
            principal.status() == wamn_platform_identity::PrincipalStatus::Active,
            "application service principal {subject:?} is disabled"
        );
        let written = transaction
            .execute(
                "INSERT INTO platform.\"user\" (id, type, email, display_name) \
             VALUES ($1::text::uuid, 'service', $2, $3) ON CONFLICT (id) DO NOTHING",
                &[
                    &principal.id().to_string().as_str(),
                    &format!("{subject}@{domain}"),
                    &subject,
                ],
            )
            .await?;
        if written != 0 {
            drops.principal(env_key, principal.id().to_string().as_str());
        }
        let role = format!("application-{}", package.package.id.replace('_', "-"));
        wamn_platform_identity::application::create_role(&transaction, env_key, &role, &mut drops)
            .await?;
    }
    write_runtime_grants(
        &system,
        &transaction,
        env_key,
        manifest,
        packages,
        &mut drops,
    )
    .await?;
    transaction.commit().await?;
    changes.publish(drops).await;
    Ok(())
}

/// Add the selected application's permissions while retaining grants needed by running code.
/// Principals and generated roles must already exist. Shared or authored roles refuse.
pub async fn write_runtime_grants(
    system: &tokio_postgres::Client,
    transaction: &tokio_postgres::Transaction<'_>,
    env_key: &str,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[PackageManifest],
    drops: &mut wamn_platform_identity::auth_changes::AuthDrops,
) -> anyhow::Result<()> {
    let dependency_manifests = packages
        .iter()
        .map(|package| (package.package.id.as_str(), package))
        .collect();
    for package in packages.iter().filter(|package| {
        wamn_release_control::publish_release::composes_integration(package, &manifest.components)
    }) {
        let subject = wamn_platform_identity::application_subject(env_key, &package.package.id)?;
        let principal = wamn_platform_identity::resolve_subject(
            system,
            wamn_platform_identity::PrincipalType::Service,
            &subject,
        )
        .await?
        .with_context(|| {
            format!("application principal {subject} is missing; run env initialize")
        })?;
        ensure!(
            principal.status() == wamn_platform_identity::PrincipalStatus::Active,
            "application principal {subject} is disabled"
        );
        let active: bool = transaction.query_one(
            "SELECT EXISTS (SELECT FROM platform.\"user\" WHERE id=$1::text::uuid AND type='service' AND status='active')",
            &[&principal.id().to_string().as_str()],
        ).await?.get(0);
        ensure!(
            active,
            "application principal {subject} is absent or inactive in the project; run env initialize"
        );
        let role = format!("application-{}", package.package.id.replace('_', "-"));
        let owned: bool = transaction.query_one(
            "SELECT EXISTS (SELECT FROM platform.role WHERE name=$1 AND created_by=$3::text::uuid) \
             AND NOT EXISTS (SELECT FROM platform.user_role WHERE role_name=$1 AND user_id<>$2::text::uuid)",
            &[&role, &principal.id().to_string().as_str(), &wamn_identity_protocol::PlatformComponent::Provisioning.principal_id().to_string()],
        ).await?.get(0);
        ensure!(
            owned,
            "application role {role} is missing, authored, or shared; run env initialize or resolve its ownership"
        );
        for permission in wamn_release_control::publish_release::application_dependency_permissions(
            package,
            &dependency_manifests,
        )? {
            wamn_platform_identity::application::mutate_grant(
                transaction,
                env_key,
                wamn_platform_identity::application::GrantAuthority::Provisioning,
                wamn_platform_identity::application::GrantChange::Permission {
                    role: &role,
                    reference: &permission,
                    grant: true,
                },
                drops,
            )
            .await?;
        }
        wamn_platform_identity::application::mutate_grant(
            transaction,
            env_key,
            wamn_platform_identity::application::GrantAuthority::Provisioning,
            wamn_platform_identity::application::GrantChange::UserRole {
                user_id: principal.id().to_string().as_str(),
                role: &role,
                grant: true,
            },
            drops,
        )
        .await?;
    }
    Ok(())
}

/// Retire only releases observed for the target before replacement, after readiness and drain.
pub async fn retire(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &super::analyse::Analysis,
) -> anyhow::Result<()> {
    use std::collections::BTreeSet;
    let application = &analysis.application;
    let selection = &document.applications[application];
    let mut candidates: BTreeSet<String> = analysis.authorities.live.keys().cloned().collect();
    if let Some(previous) = &selection.previous {
        candidates.insert(previous.clone());
    }
    if let Some(digest) = analysis
        .authorities
        .revision
        .as_ref()
        .and_then(super::analyse::Revision::manifest_digest)
    {
        candidates.insert(digest.to_owned());
    }
    if let Some(release) = &analysis.authorities.release {
        candidates.remove(&release.digest);
    }
    if candidates.is_empty() {
        return Ok(());
    }
    let env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    let url = super::ensure::project_url(platform, &document.triple()).await?;
    let mut project = super::analyse::connect_project(&url).await?;
    let cached = project
        .query(
            "SELECT manifest_digest, canonical_bytes FROM catalog.releases WHERE env_key=$1",
            &[&env_key],
        )
        .await?;
    let mut releases = BTreeMap::new();
    for row in &cached {
        let digest: String = row.get(0);
        let (manifest, actual) = wamn_catalog::ServingManifest::from_canonical_bytes(row.get(1))?;
        ensure!(actual.as_str() == digest, "release cache digest mismatch");
        if !manifest.release.packages.is_empty()
            && manifest
                .release
                .packages
                .iter()
                .all(|package| selection.packages.contains(package.package_id()))
        {
            let (packages, _) = analyse::read_release_packages(platform, &manifest).await?;
            releases.insert(digest, (manifest, packages));
        }
    }
    let tx = project.transaction().await?;
    tx.query_one(
        wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL,
        &[&env_key],
    )
    .await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_identity_protocol::PlatformComponent::Provisioning,
    ))
    .await?;
    tx.query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
        .await?;
    // A competing preparation must be included before any permission is removed.
    let now = tx
        .query(
            "SELECT manifest_digest FROM catalog.releases WHERE env_key=$1",
            &[&env_key],
        )
        .await?;
    let before: BTreeSet<String> = cached.iter().map(|row| row.get(0)).collect();
    let now: BTreeSet<String> = now.iter().map(|row| row.get(0)).collect();
    ensure!(
        before == now,
        "release preparations changed during retirement; retain old facts and retry"
    );
    let live = analyse::read_live_set(platform, &analysis.authorities.release_name).await?;
    let target = analysis
        .authorities
        .release
        .as_ref()
        .map(|release| release.digest.as_str());
    ensure!(
        super::drain::settled(&live, target),
        "application hosts changed during retirement"
    );
    let mut connections = wamn_platform_identity::connection_changes::ConnectionDrops::default();
    let retired = super::bindings::contract(
        &tx,
        &env_key,
        application,
        &document.membership(),
        &candidates.iter().map(String::as_str).collect::<Vec<_>>(),
        &live.keys().map(String::as_str).collect::<Vec<_>>(),
        &mut connections,
    )
    .await?;
    // Cache retention never selects a release. Keep requirements only for the selection,
    // live hosts, unfinished pins and preparations that still own bindings.
    let mut needed: BTreeSet<String> = tx.query(
        "SELECT manifest_digest FROM catalog.release_connection WHERE env_key=$1 \
         UNION SELECT manifest_digest FROM catalog.connection_binding WHERE env_key=$1 \
         UNION SELECT manifest_digest FROM wamn_run.runs WHERE env_key=$1 AND manifest_digest IS NOT NULL \
         AND status NOT IN ('completed','failed','infrastructure-failure')",
        &[&env_key],
    ).await?.into_iter().map(|row| row.get(0)).collect();
    needed.extend(live.into_keys());
    if let Some(target) = target {
        needed.insert(target.to_owned());
    }
    releases.retain(|digest, _| retired.contains(digest) || needed.contains(digest));
    let mut auth = wamn_platform_identity::auth_changes::AuthDrops::default();
    let system = connect(&platform.system_database_url).await?;
    retire_runtime_grants(&system, &tx, &env_key, &releases, &retired, &mut auth).await?;
    tx.commit().await?;
    let changes = platform.auth_changes().await?;
    changes.publish(auth).await;
    wamn_platform_identity::connection_changes::ConnectionChanges::from(&changes)
        .publish(connections)
        .await;
    Ok(())
}

/// Remove generated grants exclusive to retired releases from exclusively owned roles.
/// Package manifests come from verified artifacts. Uncertain ownership refuses the transaction.
pub async fn retire_runtime_grants(
    system: &tokio_postgres::Client,
    tx: &tokio_postgres::Transaction<'_>,
    env_key: &str,
    releases: &BTreeMap<String, (wamn_catalog::ServingManifest, Vec<PackageManifest>)>,
    retired: &[String],
    drops: &mut wamn_platform_identity::auth_changes::AuthDrops,
) -> anyhow::Result<()> {
    use std::collections::BTreeSet;
    let mut removed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut retained: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (digest, (manifest, packages)) in releases {
        let dependencies = packages
            .iter()
            .map(|package| (package.package.id.as_str(), package))
            .collect();
        for package in packages.iter().filter(|package| {
            wamn_release_control::publish_release::composes_integration(
                package,
                &manifest.components,
            )
        }) {
            let requirements = if retired.contains(digest) {
                &mut removed
            } else {
                &mut retained
            };
            requirements
                .entry(package.package.id.clone())
                .or_default()
                .extend(
                    wamn_release_control::publish_release::application_dependency_permissions(
                        package,
                        &dependencies,
                    )?,
                );
        }
    }
    let actor = wamn_identity_protocol::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    for (package, permissions) in removed {
        let role = format!("application-{}", package.replace('_', "-"));
        let subject = wamn_platform_identity::application_subject(env_key, &package)?;
        let principal = wamn_platform_identity::resolve_subject(
            system,
            wamn_platform_identity::PrincipalType::Service,
            &subject,
        )
        .await?
        .context("application principal ownership is unknown; retain old facts")?;
        let owned: bool = tx.query_one(
            "SELECT EXISTS (SELECT FROM platform.role WHERE name=$1 AND created_by=$2::text::uuid) \
             AND NOT EXISTS (SELECT FROM platform.user_role WHERE role_name=$1 AND user_id<>$3::text::uuid)",
            &[&role, &actor, &principal.id().to_string().as_str()],
        ).await?.get(0);
        ensure!(
            owned,
            "application role {role} has uncertain ownership; retain old facts"
        );
        for permission in permissions {
            if retained
                .get(&package)
                .is_some_and(|requirements| requirements.contains(&permission))
            {
                continue;
            }
            let generated: bool = tx.query_one(
                "SELECT EXISTS (SELECT FROM platform.permission WHERE role_name=$1 AND permission=$2 AND created_by=$3::text::uuid AND updated_by=$3::text::uuid)",
                &[&role, &permission, &actor],
            ).await?.get(0);
            if generated {
                wamn_platform_identity::application::mutate_grant(
                    tx,
                    env_key,
                    wamn_platform_identity::application::GrantAuthority::Provisioning,
                    wamn_platform_identity::application::GrantChange::Permission {
                        role: &role,
                        reference: &permission,
                        grant: false,
                    },
                    drops,
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Create the `platform.number_sequence` row of each default the database at
/// `project_url` does not have yet, and return how many were created. A row
/// that exists keeps its pattern, so an edited pattern survives every apply.
///
/// The project owner writes the defaults and binds `wamn:provisioning`
/// as the actor and operation.
///
/// # Errors
///
/// When the database refuses a statement.
pub async fn create_number_sequences(
    project_url: &str,
    defaults: &[wamn_platform_identity::configuration::SequenceDefault],
) -> anyhow::Result<u64> {
    if defaults.is_empty() {
        return Ok(0);
    }
    let mut project = super::analyse::connect_project(project_url).await?;
    let transaction = project.transaction().await?;
    transaction
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_identity_protocol::PlatformComponent::Provisioning,
        ))
        .await
        .context("bind wamn:provisioning as the actor and operation")?;
    let created =
        wamn_platform_identity::configuration::create_default_sequences(&transaction, defaults)
            .await?;
    transaction.commit().await?;
    Ok(created)
}

/// The installed packages of the project database, read now.
pub(super) async fn installed(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<BTreeMap<String, Installed>> {
    let system = connect(&platform.system_database_url).await?;
    let row = analyse::read_row(&system, document)
        .await?
        .context("the environment row is missing after step 4")?;
    Ok(analyse::read_project(
        &analyse::project_url(platform, document, &row)?,
        &wamn_control_provision::project_env_key(&document.org, &document.project, &document.env),
    )
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
        registry: platform.registry(base).await?,
    })
    .await
    .with_context(|| format!("open package artifact {id}@{version}"))
}

/// Write `catalog.releases (manifest_digest, manifest)` for the release if
/// absent (R1). The row is immutable, and its bytes hash to its key. This is
/// the one writer of the row (R1 (3), R2); publish writes OCI only.
/// The caller supplies the authenticated publisher, or the provisioning
/// machine principal on the trusted direct path. Retries keep the first stamp.
///
/// # Errors
///
/// When `manifest` does not hash to `digest`, or the write fails.
pub async fn write_release(
    project_url: &str,
    env_key: &str,
    digest: &str,
    manifest: &wamn_catalog::ServingManifest,
    actor: &str,
) -> anyhow::Result<()> {
    let bytes = manifest.canonical_bytes();
    ensure!(
        manifest.digest().as_str() == digest,
        "release manifest {digest} hashes to {}",
        manifest.digest().as_str()
    );
    let mut client = super::analyse::connect_project(project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the release cache write")?;
    transaction
        .query_one(
            "SELECT set_config('app.user_id', $1, true), \
             set_config('app.operation', 'wamn:provisioning', true)",
            &[&actor],
        )
        .await
        .context("bind the release publisher and provisioning operation")?;
    transaction
        .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
        .await
        .context("claim the tenant")?;
    transaction
        .query_one(
            wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL,
            &[&env_key],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            &[&env_key, &digest, &bytes],
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
        ComponentConnection, ComponentConnectionType, ComponentDescriptor, PackageCoordinate,
        SERVING_MANIFEST_FORMAT_VERSION, ServingComponent, ServingManifest, ServingRelease,
    };

    use super::apply_order;

    #[tokio::test]
    async fn release_writer_stamps_publisher_and_keeps_first_stamp_on_retry() {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_platform_identity::test_database::tenant_platform();
        database
            .execute(&[&format!(
                "BEGIN; {} COMMIT;",
                wamn_control_provision::platform_principals_sql("example.invalid").unwrap()
            )])
            .unwrap();
        let manifest = ServingManifest {
            composed_entry: None,
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("orders", "1.0.0").unwrap()]),
            },
            components: BTreeSet::from([component(
                "blob-put",
                &format!("sha256:{}", "a".repeat(64)),
                &["store"],
            )]),
            routes: BTreeSet::new(),
            attachments: BTreeMap::new(),
            host_routes: BTreeSet::new(),
        };
        let digest = manifest.digest();
        let publisher = uuid::Uuid::new_v4();
        super::write_release(
            database.url(),
            "history",
            digest.as_str(),
            &manifest,
            &publisher.to_string(),
        )
        .await
        .unwrap();
        let client = super::super::analyse::connect_project(database.url())
            .await
            .unwrap();
        let stamps = async || {
            let row = client
                .query_one(
                    "SELECT created_by, updated_by, created_at::text, updated_at::text \
                 FROM catalog.releases WHERE env_key=$1 AND manifest_digest=$2",
                    &[&"history", &digest.as_str()],
                )
                .await
                .unwrap();
            (
                row.get::<_, uuid::Uuid>(0),
                row.get::<_, uuid::Uuid>(1),
                row.get::<_, String>(2),
                row.get::<_, String>(3),
            )
        };
        let before = stamps().await;
        assert_eq!((before.0, before.1), (publisher, publisher));
        assert_eq!(before.2, before.3);
        super::write_release(
            database.url(),
            "history",
            digest.as_str(),
            &manifest,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
        assert_eq!(
            stamps().await,
            before,
            "a second publisher cannot replace the first insertion stamp"
        );
    }

    #[test]
    fn sequence_defaults_use_map_names_and_refuse_conflicting_declarations() {
        let mut base = wamn_package_control::test_support::source_manifest();
        base.package.id = "base".into();
        base.sequences = serde_json::from_value(serde_json::json!({
            "order": {"pattern": "PO-{seq:po}"}
        }))
        .unwrap();
        let mut overlay = base.clone();
        overlay.package.id = "overlay".into();
        let applications = serde_json::from_value(serde_json::json!({
            "receiving": {"packages": ["base", "overlay"]}
        }))
        .unwrap();
        let defaults = super::sequence_defaults(&applications, [&base, &overlay]).unwrap();
        assert_eq!(
            defaults,
            [wamn_platform_identity::configuration::SequenceDefault {
                application: "receiving".into(),
                name: "order".into(),
                pattern: "PO-{seq:po}".into(),
            }]
        );
        overlay.sequences.get_mut("order").unwrap().pattern = "ACME-{seq:po}".into();
        assert!(super::sequence_defaults(&applications, [&base, &overlay]).is_err());
        assert!(super::sequence_defaults(&BTreeMap::new(), [&base]).is_err());
        let separate = serde_json::from_value(serde_json::json!({
            "receiving": {"packages": ["base"]}, "orders": {"packages": ["overlay"]}
        }))
        .unwrap();
        assert_eq!(
            super::sequence_defaults(&separate, [&base, &overlay])
                .unwrap()
                .len(),
            separate.len()
        );
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
        let order = apply_order(&packages).unwrap();
        assert_eq!(
            order.iter().copied().collect::<BTreeSet<_>>(),
            (0..packages.len()).collect()
        );
        for (index, (_, dependencies)) in packages.iter().enumerate() {
            for dependency in dependencies {
                if let Some(base) = packages.iter().position(|(id, _)| id == dependency) {
                    assert!(
                        order.iter().position(|item| *item == base)
                            < order.iter().position(|item| *item == index)
                    );
                }
            }
        }
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
}
