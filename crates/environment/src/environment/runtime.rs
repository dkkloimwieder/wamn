//! Read host runtime inputs from the installed platform.

use std::path::Path;

use anyhow::Context as _;
use serde_json::Value;

use super::Platform;
use crate::platform::{artifact_plan, resources};

/// Read platform inputs before an environment or application exists.
pub async fn resolve_platform(
    platform: &mut Platform,
    org: &wamn_registry::OrgId,
) -> anyhow::Result<Value> {
    crate::platform::edge::require_running(&platform.target).await?;
    let installed = crate::platform::installed::InstalledPlatform::read(&platform.target)
        .await?
        .context("platform is not installed")?;
    let reference = installed.values["platform"]["chart"]
        .as_str()
        .context("installed platform has no selected chart")?;
    let chart = if platform.chart.as_os_str().is_empty() {
        artifact_plan::select(Path::new(reference))?
    } else {
        artifact_plan::select(&platform.chart)?
    };
    chart.directory().clone_into(&mut platform.chart);
    if chart.is_downloaded() {
        platform.chart_download = Some(std::sync::Arc::new(chart));
    }
    platform.target.namespace = org.to_string();
    platform.database_namespace = org.to_string();
    apply_registry_configuration(platform, &installed.values["configuration"])?;
    Ok(installed.values)
}

fn apply_registry_configuration(
    platform: &mut Platform,
    configuration: &Value,
) -> anyhow::Result<()> {
    platform.insecure_registry = configuration["insecure_registry"]
        .as_bool()
        .context("registry transport is absent")?;
    platform.package_artifact_base = Some(
        configuration["package_base"]
            .as_str()
            .context("package registry is absent")?
            .into(),
    );
    if let Some(base) = &platform.package_artifact_base {
        platform.package_artifact_base =
            Some(pod_artifact_base(platform.insecure_registry, base)?.into_owned());
    }
    Ok(())
}

pub(super) fn pod_artifact_base(
    insecure_registry: bool,
    base: &str,
) -> anyhow::Result<std::borrow::Cow<'_, str>> {
    let (registry, repository) = base.split_once('/').context("artifact repository")?;
    let pod_registry = pod_registry(insecure_registry, registry);
    if pod_registry == registry {
        Ok(base.into())
    } else {
        Ok(format!("{pod_registry}/{repository}").into())
    }
}

pub(super) fn pod_registry(insecure_registry: bool, registry: &str) -> &str {
    if insecure_registry && registry == resources::LOCAL_REGISTRY {
        resources::POD_REGISTRY
    } else {
        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_known_local_registry_is_rewritten_for_pods() {
        for (registry, expected) in [
            (resources::LOCAL_REGISTRY, resources::POD_REGISTRY),
            (resources::POD_REGISTRY, resources::POD_REGISTRY),
            ("registry.example:5001", "registry.example:5001"),
            ("localhost:5002", "localhost:5002"),
            ("127.0.0.1:5001", "127.0.0.1:5001"),
        ] {
            let base = format!("{registry}/wamn/components");
            assert_eq!(pod_artifact_base(false, &base).unwrap(), base);
            assert_eq!(
                pod_artifact_base(true, &base).unwrap(),
                format!("{expected}/wamn/components")
            );
        }
    }
}
