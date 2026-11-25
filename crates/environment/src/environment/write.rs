//! Prepare chart inputs, then apply one application's Helm release.
//! Helm handles pending operations and reports rollout failures.

pub(super) mod plan;

use std::path::Path;

use crate::application_chart::{binding::Binding, plan::ValuesInput};
use anyhow::Context as _;
use serde_yaml::{Mapping, Value};

use super::document::EnvironmentDocument;

/// Validate an operator authority and read its fixed socket port.
pub type AuthorityPort = fn(&str) -> Result<u16, wamn_connection::administration::ConnectionError>;

/// Build release values from the application selection and artifact repositories.
#[expect(
    clippy::too_many_arguments,
    reason = "release rendering needs separate target, artifact, and repository inputs"
)]
pub fn release_values(
    document: &EnvironmentDocument,
    platform_domain: &wamn_registry::platform_domain::PlatformDomain,
    application: &wamn_registry::ApplicationName,
    package_manifests: &std::collections::BTreeMap<
        wamn_package::PackageName,
        wamn_component_policy::PackageManifest,
    >,
    package_digest: &wamn_digest::Digest,
    bindings: &[Binding],
    integration_repository: &str,
    component_repository: &str,
    authority_port: AuthorityPort,
) -> anyhow::Result<Value> {
    plan::release_values(
        document,
        platform_domain,
        application,
        package_manifests,
        package_digest,
        bindings,
        integration_repository,
        component_repository,
        authority_port,
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
    Ok(group)
}
