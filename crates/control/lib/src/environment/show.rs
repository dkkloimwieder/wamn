//! `env show`: an environment document synthesized from the authorities
//! (docs/plan/platform-deploy.md §10.1, R21 (3)).
//!
//! The release is the digest of the newest successful Helm revision, or
//! `none`. The route host and the policy name come from the environment row.
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
        policy: String::new(),
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
    if let Some(digest) = read_revision(platform, &name)
        .await?
        .as_ref()
        .and_then(|revision| revision.manifest_digest().map(str::to_owned))
    {
        document.release = DeclaredRelease::Digest(digest);
    }
    if let Some(project) = read_project(platform, &document, &row).await? {
        for (instance, existing) in project.instances {
            if !existing.enabled {
                continue;
            }
            let requirement_type: RequirementType = serde_json::from_value(
                serde_json::Value::String(existing.requirement_type.clone()),
            )
            .with_context(|| {
                format!(
                    "connection {instance} has type {}",
                    existing.requirement_type
                )
            })?;
            document.connections.insert(
                instance,
                ConnectionDefinition {
                    requirement_type,
                    definition: existing.definition.unwrap_or_default(),
                },
            );
        }
        document.floors = project.floors;
    }
    Ok(document)
}
