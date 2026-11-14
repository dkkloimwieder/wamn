//! Prepare chart inputs, then apply one application's Helm release.
//! Helm handles pending operations and reports rollout failures.

pub(super) mod plan;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde_yaml::{Mapping, Value};

use super::Platform;
use super::analyse::{Analysis, ReleaseFacts};
use super::document::EnvironmentDocument;
use super::ensure::PrivateDirectory;
use wamn_release_control::release_chart::{self, RoleName, ValuesInput};

/// What the write left for readiness.
#[derive(Debug)]
pub struct Written {
    pub release_name: String,
    /// The roles the release implies; each is a WorkloadDeployment.
    pub roles: Vec<RoleName>,
    /// The end of the readiness budget, set when the write started (§10.3).
    pub deadline: Instant,
}

/// Prepared chart inputs, retained until Helm finishes.
pub(super) struct Prepared {
    directory: PrivateDirectory,
    roles: Vec<RoleName>,
    budget: Duration,
}

/// Render and validate chart inputs before applying the release.
pub(super) async fn prepare(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
    release: &ReleaseFacts,
) -> anyhow::Result<Prepared> {
    crate::platform::edge::warn_conflicts(
        &platform.target,
        &document.applications[&analysis.application].hostname,
        &analysis.authorities.release_name,
    )
    .await?;
    let policy = &analysis.authorities.policy;
    let budget = Duration::from_secs(u64::try_from(policy.readiness_budget_seconds)?);
    let (rendered, names) = release_values(
        document,
        &analysis.application,
        &release.manifest,
        &release.digest,
        &platform.release_artifact_base,
        platform
            .component_artifact_base
            .as_deref()
            .context("missing component registry")?,
        &analysis.actor,
    )?;
    let release_name = analysis.authorities.release_name.clone();
    let private = PrivateDirectory::create()?;
    let values = private.path("values.yaml");
    std::fs::write(&values, serde_yaml::to_string(&rendered)?)
        .with_context(|| format!("write {}", values.display()))?;
    super::runtime::require_chart_references(
        &platform.target,
        &platform.chart,
        &release_name,
        &rendered,
    )
    .await?;
    Ok(Prepared {
        directory: private,
        roles: names,
        budget,
    })
}

/// Apply only the prepared application's chart and recover only its pending operation.
pub(super) async fn write(
    platform: &Platform,
    analysis: &Analysis,
    prepared: Prepared,
    rollback: bool,
) -> anyhow::Result<Written> {
    let release_name = analysis.authorities.release_name.clone();
    let budget = prepared.budget;
    let deadline = Instant::now() + budget;
    let values = prepared.directory.path("values.yaml");
    release_chart::recover_pending(&platform.target, &release_name).await?;
    release_chart::upgrade(
        &platform.target,
        &platform.chart,
        &release_name,
        &values,
        budget,
        &format!(
            "{} by {}",
            if rollback { "rollback" } else { "apply" },
            analysis.actor
        ),
    )
    .await
    .with_context(|| format!("Helm release {release_name} failed; inspect its status and pods"))?;
    Ok(Written {
        release_name,
        roles: prepared.roles,
        deadline,
    })
}

/// Render application values without host configuration.
pub fn render(input: &ValuesInput) -> anyhow::Result<Value> {
    release_chart::values(input)
}

/// Build release values from the application selection and artifact repositories.
pub fn release_values(
    document: &EnvironmentDocument,
    application: &str,
    manifest: &wamn_catalog::ServingManifest,
    manifest_digest: &str,
    artifact_base: &str,
    component_base: &str,
    actor: &str,
) -> anyhow::Result<(Value, Vec<RoleName>)> {
    plan::release_values(
        document,
        application,
        manifest,
        manifest_digest,
        artifact_base,
        component_base,
        actor,
    )
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
