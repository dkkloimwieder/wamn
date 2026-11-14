//! Application baseline initialization and immutable release caching.

use sqlx::{Connection as _, Row as _};
pub(super) mod plan;
pub use plan::{apply_order, sequence_defaults};

use std::collections::BTreeMap;

use anyhow::{Context as _, bail, ensure};
use wamn_compiler::manifest::PackageManifest;

use super::Platform;
use super::analyse::{self, Installed};
use super::document::EnvironmentDocument;
use wamn_package_control::package_artifact::{OpenedPackage, PackageSource, open_package_source};
use wamn_postgres::connect;

/// Initialize absent package baselines in dependency order, without selecting a release.
/// Existing baselines keep their SQL records even when the selected version changes.
pub async fn initialize(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysed: &BTreeMap<String, Installed>,
    manifest: &wamn_catalog::ServingManifest,
    project_url: &str,
) -> anyhow::Result<Vec<PackageManifest>> {
    let project_env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);

    let mut opened = Vec::new();
    for package in &manifest.release.packages {
        let (id, version) = (package.package_id(), package.package_version());
        let package = open(platform, id, version).await?;
        let path = wamn_compiler::package_manifest_path(package.root());
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
            crate::apply_package::apply_package(crate::apply_package::ApplyPackageRequest {
                migration_database_url: wamn_control_provision::migration::configuration(
                    project_url,
                    &document.org,
                    &document.project,
                    &document.env,
                )?,
                component_source: platform.component_source()?,
                changes: platform.auth_changes().await?,
                package: opened.package_root(),
                database_url: project_url.to_owned(),
                org: document.org.clone(),
                project: document.project.clone(),
                environment: document.env.clone(),
            })
            .await
            .with_context(|| format!("apply package {id}@{version}"))?;
        }
        packages.push((id, version, opened, package_manifest));
    }

    materialize_application_principals(
        &platform.system_database_url,
        project_url,
        &project_env_key,
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

    Ok(packages
        .into_iter()
        .map(|(_, _, _, package_manifest)| package_manifest)
        .collect())
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
    project_env_key: &str,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[PackageManifest],
    changes: &wamn_broker::Publisher,
) -> anyhow::Result<()> {
    use wamn_broker::auth_changes::AuthDrops;
    use wamn_identity_protocol::PlatformComponent;

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
    let domain: Option<String> =
        sqlx::query(wamn_registry::sql::IDENTITY_PLATFORM_SRC_APPLICATION_PROJECT_AUTHORITY)
            .fetch_one(&mut system)
            .await?
            .get(0);
    let domain =
        domain.context("platform-domain-unset: application principals need the platform domain")?;
    let mut project = super::analyse::connect_project(project_url).await?;
    let mut transaction = project.begin().await?;
    sqlx::query(wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL)
        .bind(project_env_key)
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(PlatformComponent::Provisioning),
    ))
    .execute(&mut *transaction)
    .await
    .map(drop)?;
    let mut drops = AuthDrops::default();
    for package in applications {
        let subject =
            wamn_platform_identity::application_subject(project_env_key, &package.package.id)?;
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
        let written = sqlx::query(
            "INSERT INTO platform.\"user\" (id, type, email, display_name) \
             VALUES ($1::text::uuid, 'service', $2, $3) ON CONFLICT (id) DO NOTHING",
        )
        .bind(principal.id().to_string().as_str())
        .bind(format!("{subject}@{domain}"))
        .bind(&subject)
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected())?;
        if written != 0 {
            drops.principal(project_env_key, principal.id().to_string().as_str());
        }
        let role = format!("application-{}", package.package.id.replace('_', "-"));
        wamn_platform_identity::application::create_role(
            &mut transaction,
            project_env_key,
            &role,
            &mut drops,
        )
        .await?;
    }
    write_runtime_grants(
        &mut system,
        &mut transaction,
        project_env_key,
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
    system: &mut sqlx::PgConnection,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[PackageManifest],
    drops: &mut wamn_broker::auth_changes::AuthDrops,
) -> anyhow::Result<()> {
    let dependency_manifests = packages
        .iter()
        .map(|package| (package.package.id.as_str(), package))
        .collect();
    for package in packages.iter().filter(|package| {
        wamn_release_control::publish_release::composes_integration(package, &manifest.components)
    }) {
        let subject =
            wamn_platform_identity::application_subject(project_env_key, &package.package.id)?;
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
        let active: bool = sqlx::query("SELECT EXISTS (SELECT FROM platform.\"user\" WHERE id=$1::text::uuid AND type='service' AND status='active')").bind(principal.id().to_string().as_str()).fetch_one(&mut **transaction).await?.get(0);
        ensure!(
            active,
            "application principal {subject} is absent or inactive in the project; run env initialize"
        );
        let role = format!("application-{}", package.package.id.replace('_', "-"));
        let owned: bool = sqlx::query("SELECT EXISTS (SELECT FROM platform.role WHERE name=$1 AND created_by=$3::text::uuid) \
             AND NOT EXISTS (SELECT FROM platform.user_role WHERE role_name=$1 AND user_id<>$2::text::uuid)").bind(&role).bind(principal.id().to_string().as_str()).bind(wamn_identity_protocol::PlatformComponent::Provisioning.principal_id().to_string()).fetch_one(&mut **transaction).await?.get(0);
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
                project_env_key,
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
            project_env_key,
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
    let mut transaction = project.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(
            wamn_identity_protocol::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *transaction)
    .await
    .map(drop)
    .context("bind wamn:provisioning as the actor and operation")?;
    let created =
        wamn_platform_identity::configuration::create_default_sequences(&mut transaction, defaults)
            .await?;
    transaction.commit().await?;
    Ok(created)
}

/// The installed packages of the project database, read now.
pub(super) async fn installed(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<BTreeMap<String, Installed>> {
    let mut system = connect(&platform.system_database_url).await?;
    let row = analyse::read_row(&mut system, document)
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
    project_env_key: &str,
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
    let mut transaction = client
        .begin()
        .await
        .context("begin the release cache write")?;
    sqlx::query(
        "SELECT set_config('app.user_id', $1, true), \
             set_config('app.operation', 'wamn:provisioning', true)",
    )
    .bind(actor)
    .fetch_one(&mut *transaction)
    .await
    .context("bind the release publisher and provisioning operation")?;
    sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
        .bind(project_env_key)
        .fetch_one(&mut *transaction)
        .await
        .context("bind the project identity")?;
    sqlx::query(wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL)
        .bind(project_env_key)
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(project_env_key)
    .bind(digest)
    .bind(&bytes)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .context("write the release cache row")?;
    transaction
        .commit()
        .await
        .context("commit the release cache row")
}

#[cfg(test)]
mod tests {
    use sqlx::Row as _;
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{
        ComponentConnection, ComponentConnectionType, ComponentDescriptor, PackageCoordinate,
        SERVING_MANIFEST_FORMAT_VERSION, ServingComponent, ServingManifest, ServingRelease,
    };

    use super::apply_order;

    #[tokio::test]
    async fn release_writer_stamps_publisher_and_keeps_first_stamp_on_retry() {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_platform_identity::test_database::project_platform();
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
        let mut client = super::super::analyse::connect_project(database.url())
            .await
            .unwrap();
        let mut stamps = async || {
            let row = sqlx::query(
                "SELECT created_by, updated_by, created_at::text, updated_at::text \
                 FROM catalog.releases WHERE project_env_key=$1 AND manifest_digest=$2",
            )
            .bind(&"history")
            .bind(&digest.as_str())
            .fetch_one(&mut client)
            .await
            .unwrap();
            (
                row.get::<uuid::Uuid, _>(0),
                row.get::<uuid::Uuid, _>(1),
                row.get::<String, _>(2),
                row.get::<String, _>(3),
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
            descriptor,
        }
    }
}
