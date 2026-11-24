//! Authored YAML environments and their applied project settings.

mod file;
pub(super) mod plan;

pub use file::{EnvironmentFile, Project, schema_json};

use crate::environment_sql_effects;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_kubernetes::Target;
use wamn_registry::EnvironmentApplications;
use wamn_registry::Triple;

pub use plan::{ApplicationSelection, EnvironmentDocument};

pub use plan::Client;

pub use plan::Integration;

pub use plan::DeclaredRelease;
pub use plan::{Schema, SchemaMode};

impl EnvironmentDocument {
    /// Read the project from a one-project environment file.
    pub fn compile(file: &Path) -> anyhow::Result<Self> {
        let document = EnvironmentFile::compile(file)?;
        ensure!(
            document.projects.len() == 1,
            "select a project from the environment"
        );
        Ok(document.into_projects().next().expect("one project"))
    }
}

impl EnvironmentDocument {
    /// Read applied configuration by coordinate from the registry.
    pub async fn read(platform: &super::Platform, triple: &Triple) -> anyhow::Result<Self> {
        let configuration = environment_sql_effects::read_environment_configuration(
            &platform.system_database_url,
            triple,
        )
        .await?
        .with_context(|| format!("environment {triple} has no applied configuration"))?;
        let document = Self::from_configuration(configuration);
        document.require_valid()?;
        Ok(document)
    }

    /// Applied settings exclude package membership, which comes from the selected release.
    pub fn configuration(&self) -> wamn_registry::configuration::EnvironmentConfiguration {
        use wamn_registry::configuration::{ApplicationConfiguration, EnvironmentConfiguration};
        EnvironmentConfiguration {
            settings: self.settings.clone(),
            readiness_budget_seconds: self.readiness_budget_seconds,
            drain_bound_seconds: self.drain_bound_seconds,
            applications: self
                .applications
                .iter()
                .map(|(name, application)| {
                    (
                        name.clone(),
                        ApplicationConfiguration {
                            label: application.label.clone(),
                            client: application.client.clone(),
                            connections: application.connections.clone(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn from_configuration(
        configuration: wamn_registry::configuration::EnvironmentConfiguration,
    ) -> Self {
        Self {
            settings: configuration.settings,
            readiness_budget_seconds: configuration.readiness_budget_seconds,
            drain_bound_seconds: configuration.drain_bound_seconds,
            revocation_seconds: None,
            applications: configuration
                .applications
                .into_iter()
                .map(|(name, application)| {
                    (
                        name,
                        ApplicationSelection {
                            label: application.label,
                            client: application.client,
                            connections: application.connections,
                            current: DeclaredRelease::None,
                            packages: std::collections::BTreeSet::new(),
                        },
                    )
                })
                .collect(),
        }
    }
}

/// The release metadata rendered on an application's WorkloadDeployment.
#[derive(Debug)]
pub struct WorkloadRelease {
    pub package_digest: wamn_digest::Digest,
    pub revision: u32,
}

/// Observe the current package and Helm revision through the workload reader.
pub async fn workload_release(
    cluster: &Target,
    triple: &Triple,
    application: &wamn_registry::ApplicationName,
) -> anyhow::Result<Option<WorkloadRelease>> {
    cluster
        .workload_annotations(triple, application)
        .await?
        .as_ref()
        .map(|annotations| release_annotations(Some(annotations)))
        .transpose()
}

pub(crate) fn release_annotations(
    annotations: Option<&BTreeMap<String, String>>,
) -> anyhow::Result<WorkloadRelease> {
    let annotation = |key| {
        annotations
            .and_then(|values| values.get(key))
            .map(String::as_str)
    };
    let package_digest = annotation("wamn.package-digest")
        .context("application workload has no package digest")?
        .parse()
        .context("invalid application workload package digest")?;
    let revision = annotation("wamn.release-revision")
        .context("application workload has no Helm revision")?
        .parse()
        .context("invalid application workload Helm revision")?;
    ensure!(
        revision > 0,
        "application workload Helm revision must be positive"
    );
    Ok(WorkloadRelease {
        package_digest,
        revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn configuration() -> Value {
        json!({"org":"acme","project":"wms","env":"prod","target":"cloud",
            "revocation_seconds":60,"schema":{"mode":"reset"},
            "applications":{"wms":{"label":"wms",
                "connections":{"erp":"shared-erp"},
                "client":{"package":"wamn_wms","web":format!("sha256:{:064x}",1)}}},
            "integrations":{"shared-erp":{"version":1}}})
    }

    #[test]
    fn omitted_schema_mode_uses_the_disposable_designation() -> anyhow::Result<()> {
        let read = EnvironmentDocument::from_json(&serde_json::to_vec(&configuration())?)?;
        for (disposable, expected) in [
            (Some(true), SchemaMode::Reset),
            (Some(false), SchemaMode::Migrate),
            (None, SchemaMode::Migrate),
        ] {
            let mut read = read.clone();
            read.settings.schema.mode = None;
            assert_eq!(read.schema_mode(disposable), expected);
        }
        Ok(())
    }
}
