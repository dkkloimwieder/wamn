//! Step 6 of `apply`: the one rollout write
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The verb renders the `release` values, recovers any pending Helm operation,
//! and runs `helm upgrade --install` with `--wait`, the policy's readiness
//! budget as `--timeout`, `--rollback-on-failure`, `--history-max 50` and the
//! description `apply by <actor>`, or `rollback by <actor>: <reason>` (§11.2).

pub(super) mod plan;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_yaml::{Mapping, Value};

use super::Platform;
use super::analyse::{Analysis, ReleaseFacts};
use super::document::EnvironmentDocument;
use super::ensure::PrivateDirectory;
use super::host_report;
use wamn_control_provision::host_credentials::HOST_CREDENTIALS;
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

/// Render the values, recover pending operations, and write the release chart.
///
/// # Errors
///
/// When the values do not render or Helm fails.
/// A Helm failure leaves the previous release serving:
/// `--rollback-on-failure` restores it.
pub async fn write(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
    release: &ReleaseFacts,
    reason: Option<&str>,
    clients: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<Written> {
    let policy = &analysis.authorities.policy;
    let budget = Duration::from_secs(u64::try_from(policy.readiness_budget_seconds)?);
    let deadline = Instant::now() + budget;
    let (mut rendered, names) = release_values(
        document,
        &analysis.application,
        &release.manifest,
        &release.digest,
        &platform.release_artifact_base,
        u32::try_from(policy.drain_bound_seconds)?,
        &analysis.actor,
        platform.host_group.as_deref(),
        clients,
    )?;
    let suffix = &analysis
        .authorities
        .row
        .as_ref()
        .context("missing provisioned environment")?
        .instance_suffix;
    rendered["runtime-operator"]["runtime"]["hostGroups"][0]["env"]
        .as_sequence_mut()
        .context("application host variables")?
        .push(serde_yaml::to_value(
            serde_json::json!({"name":"WAMN_SESSION_INSTANCE_SUFFIX", "value":suffix}),
        )?);
    let release_name = analysis.authorities.release_name.clone();
    let private = PrivateDirectory::create()?;
    let values = private.path("values.yaml");
    std::fs::write(&values, serde_yaml::to_string(&rendered)?)
        .with_context(|| format!("write {}", values.display()))?;
    let metadata: serde_json::Value =
        serde_yaml::from_slice(&std::fs::read(platform.chart.join("Chart.yaml"))?)?;
    let objects = crate::platform::resources::helm_template(&serde_json::json!({
        "chart":platform.chart, "version":metadata["version"],
        "name":release_name, "namespace":platform.target.namespace, "values":rendered,
    }))?;
    super::runtime::require_references(&platform.target, &objects).await?;
    release_chart::recover_pending(&platform.target, &release_name).await?;
    let poller = host_report::Poller::start(&platform.target, &release_name);
    let upgraded = release_chart::upgrade(
        &platform.target,
        &platform.chart,
        &release_name,
        &values,
        budget,
        &match reason {
            None => format!("apply by {}", analysis.actor),
            Some(reason) => format!("rollback by {}: {reason}", analysis.actor),
        },
    )
    .await;
    let report = poller.finish();
    upgraded.map_err(|error| host_report::attach(&error, &report))?;
    Ok(Written {
        release_name,
        roles: names,
        deadline,
    })
}

/// Render apply values with the platform overlay and derived credential references.
pub fn render(input: &ValuesInput, path: Option<&Path>) -> anyhow::Result<Value> {
    let mut input = input.clone();
    input.host_group = match path {
        Some(path) => host_group(Some(path), &input.org, &input.project, &input.env)?,
        None => plan::host_group(input.host_group, &input.org, &input.project, &input.env)?,
    };
    release_chart::values(&input)
}

/// Build release values from the document, artifact, policy, and host overlay.
#[expect(
    clippy::too_many_arguments,
    reason = "Release values combine document, artifact, policy, and chart inputs"
)]
pub fn release_values(
    document: &EnvironmentDocument,
    application: &str,
    manifest: &wamn_catalog::ServingManifest,
    manifest_digest: &str,
    artifact_base: &str,
    drain_bound_seconds: u32,
    actor: &str,
    host_group_path: Option<&Path>,
    clients: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<(Value, Vec<RoleName>)> {
    let group = host_group(
        host_group_path,
        &document.org,
        &document.project,
        &document.env,
    )?;
    plan::release_values(
        document,
        application,
        manifest,
        manifest_digest,
        artifact_base,
        drain_bound_seconds,
        actor,
        group,
        clients,
    )
}

/// The host group body: the platform's part, read from
/// `WAMN_RELEASE_HOST_GROUP`, and the environment's credential Secret
/// references, which the verb derives from the coordinate.
pub fn host_group(
    path: Option<&Path>,
    org: &str,
    project: &str,
    env: &str,
) -> anyhow::Result<Mapping> {
    let group = match path {
        None => Mapping::new(),
        Some(path) => serde_yaml::from_str(
            &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("{} is not a YAML mapping", path.display()))?,
    };
    plan::host_group(group, org, project, env)
}
