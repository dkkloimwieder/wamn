//! Prepare chart inputs, then apply one application's Helm release.
//! Helm handles pending operations and reports rollout failures.

pub(super) mod plan;

use std::path::Path;

use anyhow::Context as _;
use serde_yaml::{Mapping, Value};

use super::document::EnvironmentDocument;
use crate::release_chart::{self, RoleName, ValuesInput};

/// Render application values without host configuration.
pub fn render(input: &ValuesInput) -> anyhow::Result<Value> {
    release_chart::values(input)
}

/// Build release values from the application selection and artifact repositories.
pub async fn release_values(
    document: &EnvironmentDocument,
    application: &wamn_catalog::ApplicationName,
    manifest: &wamn_catalog::ServingManifest,
    manifest_digest: &wamn_digest::Digest,
    actor: &str,
    source: wamn_oci::component_artifact::component_artifact_source::ComponentArtifactSourceConfig,
) -> anyhow::Result<(Value, Vec<RoleName>)> {
    plan::release_values(
        document,
        application,
        manifest,
        manifest_digest,
        actor,
        source,
    )
    .await
}

/// Read the host placement body used to observe replica policy.
/// Project credentials come only from the environment chart projection.
pub fn host_group(path: Option<&Path>) -> anyhow::Result<Mapping> {
    let group = match path {
        None => Mapping::new(),
        Some(path) => serde_yaml::from_str(
            &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("{} is not a YAML mapping", path.display()))?,
    };
    plan::host_group(group)
}
