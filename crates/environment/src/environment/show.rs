//! Export canonical environment selections and observed serving membership.

pub(super) mod plan;
use super::{Platform, analyse::read_revision};
use anyhow::Context as _;
use plan::revision_applications;
use wamn_registry::Triple;

/// Read the target application's release and membership from the same successful revision.
pub async fn serving_application(
    platform: &Platform,
    triple: &Triple,
    application: &wamn_catalog::ApplicationName,
) -> anyhow::Result<(wamn_digest::Digest, wamn_catalog::EnvironmentApplications)> {
    let name = wamn_provision::environment_names::application_release_name(
        &triple.org,
        &triple.project,
        &triple.env,
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
