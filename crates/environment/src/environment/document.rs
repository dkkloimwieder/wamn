//! Authored YAML environments and their applied project settings.

mod file;
pub(super) mod plan;

pub use file::{EnvironmentFile, Project, schema_json};

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_catalog::EnvironmentApplications;
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

/// Read protected configuration as the provisioning component.
pub(super) async fn read_configuration(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<Option<wamn_catalog::configuration::EnvironmentConfiguration>, sqlx::Error> {
    let principal = wamn_provision::PlatformComponent::Provisioning.principal_id();
    let mut transaction =
        wamn_identity::transaction::read_connection(connection, &principal).await?;
    let configuration = wamn_catalog::configuration::read(&mut transaction, triple).await?;
    transaction.commit().await?;
    Ok(configuration)
}

impl EnvironmentDocument {
    /// Read applied configuration by coordinate from the registry.
    pub async fn read(platform: &super::Platform, triple: &Triple) -> anyhow::Result<Self> {
        let mut system = wamn_postgres::connect(&platform.system_database_url).await?;
        let configuration = read_configuration(&mut system, triple)
            .await?
            .with_context(|| format!("environment {triple} has no applied configuration"))?;
        let document = Self::from_configuration(configuration);
        document.require_valid()?;
        Ok(document)
    }

    pub(super) async fn save(&self, connection: &mut sqlx::PgConnection) -> anyhow::Result<()> {
        ensure!(
            wamn_catalog::configuration::write(connection, &self.configuration()).await?,
            "provision the environment before applying configuration"
        );
        Ok(())
    }

    /// Applied settings exclude package membership, which comes from the selected release.
    pub fn configuration(&self) -> wamn_catalog::configuration::EnvironmentConfiguration {
        use wamn_catalog::configuration::{ApplicationConfiguration, EnvironmentConfiguration};
        EnvironmentConfiguration {
            org: self.org.clone(),
            project: self.project.clone(),
            env: self.env.clone(),
            target: self.target.clone(),
            readiness_budget_seconds: self.readiness_budget_seconds,
            drain_bound_seconds: self.drain_bound_seconds,
            schema: self.schema,
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
            integrations: self.integrations.clone(),
        }
    }

    fn from_configuration(
        configuration: wamn_catalog::configuration::EnvironmentConfiguration,
    ) -> Self {
        Self {
            org: configuration.org,
            project: configuration.project,
            env: configuration.env,
            target: configuration.target,
            readiness_budget_seconds: configuration.readiness_budget_seconds,
            drain_bound_seconds: configuration.drain_bound_seconds,
            revocation_seconds: None,
            schema: configuration.schema,
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
            integrations: configuration.integrations,
        }
    }
}

/// The release metadata rendered on an application's WorkloadDeployment.
#[derive(Debug)]
pub struct WorkloadRelease {
    pub package_digest: wamn_digest::Digest,
    pub revision: u32,
}

/// Observe the current package and Helm revision through the existing workload reader.
pub async fn workload_release(
    api: &wamn_kubernetes::client::KubernetesApi,
    triple: &Triple,
    application: &wamn_catalog::ApplicationName,
) -> anyhow::Result<Option<WorkloadRelease>> {
    let release = wamn_catalog::resource_names::application_release_name(
        &triple.org,
        &triple.project,
        &triple.env,
        application,
    );
    let workloads = wamn_kubernetes::client::workloads(api.client(), triple.org.as_str());
    let workload = tokio::time::timeout(
        wamn_kubernetes::client::REQUEST_TIMEOUT,
        workloads.get_opt(&format!("{release}-http")),
    )
    .await??;
    workload
        .as_ref()
        .map(workload_release_annotations)
        .transpose()
}

/// Decode the live package digest and content revision from workload metadata.
pub fn workload_release_annotations(
    workload: &kube::api::DynamicObject,
) -> anyhow::Result<WorkloadRelease> {
    let annotations = workload.metadata.annotations.as_ref();
    let annotation = |key| annotations.and_then(|values| values.get(key)).map(String::as_str);
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
            read.schema.mode = None;
            assert_eq!(read.schema_mode(disposable), expected);
        }
        Ok(())
    }

}
