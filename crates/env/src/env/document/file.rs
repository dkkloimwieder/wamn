//! Authored settings for one environment, in project execution order.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context as _, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use wamn_registry::AppName;
use wamn_registry::configuration::EnvSettings;
use wamn_registry::{EnvName, OrgId, ProjectId, Triple};

use super::plan::{default_drain_bound_seconds, default_readiness_budget_seconds};
use super::{AppSelection, EnvDocument, Integration, Schema};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvFile {
    #[schemars(with = "String")]
    pub org: OrgId,
    #[schemars(with = "String")]
    pub env: EnvName,
    #[schemars(regex(pattern = r"\S"))]
    pub target: String,
    #[serde(default = "default_readiness_budget_seconds")]
    #[schemars(range(min = 1, max = "i32::MAX"))]
    pub readiness_budget_seconds: i32,
    #[serde(default = "default_drain_bound_seconds")]
    #[schemars(range(min = 1, max = "i32::MAX"))]
    pub drain_bound_seconds: i32,
    #[schemars(range(max = 3600))]
    pub revocation_seconds: u32,
    pub database: Database,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, Integration>,
    #[schemars(length(min = 1))]
    pub projects: Vec<Project>,
}

/// Database sizing for every project in this environment.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Database {
    #[schemars(range(min = 1))]
    pub instances: u32,
    #[schemars(regex(pattern = r"\S"))]
    pub storage: String,
    #[schemars(regex(pattern = r"\S"))]
    pub cpu: String,
    #[schemars(regex(pattern = r"\S"))]
    pub memory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<Backup>,
}

/// Base backup schedule and recovery window.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    #[schemars(regex(pattern = r"\S"))]
    pub cadence: String,
    #[schemars(regex(pattern = r"^[1-9][0-9]*[dwm]$"))]
    pub retention: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    #[schemars(with = "String")]
    pub name: ProjectId,
    #[serde(default)]
    pub schema: Schema,
    #[schemars(with = "BTreeMap<String, AppSelection>")]
    pub apps: BTreeMap<AppName, AppSelection>,
}

impl EnvFile {
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
            self.readiness_budget_seconds,
            self.drain_bound_seconds,
            self.revocation_seconds,
            &self.connections,
        )?;
        let db = &self.database;
        ensure!(
            db.instances > 0
                && [&db.storage, &db.cpu, &db.memory]
                    .into_iter()
                    .all(|quantity| !quantity.trim().is_empty()),
            "database requires instances, storage, cpu, and memory"
        );
        if let Some(backup) = &db.backup {
            let valid_retention =
                backup
                    .retention
                    .as_bytes()
                    .split_last()
                    .is_some_and(|(unit, number)| {
                        matches!(unit, b'd' | b'w' | b'm')
                            && number
                                .first()
                                .is_some_and(|first| matches!(first, b'1'..=b'9'))
                            && number.iter().all(u8::is_ascii_digit)
                    });
            ensure!(
                !backup.cadence.trim().is_empty() && valid_retention,
                "database backup requires a cadence and a positive retention in days, weeks, or months"
            );
        }
        let mut projects = BTreeSet::new();
        let mut apps = BTreeSet::new();
        for project in &self.projects {
            ensure!(
                projects.insert(&project.name),
                "duplicate project {}",
                project.name
            );
            wamn_platform_install::env::validate_project_env(&self.org, &project.name, &self.env)?;
            super::plan::require_apps(&project.apps, &self.connections)?;
            for name in project.apps.keys() {
                ensure!(apps.insert(name), "duplicate application {name}");
            }
        }
        Ok(())
    }

    pub fn triple(&self, project: &ProjectId) -> Triple {
        Triple::new(self.org.clone(), self.env.clone(), project.clone())
    }

    /// Resolve a declared application to its project.
    pub fn app_project(&self, app: &AppName) -> anyhow::Result<&ProjectId> {
        self.projects
            .iter()
            .find(|project| project.apps.contains_key(app))
            .map(|project| &project.name)
            .with_context(|| format!("application {app} is not declared"))
    }

    /// Resolve a source-only command, which requires a single declared application.
    pub fn only_app(&self) -> anyhow::Result<&AppName> {
        let mut apps = self.projects.iter().flat_map(|project| project.apps.keys());
        let name = apps
            .next()
            .context("environment has no declared application")?;
        ensure!(
            apps.next().is_none(),
            "select an environment with one application for source publication"
        );
        Ok(name)
    }

    /// Take the named project and its shared settings.
    pub fn into_project(mut self, project: &ProjectId) -> anyhow::Result<EnvDocument> {
        let index = self
            .projects
            .iter()
            .position(|item| &item.name == project)
            .with_context(|| format!("project {project} is not declared"))?;
        let project = self.projects.swap_remove(index);
        Ok(self.project_document(project))
    }

    /// Take the project containing the named application.
    pub fn into_app(self, app: &AppName) -> anyhow::Result<EnvDocument> {
        let project = self.app_project(app)?.clone();
        self.into_project(&project)
    }

    /// Project settings in the exact order authored in YAML.
    pub fn into_projects(mut self) -> impl Iterator<Item = EnvDocument> {
        std::mem::take(&mut self.projects)
            .into_iter()
            .map(move |project| EnvDocument {
                settings: EnvSettings {
                    org: self.org.clone(),
                    project: project.name,
                    env: self.env.clone(),
                    target: self.target.clone(),
                    schema: project.schema,
                    integrations: self.connections.clone(),
                },
                readiness_budget_seconds: self.readiness_budget_seconds,
                drain_bound_seconds: self.drain_bound_seconds,
                revocation_seconds: self.revocation_seconds,
                apps: project.apps,
            })
    }

    fn project_document(self, project: Project) -> EnvDocument {
        EnvDocument {
            settings: EnvSettings {
                org: self.org,
                project: project.name,
                env: self.env,
                target: self.target,
                schema: project.schema,
                integrations: self.connections,
            },
            readiness_budget_seconds: self.readiness_budget_seconds,
            drain_bound_seconds: self.drain_bound_seconds,
            revocation_seconds: self.revocation_seconds,
            apps: project.apps,
        }
    }
}

/// Generate the checked-in schema from the authored Serde type.
pub fn schema_json() -> serde_json::Result<String> {
    let mut schema = serde_json::to_string_pretty(&schemars::schema_for!(EnvFile))?;
    schema.push('\n');
    Ok(schema)
}
