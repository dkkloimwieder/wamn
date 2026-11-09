//! Prepare chart inputs before selection persistence, then apply one application's Helm release.
//! Helm handles pending operations and reports rollout failures.

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

/// Prepared chart inputs, retained until Helm finishes.
pub(super) struct Prepared {
    directory: PrivateDirectory,
    roles: Vec<RoleName>,
    budget: Duration,
}

/// Render and validate chart inputs before persisting the application selection.
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
    let installed = crate::platform::resources::release_values(
        &platform.target,
        &platform.target.namespace,
        &analysis.authorities.release_name,
    )
    .await?;
    let (mut rendered, names) = release_values(
        document,
        &analysis.application,
        &release.manifest,
        &release.digest,
        &platform.release_artifact_base,
        u32::try_from(policy.drain_bound_seconds)?,
        &analysis.actor,
        platform.host_group.as_deref(),
        installed.as_ref(),
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
    let poller = host_report::Poller::start(&platform.target, &release_name);
    let upgraded = release_chart::upgrade(
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
    .await;
    let report = poller.finish();
    upgraded.map_err(|error| host_report::attach(&error, &report))?;
    Ok(Written {
        release_name,
        roles: prepared.roles,
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
    installed: Option<&serde_json::Value>,
) -> anyhow::Result<(Value, Vec<RoleName>)> {
    let group = host_group(
        host_group_path,
        &document.org,
        &document.project,
        &document.env,
    )?;
    let (mut values, roles) = plan::release_values(
        document,
        application,
        manifest,
        manifest_digest,
        artifact_base,
        drain_bound_seconds,
        actor,
        group,
    )?;
    let group = &mut values["runtime-operator"]["runtime"]["hostGroups"][0];
    if let Some(image) = super::runtime::host_image(
        installed,
        group["name"]
            .as_str()
            .context("application host group name")?,
    ) {
        group["image"] = serde_yaml::to_value(image)?;
    }
    Ok((values, roles))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn application_reapply_keeps_only_its_named_host_image() -> anyhow::Result<()> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let chart = crate::platform::artifact_plan::test_util::prepared_chart(&repository)?;
        let digest = format!("sha256:{}", "c".repeat(64));
        let document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
            "org":"acme", "project":"apps", "env":"dev", "target":"cloud", "policy":"dev",
            "applications":{"alpha":{"packages":["alpha"],"current":digest,"hostname":"alpha.example"}}
        }))?)?;
        let manifest = serde_json::from_value(json!({
            "format-version":wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release":{"packages":[]}, "components":[], "routes":[], "attachments":{}
        }))?;
        let render = |installed| {
            release_values(
                &document,
                "alpha",
                &manifest,
                &digest,
                "registry.example/releases",
                70,
                "render",
                None,
                installed,
            )
        };
        let (fresh, _) = render(None)?;
        let mut installed = serde_json::to_value(&fresh)?;
        let image = json!({"registry":"selected.example", "repository":"host",
            "tag":format!("selected@sha256:{}", "b".repeat(64)), "pull_policy":"IfNotPresent"});
        let group = &mut installed["runtime-operator"]["runtime"]["hostGroups"][0];
        group["image"] = image.clone();
        group["replicas"] = 9.into();
        installed["routeHost"] = "stale.example".into();
        installed["release"]["manifestDigest"] = "stale".into();
        installed["runtime-operator"]["runtime"]["hostGroups"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"name":"unrelated", "image":{"tag":"wrong"}}));
        let (mut reapplied, _) = render(Some(&installed))?;
        assert_eq!(
            serde_json::to_value(
                &reapplied["runtime-operator"]["runtime"]["hostGroups"][0]["image"]
            )?,
            image
        );
        let entry = json!({"chart":chart.0, "version":crate::platform::artifact_plan::select(&chart.0)?.version,
            "name":"alpha", "namespace":"hosts", "values":reapplied});
        let objects = crate::platform::resources::helm_template(&entry)?;
        let host = objects
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .unwrap();
        assert_eq!(
            host["spec"]["template"]["spec"]["containers"][0]["image"],
            format!("selected.example/host:selected@sha256:{}", "b".repeat(64))
        );
        reapplied["runtime-operator"]["runtime"]["hostGroups"][0]
            .as_mapping_mut()
            .unwrap()
            .remove("image");
        assert_eq!(reapplied, fresh);
        Ok(())
    }
}
