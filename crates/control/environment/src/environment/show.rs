//! Export canonical environment selections and observed serving membership.

pub(super) mod plan;
use super::{Platform, analyse::read_revision, document::EnvironmentDocument};
use anyhow::Context as _;
use plan::revision_applications;
use std::path::Path;
use wamn_control_registry::Triple;

/// Export the canonical document without substituting successful Helm revisions.
pub async fn show(platform: &Platform, file: &Path) -> anyhow::Result<EnvironmentDocument> {
    EnvironmentDocument::read_deployed(platform, file, None).await
}

/// Read the target application's release and membership from the same successful revision.
pub async fn serving_application(
    platform: &Platform,
    triple: &Triple,
    application: &str,
) -> anyhow::Result<(String, wamn_catalog::EnvironmentApplications)> {
    let name = wamn_control_provision::environment_state::application_release_name(
        &triple.org,
        &triple.project,
        triple.env.as_str(),
        application,
    )?;
    let revision = read_revision(platform, &name)
        .await?
        .with_context(|| format!("application {application} in {triple} serves no release"))?;
    let membership = revision_applications(&revision.values)?;
    anyhow::ensure!(
        membership.len() == 1 && membership.contains_key(application),
        "application Helm revision has foreign membership"
    );
    let digest = revision
        .manifest_digest()
        .context("application revision has no release digest")?
        .to_owned();
    Ok((digest, membership))
}
