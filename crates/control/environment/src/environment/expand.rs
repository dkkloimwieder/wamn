//! Prepare only the selected application's runtime control facts before Helm starts it.

use super::{Platform, analyse::Analysis, document::EnvironmentDocument};
use anyhow::{Context as _, ensure};
use sqlx::{Connection as _, Row as _};
use wamn_postgres::connect;

/// Prepare a selected release without changing application SQL or shared environment state.
/// Old bindings and grants remain usable after any preparation or rollout failure.
pub async fn expand(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
    actor: &str,
) -> anyhow::Result<()> {
    let Some(release) = &analysis.authorities.release else {
        return Ok(());
    };
    let selected = super::analyse::select_application(document, Some(&analysis.application))?;
    super::clients::verify_client_package(
        &selected.applications[&analysis.application],
        analysis.packages.iter().filter_map(|package| {
            package
                .client_package
                .as_ref()
                .map(|_| package.package.id.as_str())
        }),
    )?;
    prepare(
        platform,
        document,
        &analysis.application,
        &release.digest,
        &release.manifest,
        &analysis.packages,
        actor,
    )
    .await
}

/// Commit the control facts used by application activation, without invoking Kubernetes.
pub async fn prepare(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    digest: &str,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[wamn_compiler::manifest::PackageManifest],
    actor: &str,
) -> anyhow::Result<()> {
    for package in packages.iter().filter(|package| {
        wamn_release_control::publish_release::composes_integration(package, &manifest.components)
    }) {
        ensure!(
            document.application_for_package(&package.package.id) == Some(application),
            "application role for {} has ambiguous ownership",
            package.package.id
        );
    }
    let selected = super::analyse::select_application(document, Some(application))?;
    let document = &selected;
    ensure!(
        matches!(document.legacy_release(), super::document::DeclaredRelease::Digest(selected) if selected == digest),
        "activation digest differs from the application selection"
    );
    let refusals = super::analyse::application_refusals(
        &document.membership(),
        manifest
            .release
            .packages
            .iter()
            .map(wamn_catalog::PackageCoordinate::package_id),
    );
    ensure!(refusals.is_empty(), "{}", refusals.join("; "));
    let project_url = super::ensure::project_url(platform, &document.triple())
        .await
        .context("environment is not provisioned; run env provision and env initialize")?;
    prepare_at(
        platform,
        document,
        application,
        digest,
        manifest,
        packages,
        &project_url,
        actor,
    )
    .await
}

/// Rebuild runtime facts through the same owner in a prepared replacement database.
#[expect(
    clippy::too_many_arguments,
    reason = "Release preparation needs its target, artifact, packages, database, and publisher"
)]
pub(super) async fn prepare_at(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    digest: &str,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[wamn_compiler::manifest::PackageManifest],
    project_url: &str,
    actor: &str,
) -> anyhow::Result<()> {
    let document = super::analyse::select_application(document, Some(application))?;
    let document = &document;
    let project_env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    let mut project = super::analyse::connect_project(project_url)
        .await
        .context("application database is absent; run env provision and env initialize")?;
    let initialized: bool = sqlx::query("SELECT to_regclass('catalog.packages') IS NOT NULL")
        .fetch_one(&mut project)
        .await?
        .get(0);
    ensure!(
        initialized,
        "application database has no baselines; run env initialize"
    );
    for package in &manifest.release.packages {
        ensure!(
            sqlx::query(
                "SELECT 1 FROM catalog.packages WHERE project_env_key=$1 AND package_id=$2"
            )
            .bind(&project_env_key)
            .bind(package.package_id())
            .fetch_optional(&mut project)
            .await?
            .is_some(),
            "package {} has no installed baseline; run env initialize",
            package.package_id()
        );
    }
    let mut system = connect(&platform.system_database_url).await?;
    let bindings = super::bindings::analyse(&mut system, document, packages).await?;
    let changes = platform.auth_changes().await?;
    // The immutable cache can survive a later preparation failure. It selects no release.
    super::stage::write_release(project_url, &project_env_key, digest, manifest, actor).await?;
    let mut tx = project.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(
            wamn_identity_protocol::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)?;
    sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
        .bind(&project_env_key)
        .fetch_one(&mut *tx)
        .await?;
    super::document::write_revocation_configuration(&mut tx, document).await?;
    wamn_package_control::connection_requirement::prepare_release(
        &mut tx,
        &project_env_key,
        manifest,
        packages,
        platform.component_source()?,
    )
    .await?;
    sqlx::query(wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL)
        .bind(&project_env_key)
        .fetch_one(&mut *tx)
        .await?;
    let mut auth = wamn_broker::auth_changes::AuthDrops::default();
    super::stage::write_runtime_grants(
        &mut system,
        &mut tx,
        &project_env_key,
        manifest,
        packages,
        &mut auth,
    )
    .await?;
    let mut drops = wamn_broker::connection_changes::ConnectionDrops::default();
    super::bindings::write(
        &mut tx,
        &project_env_key,
        digest,
        document,
        &bindings,
        &mut drops,
    )
    .await?;
    tx.commit().await?;
    changes.publish(auth).await;
    changes.publish(drops).await;
    Ok(())
}
