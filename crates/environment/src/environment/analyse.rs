//! Analysis of the environment authorities in
//! `docs/architecture/overview.md#environment-lifecycle`.
//!
//! Analysis compiles nothing new and writes nothing. It reads each authority
//! once (§5): the environment row in the system database, the
//! package closure from OCI, the installed state in the project database, and
//! the live set. It then tests the document
//! against them and returns the plan, or the refusals.

use super::database::DatabaseSession;
pub(super) mod plan;
pub(crate) use plan::project_url;
pub use plan::refusals;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use serde_json::Value;
use wamn_component_policy::PackageManifest;
use wamn_package::PackageName;
use wamn_registry::EnvironmentRow;

use super::Platform;
use super::document::EnvironmentDocument;

/// One installed package in the project database.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Installed {
    /// The installed package baseline.
    pub current: wamn_package::package_version::PackageVersion,
}

/// What the project database holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub installed: BTreeMap<wamn_package::PackageName, Installed>,
    pub schema_report: Option<crate::migrations::SchemaReport>,
}

/// The release the document names, as OCI holds it.
#[derive(Debug, Clone)]
pub struct ReleaseFacts {
    pub digest: wamn_digest::Digest,
    pub packages: BTreeMap<PackageName, PackageManifest>,
}

#[derive(Debug, Clone)]
pub struct Authorities {
    pub release_name: String,
    pub connections: BTreeMap<String, wamn_application_chart::ResolvedConnection>,
    /// Application-qualified names resolved to published wasm artifacts.
    pub artifact_bindings: BTreeSet<String>,
    pub row: Option<EnvironmentRow>,
    pub project: Option<ProjectState>,
    pub release: Option<ReleaseFacts>,
    /// Loaded application and digest pairs observed from the environment hosts.
    pub live: BTreeMap<(wamn_registry::ApplicationName, wamn_digest::Digest), usize>,
}

/// The result of analysis: the authorities and the differences to apply.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub schema_mode: super::document::SchemaMode,
    pub authorities: Authorities,
    pub application: wamn_registry::ApplicationName,
    pub notices: Vec<Notice>,
    pub actor: String,
    pub plan: Vec<String>,
    pub packages: Vec<wamn_component_policy::PackageManifest>,
}

/// Evidence that does not prevent application activation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Notice {
    pub application: wamn_registry::ApplicationName,
    pub identity: String,
    pub observed: Observation,
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Observation {
    PlatformSchema {
        report: crate::migrations::SchemaReport,
    },
    SchemaDifference {
        installed: Option<String>,
        selected: String,
    },
}

/// Select one application's inputs while preserving shared configuration.
pub fn select_application(
    document: &EnvironmentDocument,
    application: Option<&wamn_registry::ApplicationName>,
) -> anyhow::Result<EnvironmentDocument> {
    let name = if let Some(name) = application {
        name
    } else {
        ensure!(
            document.applications.len() == 1,
            "name the application to analyse"
        );
        document
            .applications
            .keys()
            .next()
            .expect("one application")
    };
    let selection = document
        .applications
        .get(name)
        .with_context(|| format!("application {name} is not declared"))?;
    Ok(EnvironmentDocument {
        readiness_budget_seconds: document.readiness_budget_seconds,
        drain_bound_seconds: document.drain_bound_seconds,
        revocation_seconds: document.revocation_seconds,
        applications: BTreeMap::from([(name.to_owned(), selection.clone())]),
        settings: wamn_registry::configuration::EnvironmentSettings {
            org: document.settings.org.clone(),
            project: document.settings.project.clone(),
            env: document.settings.env.clone(),
            target: document.settings.target.clone(),
            schema: document.settings.schema,
            integrations: document
                .settings
                .integrations
                .iter()
                .filter(|(name, _)| {
                    selection
                        .connections
                        .values()
                        .any(|selected| selected.as_str() == *name)
                })
                .map(|(name, integration)| (name.clone(), integration.clone()))
                .collect(),
        },
    })
}

/// The targeted application's Helm release and host group identity.
pub fn application_release_name(
    document: &EnvironmentDocument,
    application: &wamn_registry::ApplicationName,
) -> anyhow::Result<String> {
    super::super::release_chart::release_name(
        &document.settings.org,
        &document.settings.project,
        &document.settings.env,
        application,
    )
}

/// Derive application release names from declarations and observed workloads.
pub(crate) async fn application_releases(
    platform: &Platform,
    system: &dyn DatabaseSession,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<BTreeMap<wamn_registry::ApplicationName, String>> {
    let mut applications = system.application_names(triple).await?;
    applications.extend(platform.cluster.applications(triple).await?);
    applications
        .into_iter()
        .map(|application| {
            let release = crate::release_chart::release_name(
                &triple.org,
                &triple.project,
                &triple.env,
                &application,
            )?;
            Ok((application, release))
        })
        .collect()
}

pub(crate) async fn read_pods(platform: &Platform, release_name: &str) -> anyhow::Result<Value> {
    platform.cluster.host_pods(release_name).await
}
