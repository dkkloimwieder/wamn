//! Step 6 of `apply`: the one rollout write (docs/plan/platform-deploy.md
//! §10.1 step 6, §9.2, R6, R22 (5)).
//!
//! The verb renders the `release` values. When the chart version and the
//! values, `audit` excluded, equal the newest successful revision's, there is
//! nothing for Kubernetes to converge on and the Helm call alone is skipped;
//! readiness still runs. Otherwise it runs `helm upgrade --install` at
//! [`release_chart::CHART_VERSION`] with `--wait`, the policy's readiness
//! budget as `--timeout`, `--rollback-on-failure`, `--history-max 50` and the
//! description `apply by <actor>`, or `rollback by <actor>: <reason>` (§11.2).
//! A connection-only apply writes no revision,
//! because no connection fact is in the values.

use std::time::{Duration, Instant};

use anyhow::{Context as _, ensure};
use serde_yaml::{Mapping, Value};

use super::Platform;
use super::analyse::{Analysis, ReleaseFacts};
use super::document::EnvironmentDocument;
use super::ensure::PrivateDirectory;
use crate::release_chart::{self, RoleName, ValuesInput};
use wamn_control_provision::WorkloadRoleFamily;

/// The host variable each environment credential Secret feeds, by family.
/// The Secret names derive from the coordinate (§9.1).
const CREDENTIAL_VARIABLES: [(WorkloadRoleFamily, &str); 6] = [
    (WorkloadRoleFamily::App, "WAMN_PG_URL"),
    (WorkloadRoleFamily::IdentityReader, "WAMN_SYSTEM_URL"),
    (
        WorkloadRoleFamily::ExecutorPlatform,
        "WAMN_EXECUTOR_PLATFORM_PG_URL",
    ),
    (
        WorkloadRoleFamily::HttpAdmitter,
        "WAMN_HTTP_ADMITTER_PG_URL",
    ),
    (
        WorkloadRoleFamily::EventMaterializer,
        "WAMN_EVENT_MATERIALIZER_PG_URL",
    ),
    (
        WorkloadRoleFamily::Administration,
        "WAMN_ADMINISTRATION_PG_URL",
    ),
];

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
/// When the chart is not the version this verb renders for, the values do
/// not render, or Helm fails. A Helm failure leaves the previous release
/// serving: `--rollback-on-failure` restores it.
pub async fn write(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
    release: &ReleaseFacts,
    reason: Option<&str>,
) -> anyhow::Result<Written> {
    let policy = &analysis.authorities.policy;
    let budget = Duration::from_secs(u64::try_from(policy.readiness_budget_seconds)?);
    let deadline = Instant::now() + budget;
    let shown: Value = serde_yaml::from_str(&helm_show_chart(&platform.chart).await?)
        .context("parse the chart's Chart.yaml")?;
    ensure!(
        shown["version"].as_str() == Some(release_chart::CHART_VERSION),
        "the chart {} is version {:?}; this verb renders for {}",
        platform.chart.display(),
        shown["version"],
        release_chart::CHART_VERSION
    );
    let roles = release_chart::roles(
        &release.manifest,
        &document.org,
        &document.project,
        &document.env,
    );
    let names = roles.iter().map(|role| role.name).collect();
    let rendered = release_chart::values(&ValuesInput {
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        manifest_digest: release.digest.clone(),
        artifact_base: platform.release_artifact_base.clone(),
        route_host: document.route_host.clone(),
        roles,
        drain_bound_seconds: u32::try_from(policy.drain_bound_seconds)?,
        actor: analysis.actor.clone(),
        host_group: host_group(platform, document)?,
    })?;
    let release_name = analysis.authorities.release_name.clone();
    let unchanged = analysis
        .authorities
        .revision
        .as_ref()
        .is_some_and(|revision| {
            revision.chart == format!("release-{}", release_chart::CHART_VERSION)
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
    release_chart::upgrade(
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
    .await?;
    Ok(Written {
        release_name,
        roles: names,
        deadline,
        helm_written: true,
    })
}

/// The host group body: the platform's part, read from
/// `WAMN_RELEASE_HOST_GROUP`, and the environment's credential Secret
/// references, which the verb derives from the coordinate.
fn host_group(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<Mapping> {
    let mut group = match &platform.host_group {
        None => Mapping::new(),
        Some(path) => serde_yaml::from_str(
            &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("{} is not a YAML mapping", path.display()))?,
    };
    let variables = group
        .entry("env".into())
        .or_insert_with(|| Value::Sequence(Vec::new()))
        .as_sequence_mut()
        .context("the platform host group's `env` is not a list")?;
    for (family, variable) in CREDENTIAL_VARIABLES {
        ensure!(
            !variables
                .iter()
                .any(|entry| entry["name"].as_str() == Some(variable)),
            "the platform host group sets {variable}, which apply derives from the coordinate"
        );
        let secret = wamn_control_provision::workload_secret_name(
            family,
            &document.org,
            &document.project,
            &document.env,
        );
        variables.push(serde_yaml::from_str(&format!(
            "{{name: {variable}, valueFrom: {{secretKeyRef: {{name: {secret}, key: url, optional: false}}}}}}"
        ))?);
    }
    Ok(group)
}

/// The values with `audit` removed: who applied is no reason to write.
fn without_audit(mut values: serde_json::Value) -> serde_json::Value {
    if let Some(values) = values.as_object_mut() {
        values.remove("audit");
    }
    values
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn audit_is_no_difference() {
        let written = json!({"release": {"manifestDigest": "sha256:a"}, "audit": {"actor": "a"}});
        let rendered = json!({"release": {"manifestDigest": "sha256:a"}, "audit": {"actor": "b"}});
        assert_eq!(without_audit(written.clone()), without_audit(rendered));
        let other = json!({"release": {"manifestDigest": "sha256:b"}, "audit": {"actor": "a"}});
        assert_ne!(without_audit(written), without_audit(other));
    }
}
