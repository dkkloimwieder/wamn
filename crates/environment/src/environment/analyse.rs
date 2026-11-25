//! Application selection, installed project state, and workload lookups.

use crate::environment_sql_effects::PostgresSession;
pub(super) mod plan;
pub(crate) use plan::project_url;

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde_json::Value;

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

/// Derive application release names from declarations and observed workloads.
pub(crate) async fn application_releases(
    platform: &Platform,
    system: &PostgresSession,
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
