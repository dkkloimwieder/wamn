//! Application baseline initialization and immutable release caching.

use sqlx::{Connection as _, Row as _};
pub(super) mod plan;
pub use plan::{apply_order, sequence_defaults};

use std::collections::BTreeMap;

use anyhow::{Context as _, bail, ensure};
use wamn_catalog::manifest::PackageManifest;

use super::Platform;
use super::analyse::{self, Installed};
use super::document::EnvironmentDocument;
use wamn_postgres::connect;
use wamn_publish::package_artifact::{OpenedPackage, PackageSource, open_package_source};

/// Create each integration application principal and its declared dependency role.
/// An identical apply writes no rows.
///
/// # Errors
///
/// Returns an error if an identity is disabled or a database write fails.
pub async fn materialize_application_principals(
    system_url: &str,
    project_url: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[PackageManifest],
) -> anyhow::Result<()> {
    use wamn_identity_protocol::PlatformComponent;

    let applications = packages
        .iter()
        .filter(|package| {
            wamn_publish::publish_release::composes_integration(package, &manifest.components)
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
    sqlx::query(wamn_provision::operation_grants::OPERATION_GRANT_LOCK_SQL)
        .bind(project_env_key)
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::bind_platform_principal_sql(PlatformComponent::Provisioning),
    ))
    .execute(&mut *transaction)
    .await
    .map(drop)?;
    for package in applications {
        let subject =
            wamn_identity::application_subject(project_env_key, &package.package.id)?;
        let principal = crate::provision_project_env::resolve_or_create_service(
            &mut system,
            &subject,
            &subject,
        )
        .await?;
        ensure!(
            principal.status() == wamn_identity::PrincipalStatus::Active,
            "application service principal {subject:?} is disabled"
        );
        sqlx::query(
            "INSERT INTO platform.\"user\" (id, type, email, display_name) \
             VALUES ($1, 'service', $2, $3) ON CONFLICT (id) DO NOTHING",
        )
        .bind(principal.id())
        .bind(format!("{subject}@{domain}"))
        .bind(&subject)
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected())?;
        let role = format!(
            "application-{}",
            package.package.id.as_str().replace('_', "-")
        );
        crate::application::create_role(&mut transaction, &role).await?;
    }
    write_runtime_grants(
        &mut system,
        &mut transaction,
        project_env_key,
        manifest,
        packages,
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// Add the selected application's permissions while retaining grants needed by running code.
/// Principals and generated roles must already exist. Shared or authored roles refuse.
pub async fn write_runtime_grants(
    system: &mut sqlx::PgConnection,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &wamn_registry::ProjectEnvKey,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[PackageManifest],
) -> anyhow::Result<()> {
    let mut system = crate::provision_project_env::provisioning_transaction(system).await?;
    let dependency_manifests = packages
        .iter()
        .map(|package| (&package.package.id, package))
        .collect();
    for package in packages.iter().filter(|package| {
        wamn_publish::publish_release::composes_integration(package, &manifest.components)
    }) {
        let subject =
            wamn_identity::application_subject(project_env_key, &package.package.id)?;
        let principal = wamn_identity::resolve_subject(
            &mut system,
            wamn_identity::PrincipalType::Service,
            &subject,
        )
        .await?
        .with_context(|| {
            format!("application principal {subject} is missing; run env initialize")
        })?;
        ensure!(
            principal.status() == wamn_identity::PrincipalStatus::Active,
            "application principal {subject} is disabled"
        );
        let active: bool = sqlx::query("SELECT EXISTS (SELECT FROM platform.\"user\" WHERE id=$1 AND type='service' AND status='active')").bind(principal.id()).fetch_one(&mut **transaction).await?.get(0);
        ensure!(
            active,
            "application principal {subject} is absent or inactive in the project; run env initialize"
        );
        let role = format!(
            "application-{}",
            package.package.id.as_str().replace('_', "-")
        );
        let owned: bool = sqlx::query(
            "SELECT EXISTS (SELECT FROM platform.role WHERE name=$1 AND created_by=$3) \
             AND NOT EXISTS (SELECT FROM platform.user_role WHERE role_name=$1 AND user_id<>$2)",
        )
        .bind(&role)
        .bind(principal.id())
        .bind(wamn_identity_protocol::PlatformComponent::Provisioning.principal_id())
        .fetch_one(&mut **transaction)
        .await?
        .get(0);
        ensure!(
            owned,
            "application role {role} is missing, authored, or shared; run env initialize or resolve its ownership"
        );
        for permission in wamn_publish::publish_release::application_dependency_permissions(
            package,
            &dependency_manifests,
        )? {
            crate::application::mutate_grant(
                transaction,
                project_env_key,
                crate::application::GrantAuthority::Provisioning,
                crate::application::GrantChange::Permission {
                    role: &role,
                    reference: &permission,
                    grant: true,
                },
            )
            .await?;
        }
        crate::application::mutate_grant(
            transaction,
            project_env_key,
            crate::application::GrantAuthority::Provisioning,
            crate::application::GrantChange::UserRole {
                user_id: principal.id(),
                role: &role,
                grant: true,
            },
        )
        .await?;
    }
    system.commit().await?;
    Ok(())
}

/// The installed packages of the project database, read now.
pub(super) async fn installed(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<BTreeMap<wamn_catalog::PackageName, Installed>> {
    let mut system = connect(&platform.system_database_url).await?;
    let mut system = crate::provision_project_env::provisioning_transaction(&mut system).await?;
    let row = analyse::read_row(&mut system, document)
        .await?
        .context("the environment row is missing after step 4")?;
    system.commit().await?;
    Ok(analyse::read_project(
        &analyse::project_url(platform, document, &row).await?,
    )
    .await?
    .context("the project database is missing after step 4")?
    .installed)
}

/// The package artifact `<id>-<version>`, pulled from the registry.
pub(super) async fn open(
    platform: &Platform,
    id: &wamn_catalog::PackageName,
    version: &wamn_catalog::package_version::PackageVersion,
) -> anyhow::Result<OpenedPackage> {
    let Some(base) = &platform.package_artifact_base else {
        bail!(
            "release names package {id}@{version}; set WAMN_PACKAGE_ARTIFACT_BASE to read \
             its artifact"
        );
    };
    open_package_source(PackageSource::Artifact {
        tag: format!("{id}-{version}").parse()?,
        registry: platform.registry(base).await?,
    })
    .await
    .with_context(|| format!("open package artifact {id}@{version}"))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::apply_order;

    #[test]
    fn sequence_defaults_use_map_names_and_refuse_conflicting_declarations() {
        let mut base = wamn_publish::test_support::source_manifest();
        base.package.id = "base".parse().expect("valid name");
        base.sequences = serde_json::from_value(serde_json::json!({
            "order": {"pattern": "PO-{seq:po}"}
        }))
        .unwrap();
        let mut overlay = base.clone();
        overlay.package.id = "overlay".parse().expect("valid name");
        let applications = serde_json::from_value(serde_json::json!({
            "receiving": {"packages": ["base", "overlay"]}
        }))
        .unwrap();
        let defaults = super::sequence_defaults(&applications, [&base, &overlay]).unwrap();
        assert_eq!(
            defaults,
            [crate::configuration::SequenceDefault {
                application: "receiving".parse().unwrap(),
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
        let [overlay, base, other, outside] = [
            "client_acme_receiving",
            "wamn_receiving",
            "other",
            "outside_the_release",
        ]
        .map(|name| name.parse().unwrap());
        let packages = [
            (&overlay, vec![&base]),
            (&base, Vec::new()),
            (&other, vec![&outside]),
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
        assert!(
            apply_order(&[
                (&"a".parse().unwrap(), vec![&"b".parse().unwrap()]),
                (&"b".parse().unwrap(), vec![&"a".parse().unwrap()])
            ])
            .is_err()
        );
    }
}
