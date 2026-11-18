//! Authored settings for one environment, in project execution order.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context as _, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use wamn_catalog::ApplicationName;
use wamn_registry::{EnvName, OrgId, ProjectId, Triple};

use super::{ApplicationSelection, EnvironmentDocument, Integration, Schema};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentFile {
    #[schemars(with = "String")]
    pub org: OrgId,
    #[schemars(with = "String")]
    pub env: EnvName,
    #[schemars(regex(pattern = r"\S"))]
    pub target: String,
    #[schemars(with = "String")]
    pub policy: EnvName,
    #[schemars(range(max = 3600))]
    pub revocation_seconds: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, Integration>,
    #[schemars(length(min = 1))]
    pub projects: Vec<Project>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    #[schemars(with = "String")]
    pub name: ProjectId,
    #[serde(default)]
    pub schema: Schema,
    #[schemars(with = "BTreeMap<String, ApplicationSelection>")]
    pub applications: BTreeMap<ApplicationName, ApplicationSelection>,
}

impl EnvironmentFile {
    /// Read and validate an authored YAML environment.
    pub fn compile(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let file: Self = serde_saphyr::from_slice(&bytes)
            .with_context(|| format!("decode environment YAML {}", path.display()))?;
        file.require_valid()?;
        Ok(file)
    }

    /// Write the authored environment in YAML format.
    pub fn to_yaml(&self) -> anyhow::Result<String> {
        serde_saphyr::to_string(self).context("encode environment YAML")
    }

    /// Check cross-project and connection constraints before any provisioning.
    pub fn require_valid(&self) -> anyhow::Result<()> {
        ensure!(!self.projects.is_empty(), "environment has no projects");
        super::plan::require_settings(
            &self.target,
            Some(self.revocation_seconds),
            &self.connections,
        )?;
        let mut projects = BTreeSet::new();
        let mut applications = BTreeSet::new();
        for project in &self.projects {
            ensure!(
                projects.insert(&project.name),
                "duplicate project {}",
                project.name
            );
            wamn_provision::validate_project_env(&self.org, &project.name, &self.env)?;
            super::plan::require_applications(&project.applications, &self.connections)?;
            for name in project.applications.keys() {
                ensure!(applications.insert(name), "duplicate application {name}");
            }
        }
        Ok(())
    }

    pub fn triple(&self, project: &ProjectId) -> Triple {
        Triple::new(self.org.clone(), self.env.clone(), project.clone())
    }

    /// Resolve a declared application to its project.
    pub fn application_project(&self, application: &ApplicationName) -> anyhow::Result<&ProjectId> {
        self.projects
            .iter()
            .find(|project| project.applications.contains_key(application))
            .map(|project| &project.name)
            .with_context(|| format!("application {application} is not declared"))
    }

    /// Resolve a source-only command, which requires a single declared application.
    pub fn only_application(&self) -> anyhow::Result<&ApplicationName> {
        let mut applications = self
            .projects
            .iter()
            .flat_map(|project| project.applications.keys());
        let name = applications
            .next()
            .context("environment has no declared application")?;
        ensure!(
            applications.next().is_none(),
            "select an environment with one application for source publication"
        );
        Ok(name)
    }

    /// Take the named project and its shared settings.
    pub fn into_project(mut self, project: &ProjectId) -> anyhow::Result<EnvironmentDocument> {
        let index = self
            .projects
            .iter()
            .position(|item| &item.name == project)
            .with_context(|| format!("project {project} is not declared"))?;
        let project = self.projects.swap_remove(index);
        Ok(self.project_document(project))
    }

    /// Take the project containing the named application.
    pub fn into_application(
        self,
        application: &ApplicationName,
    ) -> anyhow::Result<EnvironmentDocument> {
        let project = self.application_project(application)?.clone();
        self.into_project(&project)
    }

    /// Project settings in the exact order authored in YAML.
    pub fn into_projects(mut self) -> impl Iterator<Item = EnvironmentDocument> {
        std::mem::take(&mut self.projects)
            .into_iter()
            .map(move |project| EnvironmentDocument {
                org: self.org.clone(),
                project: project.name,
                env: self.env.clone(),
                target: self.target.clone(),
                policy: self.policy.clone(),
                revocation_seconds: Some(self.revocation_seconds),
                schema: project.schema,
                applications: project.applications,
                integrations: self.connections.clone(),
            })
    }

    fn project_document(self, project: Project) -> EnvironmentDocument {
        EnvironmentDocument {
            org: self.org,
            project: project.name,
            env: self.env,
            target: self.target,
            policy: self.policy,
            revocation_seconds: Some(self.revocation_seconds),
            schema: project.schema,
            applications: project.applications,
            integrations: self.connections,
        }
    }
}

/// Generate the checked-in schema from the authored Serde type.
pub fn schema_json() -> serde_json::Result<String> {
    let mut schema = serde_json::to_string_pretty(&schemars::schema_for!(EnvironmentFile))?;
    schema.push('\n');
    Ok(schema)
}
