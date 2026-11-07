//! Prepare only the selected application's runtime control facts before Helm starts it.

use super::{Platform, analyse::Analysis, document::EnvironmentDocument};
use anyhow::{Context as _, ensure};
use wamn_package_control::database::connect;

/// Prepare a selected release without changing application SQL or shared environment state.
/// Old bindings and grants remain usable after any preparation or rollout failure.
pub async fn expand(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let Some(release) = &analysis.authorities.release else {
        return Ok(std::collections::BTreeMap::new());
    };
    let selected = super::analyse::select_application(document, Some(&analysis.application))?;
    let clients = super::clients::resolve(
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
    )
    .await?;
    Ok(clients)
}

/// Commit the control facts used by application activation, without invoking Kubernetes.
pub async fn prepare(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    digest: &str,
    manifest: &wamn_catalog::ServingManifest,
    packages: &[wamn_catalog::manifest::PackageManifest],
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
    let env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    let project_url = super::ensure::project_url(platform, &document.triple())
        .await
        .context("environment is not provisioned; run env provision and env initialize")?;
    let mut project = super::analyse::connect_project(&project_url)
        .await
        .context("application database is absent; run env provision and env initialize")?;
    let initialized: bool = project
        .query_one("SELECT to_regclass('catalog.packages') IS NOT NULL", &[])
        .await?
        .get(0);
    ensure!(
        initialized,
        "application database has no baselines; run env initialize"
    );
    for package in &manifest.release.packages {
        ensure!(
            project
                .query_opt(
                    "SELECT 1 FROM catalog.packages WHERE env_key=$1 AND package_id=$2",
                    &[&env_key, &package.package_id()]
                )
                .await?
                .is_some(),
            "package {} has no installed baseline; run env initialize",
            package.package_id()
        );
    }
    let system = connect(&platform.system_database_url).await?;
    let bindings = super::bindings::analyse(&system, document, packages).await?;
    let changes = platform.auth_changes().await?;
    // The immutable cache can survive a later preparation failure. It selects no release.
    super::stage::write_release(&project_url, &env_key, digest, manifest).await?;
    let tx = project.transaction().await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_project_state::PlatformComponent::Provisioning,
    ))
    .await?;
    tx.query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
        .await?;
    wamn_package_control::connection_requirement::prepare_release(
        &tx,
        &env_key,
        manifest,
        packages,
        platform.component_source()?,
    )
    .await?;
    tx.query_one(
        wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL,
        &[&env_key],
    )
    .await?;
    let mut auth = wamn_platform_identity::auth_changes::AuthDrops::default();
    super::stage::write_runtime_grants(&system, &tx, &env_key, manifest, packages, &mut auth)
        .await?;
    let mut drops = wamn_platform_identity::connection_changes::ConnectionDrops::default();
    super::bindings::write(&tx, &env_key, digest, document, &bindings, &mut drops).await?;
    tx.commit().await?;
    changes.publish(auth).await;
    wamn_platform_identity::connection_changes::ConnectionChanges::from(&changes)
        .publish(drops)
        .await;
    Ok(())
}
