//! Release cache and permission retention, independent of rollout.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use sqlx::{Connection as _, Row as _};
use wamn_compiler::manifest::PackageManifest;
use wamn_postgres::connect;
use wamn_registry::Triple;

use super::{Platform, document::EnvironmentDocument, lock::LifecycleLock};

/// Remove unused cached releases and their generated permissions under environment exclusion.
/// Unfinished runs and live hosts keep their release facts. Status reports stranded runs.
///
/// # Errors
/// Refuses unreadable host state, corrupt artifacts, uncertain role ownership, or lock loss.
pub async fn prune(platform: &Platform, triple: &Triple) -> anyhow::Result<Vec<String>> {
    LifecycleLock::hold(&platform.system_database_url, triple, async |_| {
        let document = EnvironmentDocument::read_serving(platform, triple).await?;
        let residency = super::drain::observe(platform,triple).await?;
        let live = super::drain::loaded(&residency);
        let mut retained: BTreeSet<String> = live.into_keys().map(|(_, digest)| digest).collect();
        for (application, selection) in &document.applications {
            if let super::document::DeclaredRelease::Digest(digest) = &selection.current {
                retained.insert(digest.clone());
            }
            // A failed write can leave Helm serving the previous desired release.
            let name = super::analyse::application_release_name(&document, application)?;
            if let Some(revision) = super::analyse::read_revision(platform, &name).await? {
                retained.insert(revision.manifest_digest().context("Helm revision has no release digest")?.to_owned());
            }
        }
        let project_env_key = wamn_registry::project_env_key(&triple.org, &triple.project, triple.env.as_str());
        let url = super::ensure::project_url(platform, triple).await?;
        let mut project = super::analyse::connect_project(&url).await?;
        let mut releases = BTreeMap::new();
        for row in sqlx::query("SELECT manifest_digest, canonical_bytes FROM catalog.releases WHERE project_env_key=$1")
            .bind(&project_env_key).fetch_all(&mut project).await? {
            let digest: String = row.get(0);
            let (manifest, actual) = wamn_catalog::ServingManifest::from_canonical_bytes(row.get(1))?;
            ensure!(actual.as_str() == digest, "release cache digest mismatch");
            let packages = super::analyse::read_release_packages(platform, &manifest).await?;
            releases.insert(digest, (manifest, packages));
        }
        let mut tx = project.begin().await?;
        sqlx::query(wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL)
            .bind(&project_env_key).fetch_one(&mut *tx).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(wamn_control_provision::bind_platform_principal_sql(
            wamn_identity_protocol::PlatformComponent::Provisioning,
        ))).execute(&mut *tx).await?;
        let mut connections = wamn_broker::connection_changes::ConnectionDrops::default();
        let removed = prune_releases(&mut tx, &project_env_key, &retained, &mut connections).await?;
        let mut auth = wamn_broker::auth_changes::AuthDrops::default();
        let mut system = connect(&platform.system_database_url).await?;
        retire_runtime_grants(&mut system, &mut tx, &project_env_key, &releases, &removed, &mut auth).await?;
        tx.commit().await?;
        let changes = platform.auth_changes().await?;
        changes.publish(auth).await;
        changes.publish(connections).await;
        crate::prune_dev_tags::prune_published(platform).await?;
        Ok(removed)
    }).await
}

/// Remove unserved releases after excluding unfinished-run pins in the same transaction.
/// The caller holds environment exclusion and supplies loaded releases, desired selections and observed Helm selections.
///
/// # Errors
/// Returns database errors. Unknown host identity refuses cleanup.
pub async fn prune_releases(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    retained: &BTreeSet<String>,
    drops: &mut wamn_broker::connection_changes::ConnectionDrops,
) -> anyhow::Result<Vec<String>> {
    ensure!(
        !retained.contains("unlabelled"),
        "cannot retain releases with unknown live hosts"
    );
    sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
        .bind(project_env_key)
        .execute(&mut **tx)
        .await?;
    let retained: Vec<&str> = retained.iter().map(String::as_str).collect();
    let removed: Vec<String> = sqlx::query_scalar(
        "SELECT r.manifest_digest FROM catalog.releases r WHERE r.project_env_key=$1 \
         AND NOT (r.manifest_digest=ANY($2)) AND NOT EXISTS (SELECT FROM wamn_run.runs u \
         WHERE u.project_env_key=r.project_env_key AND u.manifest_digest=r.manifest_digest \
         AND u.status NOT IN ('completed','failed','infrastructure-failure')) FOR UPDATE OF r",
    )
    .bind(project_env_key)
    .bind(&retained)
    .fetch_all(&mut **tx)
    .await?;
    for digest in &removed {
        for table in ["catalog.connection_binding", "catalog.release_connection"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE project_env_key=$1 AND manifest_digest=$2"
            )))
            .bind(project_env_key)
            .bind(digest)
            .execute(&mut **tx)
            .await?;
        }
        drops.release(project_env_key, digest);
    }
    if !removed.is_empty() {
        // Terminal history still references its immutable manifest. Only the owner can
        // remove unreferenced cache rows, with the trigger change inside this transaction.
        sqlx::raw_sql("ALTER TABLE catalog.releases DISABLE TRIGGER releases_immutable")
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "DELETE FROM catalog.releases r WHERE r.project_env_key=$1 AND r.manifest_digest=ANY($2) \
             AND NOT EXISTS (SELECT FROM wamn_run.runs u WHERE u.project_env_key=r.project_env_key \
             AND u.manifest_digest=r.manifest_digest)",
        )
        .bind(project_env_key)
        .bind(&removed)
        .execute(&mut **tx)
        .await?;
        sqlx::raw_sql("ALTER TABLE catalog.releases ENABLE TRIGGER releases_immutable")
            .execute(&mut **tx)
            .await?;
    }
    Ok(removed)
}

/// Remove generated grants exclusive to retired releases from exclusively owned roles.
/// Package manifests come from verified artifacts. Uncertain ownership refuses the transaction.
pub async fn retire_runtime_grants(
    system: &mut sqlx::PgConnection,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    releases: &BTreeMap<String, (wamn_catalog::ServingManifest, Vec<PackageManifest>)>,
    retired: &[String],
    drops: &mut wamn_broker::auth_changes::AuthDrops,
) -> anyhow::Result<()> {
    let mut removed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut retained: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (digest, (manifest, packages)) in releases {
        let dependencies = packages
            .iter()
            .map(|package| (package.package.id.as_str(), package))
            .collect();
        for package in packages.iter().filter(|package| {
            wamn_publish::publish_release::composes_integration(package, &manifest.components)
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
                    wamn_publish::publish_release::application_dependency_permissions(
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
        let subject = wamn_platform_identity::application_subject(project_env_key, &package)?;
        let principal = wamn_platform_identity::resolve_subject(
            system,
            wamn_platform_identity::PrincipalType::Service,
            &subject,
        )
        .await?
        .context("application principal ownership is unknown; retain old facts")?;
        let owned: bool = sqlx::query("SELECT EXISTS (SELECT FROM platform.role WHERE name=$1 AND created_by=$2::text::uuid) \
             AND NOT EXISTS (SELECT FROM platform.user_role WHERE role_name=$1 AND user_id<>$3::text::uuid)").bind(&role).bind(&actor).bind(principal.id().to_string().as_str()).fetch_one(&mut **tx).await?.get(0);
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
            let generated: bool = sqlx::query("SELECT EXISTS (SELECT FROM platform.permission WHERE role_name=$1 AND permission=$2 AND created_by=$3::text::uuid AND updated_by=$3::text::uuid)").bind(&role).bind(&permission).bind(&actor).fetch_one(&mut **tx).await?.get(0);
            if generated {
                wamn_platform_identity::application::mutate_grant(
                    tx,
                    project_env_key,
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
