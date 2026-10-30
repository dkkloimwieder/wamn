//! Step 5 of `apply`: expand
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The policy name and the route host of the environment row are written
//! together, then the durability projection is reconciled from the declared
//! policy. With a release, the package stage runs (§10.2) and CDC is ensured
//! for its packages. The release's
//! components receive their catalog bindings before Helm writes.

use anyhow::Context as _;
use wamn_schema_control::BareSchemaName;

use super::Platform;
use super::analyse::{Analysis, connect};
use super::document::EnvironmentDocument;
use super::ensure::{RUN_SCHEMA, ensure_cdc, project_url};
/// Write the policy name and the route host where they differ.
const UPDATE_ROW_SQL: &str = "\
UPDATE registry.project_envs SET policy_name = $4, route_host = $5 \
 WHERE org = $1 AND project = $2 AND env = $3 \
   AND (policy_name, route_host) IS DISTINCT FROM ($4, $5)";

/// Expand the environment toward the document.
///
/// # Errors
///
/// When a write fails or the package stage refuses. Nothing is written to
/// Kubernetes before this step succeeds.
pub async fn expand(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let triple = document.triple();
    let env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    let mut system = connect(&platform.system_database_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let tx = crate::provision_project_env::provisioning_transaction(&mut system).await?;
    tx.execute(
        UPDATE_ROW_SQL,
        &[
            &document.org,
            &document.project,
            &document.env,
            &document.policy,
            &document.route_host,
        ],
    )
    .await
    .context("write the policy name and the route host")?;
    tx.batch_execute("SET LOCAL ROLE wamn_db_owner").await?;
    let bindings = super::bindings::analyse(&tx, document, &analysis.packages).await?;
    tx.commit().await?;
    drop(system);

    // The durability projection of the declared policy (R2).
    let project_url = project_url(platform, &triple).await?;
    let drops =
        super::bindings::project_authority(&platform.system_database_url, &project_url, document)
            .await?;
    platform.auth_changes().await?.publish(drops).await;
    let source = crate::verification_policy::read_authoritative_environment_policy(
        &platform.system_database_url,
        &document.org,
        &document.policy,
        true,
    )
    .await?;
    let mut project = connect(&project_url).await?;
    crate::reconcile_run_plane::converge_environment_policy(
        &mut project,
        &BareSchemaName::new(RUN_SCHEMA.to_owned())?,
        &env_key,
        &document.env,
        &source,
        true,
    )
    .await
    .context("reconcile the durability projection")?;

    // The package stage, CDC of its packages, then the release's bindings,
    // which name the release cache row the stage wrote.
    let mut clients = std::collections::BTreeMap::new();
    if let Some(release) = &analysis.authorities.release {
        let analysed = analysis
            .authorities
            .project
            .as_ref()
            .map(|project| project.installed.clone())
            .unwrap_or_default();
        let manifests = super::stage::stage(
            platform,
            document,
            &analysed,
            &release.digest,
            &release.manifest,
            &project_url,
        )
        .await?;
        clients = super::clients::resolve(
            document,
            manifests.iter().filter_map(|manifest| {
                manifest
                    .client_package
                    .as_ref()
                    .map(|_| manifest.package.id.as_str())
            }),
        )?;
        ensure_cdc(platform, document, &manifests).await?;
    }
    if let Some(release) = &analysis.authorities.release {
        let tx = project.transaction().await?;
        super::bindings::write(&tx, &env_key, &release.digest, document, &bindings).await?;
        tx.commit().await?;
    }
    Ok(clients)
}
