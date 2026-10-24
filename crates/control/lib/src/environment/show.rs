//! `env show`: an environment document synthesized from the authorities
//! (docs/plan/platform-deploy.md §10.1, R21 (3)).
//!
//! The release is the digest of the newest successful Helm revision, or
//! `none`, and the applications are the ones that revision keeps. The route host and the policy name come from the environment row.
//! The connections are the enabled instances of the project database, and the
//! floors are the recorded floors. `show` takes no lock.

use std::collections::BTreeMap;

use anyhow::Context as _;
use wamn_catalog::RequirementType;
use wamn_control_registry::Triple;

use super::Platform;
use super::analyse::{connect, read_project, read_revision, read_row};
use super::document::{ConnectionDefinition, DeclaredRelease, EnvironmentDocument};
use crate::release_chart;

/// Synthesize the document of an environment.
///
/// # Errors
///
/// When the environment has no row, or an authority cannot be read.
pub async fn show(platform: &Platform, triple: &Triple) -> anyhow::Result<EnvironmentDocument> {
    let mut document = EnvironmentDocument {
        org: triple.org.clone(),
        project: triple.project.clone(),
        env: triple.env.as_str().to_owned(),
        release: DeclaredRelease::None,
        route_host: String::new(),
        client_package: None,
        policy: String::new(),
        applications: BTreeMap::new(),
        connections: BTreeMap::new(),
        floors: BTreeMap::new(),
    };
    let system = connect(&platform.system_database_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let row = read_row(&system, &document)
        .await?
        .with_context(|| format!("environment {triple} has no row"))?;
    document.route_host = row.route_host.clone().unwrap_or_default();
    document.policy = row.policy_name.clone();
    let name = release_chart::release_name(&document.org, &document.project, &document.env)?;
    if let Some(revision) = read_revision(platform, &name).await? {
        if let Some(digest) = revision.manifest_digest() {
            document.release = DeclaredRelease::Digest(digest.to_owned());
        }
        document.client_package = revision.values["webClients"][&document.route_host]
            .as_str()
            .map(str::to_owned);
        document.applications = revision_applications(&revision.values)?;
    }
    if let Some(project) = read_project(platform, &document, &row).await? {
        for (instance, existing) in project.instances {
            if !existing.enabled {
                continue;
            }
            document
                .connections
                .insert(instance, connection_definition(&existing)?);
        }
        document.floors = project.floors;
    }
    Ok(document)
}

/// The release an environment serves, as [`show`] derives it: the digest of
/// the newest successful Helm revision.
///
/// # Errors
///
/// When Helm cannot be read, or the environment serves no release.
pub async fn serving_release(platform: &Platform, triple: &Triple) -> anyhow::Result<String> {
    let name = release_chart::release_name(&triple.org, &triple.project, triple.env.as_str())?;
    read_revision(platform, &name)
        .await?
        .and_then(|revision| revision.manifest_digest().map(str::to_owned))
        .with_context(|| format!("environment {triple} serves no release"))
}

/// The applications an environment serves, as [`show`] derives them: the
/// ones the newest successful Helm revision keeps.
///
/// # Errors
///
/// When Helm cannot be read, or the environment serves no release.
pub async fn serving_applications(
    platform: &Platform,
    triple: &Triple,
) -> anyhow::Result<wamn_catalog::EnvironmentApplications> {
    let name = release_chart::release_name(&triple.org, &triple.project, triple.env.as_str())?;
    let revision = read_revision(platform, &name)
        .await?
        .with_context(|| format!("environment {triple} serves no release"))?;
    revision_applications(&revision.values)
}

/// The applications one revision's values keep. A revision written before
/// the values carried them keeps none.
fn revision_applications(
    values: &serde_json::Value,
) -> anyhow::Result<wamn_catalog::EnvironmentApplications> {
    match values.get("applications") {
        None | Some(serde_json::Value::Null) => Ok(BTreeMap::new()),
        Some(applications) => serde_json::from_value(applications.clone())
            .context("read the applications of the Helm revision"),
    }
}

/// Reconstruct the source form used by show and rollback.
fn connection_definition(
    existing: &super::analyse::Instance,
) -> anyhow::Result<ConnectionDefinition> {
    if let Some(org) = &existing.org_connection_name {
        return Ok(ConnectionDefinition::Org { org: org.clone() });
    }
    let requirement_type: RequirementType =
        serde_json::from_value(serde_json::Value::String(existing.requirement_type.clone()))
            .context("read the connection type")?;
    Ok(ConnectionDefinition::Inline {
        requirement_type,
        definition: existing.definition.clone().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_metadata_preserves_org_references() {
        let mut instance = super::super::analyse::Instance {
            requirement_type: "http".to_owned(),
            enabled: true,
            definition: Some(
                serde_json::json!({"primary-authority": "https://erp.example", "tls-verification": "verify-authority"}),
            ),
            credential_handle: Some("org:acme:erp".to_owned()),
            org_connection_name: Some("erp".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(connection_definition(&instance).unwrap()).unwrap(),
            serde_json::json!({"org": "erp"})
        );
        instance.org_connection_name = None;
        assert!(matches!(
            connection_definition(&instance).unwrap(),
            ConnectionDefinition::Inline { .. }
        ));
    }
}
