//! Authored YAML environments and their applied project settings.

mod file;
pub(super) mod plan;

pub use file::{EnvFile, Project, schema_json};

use crate::env_sql_effects;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_kubernetes::Target;
use wamn_registry::EnvApps;
use wamn_registry::Triple;

pub use plan::{AppSelection, EnvDocument};

pub use plan::Client;

pub use plan::Integration;

pub use plan::DeclaredRelease;
pub use plan::{Schema, SchemaMode};

impl EnvDocument {
    /// Read the project from a one-project environment file.
    pub fn compile(file: &Path) -> anyhow::Result<Self> {
        let document = EnvFile::compile(file)?;
        ensure!(
            document.projects.len() == 1,
            "select a project from the environment"
        );
        Ok(document.into_projects().next().expect("one project"))
    }
}

impl EnvDocument {
    /// Read applied configuration by coordinate from the registry.
    pub async fn read(platform: &super::Platform, triple: &Triple) -> anyhow::Result<Self> {
        let configuration =
            env_sql_effects::read_env_configuration(&platform.system_database_url, triple)
                .await?
                .with_context(|| format!("environment {triple} has no applied configuration"))?;
        let document = Self::from_configuration(configuration);
        document.require_valid()?;
        Ok(document)
    }

    /// Applied settings exclude package membership, which comes from the selected release.
    pub fn configuration(&self) -> wamn_registry::configuration::EnvConfiguration {
        use wamn_registry::configuration::{AppConfiguration, EnvConfiguration};
        EnvConfiguration {
            settings: self.settings.clone(),
            readiness_budget_seconds: self.readiness_budget_seconds,
            drain_bound_seconds: self.drain_bound_seconds,
            revocation_seconds: self.revocation_seconds,
            applications: self
                .applications
                .iter()
                .map(|(name, app)| {
                    (
                        name.clone(),
                        AppConfiguration {
                            label: app.label.clone(),
                            client: app.client.clone(),
                            connections: app.connections.clone(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn from_configuration(configuration: wamn_registry::configuration::EnvConfiguration) -> Self {
        Self {
            settings: configuration.settings,
            readiness_budget_seconds: configuration.readiness_budget_seconds,
            drain_bound_seconds: configuration.drain_bound_seconds,
            revocation_seconds: configuration.revocation_seconds,
            applications: configuration
                .applications
                .into_iter()
                .map(|(name, app)| {
                    (
                        name,
                        AppSelection {
                            label: app.label,
                            client: app.client,
                            connections: app.connections,
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
    app: &wamn_registry::AppName,
) -> anyhow::Result<Option<WorkloadRelease>> {
    cluster
        .workload_annotations(triple, app)
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
    fn applied_document_preserves_a_disabled_grant_cache() -> anyhow::Result<()> {
        let mut input = configuration();
        input["revocation_seconds"] = 0.into();
        let document = EnvDocument::from_json(&serde_json::to_vec(&input)?)?;
        let applied = EnvDocument::from_configuration(document.configuration());
        assert_eq!(applied.revocation_seconds, 0);
        Ok(())
    }

    #[test]
    fn omitted_schema_mode_uses_the_disposable_designation() -> anyhow::Result<()> {
        let read = EnvDocument::from_json(&serde_json::to_vec(&configuration())?)?;
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
