//! Step 6 of `apply`: the one rollout write
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The verb renders the `release` values. When the chart version and the
//! values, `audit` excluded, equal the newest successful revision's, there is
//! nothing for Kubernetes to converge on and the Helm call alone is skipped;
//! readiness still runs. Otherwise it runs `helm upgrade --install` at the
//! chart's own version with `--wait`, the policy's readiness
//! budget as `--timeout`, `--rollback-on-failure`, `--history-max 50` and the
//! description `apply by <actor>`, or `rollback by <actor>: <reason>` (§11.2).
//! A connection-only apply writes no revision,
//! because no connection fact is in the values.

pub(super) mod plan;
use plan::without_audit;

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
    /// Whether Helm wrote a revision.
    pub helm_written: bool,
}

/// Render the values and write the release chart when they differ.
///
/// # Errors
///
/// When the chart has no version, the values do not render, or Helm fails.
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
    let shown: Value = serde_yaml::from_str(&helm_show_chart(&platform.chart).await?)
        .context("parse the chart's Chart.yaml")?;
    let version = shown["version"]
        .as_str()
        .with_context(|| format!("the chart {} has no version", platform.chart.display()))?;
    let (rendered, names) = release_values(
        document,
        &release.manifest,
        &release.digest,
        &platform.release_artifact_base,
        u32::try_from(policy.drain_bound_seconds)?,
        &analysis.actor,
        platform.host_group.as_deref(),
        clients,
    )?;
    let release_name = analysis.authorities.release_name.clone();
    let unchanged = analysis
        .authorities
        .revision
        .as_ref()
        .is_some_and(|revision| {
            revision.chart == format!("release-{version}")
                && serde_json::to_value(&rendered).is_ok_and(|rendered| {
                    without_audit(rendered) == without_audit(revision.values.clone())
                })
        });
    if unchanged {
        return Ok(Written {
            release_name,
            roles: names,
            deadline,
            helm_written: false,
        });
    }
    let private = PrivateDirectory::create()?;
    let values = private.path("values.yaml");
    std::fs::write(&values, serde_yaml::to_string(&rendered)?)
        .with_context(|| format!("write {}", values.display()))?;
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
        helm_written: true,
    })
}

/// Render apply values with the platform overlay and derived credential references.
pub fn render(input: &ValuesInput, path: Option<&Path>) -> anyhow::Result<Value> {
    let mut input = input.clone();
    input.host_group = host_group(path, &input.org, &input.project, &input.env)?;
    release_chart::values(&input)
}

async fn helm_show_chart(chart: &std::path::Path) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("helm")
        .args(["show", "chart"])
        .arg(chart)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start helm")?;
    ensure!(
        output.status.success(),
        "helm show chart {} exited {}: {}",
        chart.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("helm wrote text")
}

/// Build release values from the document, artifact, policy, and host overlay.
#[expect(
    clippy::too_many_arguments,
    reason = "Release values combine document, artifact, policy, and chart inputs"
)]
pub(crate) fn release_values(
    document: &EnvironmentDocument,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn release_values_keep_both_client_maps_and_clear_an_absent_pin() {
        let mut document = EnvironmentDocument::from_json(
            br#"{"org":"acme","project":"wms","env":"dev","target":"cloud","release":"none","route_host":"wms.example.com","policy":"dev","applications":{}}"#,
        ).unwrap();
        let manifest = serde_json::from_value(json!({
            "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release": {"packages": []}, "components": [], "routes": [], "attachments": {},
        }))
        .unwrap();
        let digest = wamn_execution_contract::bytes_sha256(b"web artifact");
        for client in [
            Some(super::super::document::Client {
                package: "wms".into(),
                web: digest.clone(),
            }),
            None,
        ] {
            document.client = client;
            let clients = super::super::clients::resolve(&document, ["wms"].into_iter()).unwrap();
            let (values, _) = release_values(
                &document,
                &manifest,
                &digest,
                "registry.example/releases",
                30,
                "test",
                None,
                &clients,
            )
            .unwrap();
            if document.client.is_some() {
                assert_eq!(
                    values["webClients"]["wms.example.com"].as_str(),
                    Some("wms")
                );
                assert_eq!(
                    values["webDigests"]["wms.example.com"].as_str(),
                    Some(digest.as_str())
                );
            } else {
                assert!(values["webClients"].as_mapping().unwrap().is_empty());
                assert!(values["webDigests"].as_mapping().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn audit_is_no_difference() {
        let written = json!({"release": {"manifestDigest": "sha256:a"}, "audit": {"actor": "a"}});
        let rendered = json!({"release": {"manifestDigest": "sha256:a"}, "audit": {"actor": "b"}});
        assert_eq!(without_audit(written.clone()), without_audit(rendered));
        let other = json!({"release": {"manifestDigest": "sha256:b"}, "audit": {"actor": "a"}});
        assert_ne!(without_audit(written), without_audit(other));
    }
}
