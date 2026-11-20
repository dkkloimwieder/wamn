//! Analysis of the environment authorities in
//! `docs/architecture/overview.md#environment-lifecycle`.
//!
//! Analysis compiles nothing new and writes nothing. It reads each authority
//! once (§5): the environment row in the system database, the
//! package closure from OCI, the installed state in the project database, and
//! the live set. It then tests the document
//! against them and returns the plan, or the refusals.

use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::ListParams};
use sqlx::{Connection as _, Row as _};
use wamn_kubernetes::client::workloads;
pub(super) mod plan;
pub use plan::refusals;
pub(crate) use plan::project_url;
use plan::replica_refusal;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use serde_json::Value;
use sqlx::PgConnection as Client;
use wamn_catalog::{PackageName, lifecycle_lock::LifecycleLock, manifest::PackageManifest};
use wamn_postgres::connect;

use super::Platform;
use super::document::EnvironmentDocument;

/// The environment row of the system database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRow {
    pub disposable: bool,
    pub instance_suffix: wamn_registry::instance_suffix::InstanceSuffix,
}

/// One installed package in the project database.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Installed {
    /// The installed package baseline.
    pub current: wamn_catalog::package_version::PackageVersion,
}

/// What the project database holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub installed: BTreeMap<wamn_catalog::PackageName, Installed>,
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
    pub connections: BTreeMap<String, crate::org_connection::ResolvedConnection>,
    /// Application-qualified names resolved to published wasm artifacts.
    pub artifact_bindings: BTreeSet<String>,
    pub row: Option<EnvironmentRow>,
    pub project: Option<ProjectState>,
    pub release: Option<ReleaseFacts>,
    /// Loaded application and digest pairs observed from the environment hosts.
    pub live: BTreeMap<(wamn_catalog::ApplicationName, wamn_digest::Digest), usize>,
}

/// The result of analysis: the authorities and the differences to apply.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub schema_mode: super::document::SchemaMode,
    pub authorities: Authorities,
    pub application: wamn_catalog::ApplicationName,
    pub notices: Vec<Notice>,
    pub actor: String,
    pub plan: Vec<String>,
    pub packages: Vec<wamn_catalog::manifest::PackageManifest>,
}

/// Evidence that does not prevent application activation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Notice {
    pub application: wamn_catalog::ApplicationName,
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
    application: Option<&wamn_catalog::ApplicationName>,
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
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        target: document.target.clone(),
        readiness_budget_seconds: document.readiness_budget_seconds,
        drain_bound_seconds: document.drain_bound_seconds,
        revocation_seconds: document.revocation_seconds,
        schema: document.schema,
        applications: BTreeMap::from([(name.to_owned(), selection.clone())]),
        integrations: document
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
    })
}




pub(crate) async fn connect_project(url: &str) -> anyhow::Result<Client> {
    let mut client = connect(url).await?;
    let owner: String = sqlx::query(
        "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()",
    )
    .fetch_one(&mut client)
    .await?
    .get(0);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET ROLE {}",
        postgres_protocol::escape::escape_identifier(&owner)
    )))
    .execute(&mut client)
    .await
    .map(drop)?;
    Ok(client)
}

pub(crate) async fn read_row(
    system: &mut Client,
    document: &EnvironmentDocument,
) -> anyhow::Result<Option<EnvironmentRow>> {
    Ok(
        sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_READ_ROW)
            .bind(&document.org)
            .bind(&document.project)
            .bind(&document.env)
            .fetch_optional(&mut *system)
            .await
            .context("read the environment row")?
            .map(|row| {
                Ok::<_, sqlx::Error>(EnvironmentRow {
                    instance_suffix: row.try_get(0)?,
                    disposable: row.try_get(1)?,
                })
            })
            .transpose()?,
    )
}

/// Read installed SQL as the project database owner; deploy facts live in Helm values.
/// Only an absent database returns `None`; unreadable state returns its cause.
pub async fn read_project(url: &str) -> anyhow::Result<Option<ProjectState>> {
    let mut client = match connect_project(url).await {
        Ok(client) => client,
        Err(error)
            if error
                .downcast_ref::<sqlx::Error>()
                .and_then(|error| error.as_database_error())
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref()
                == Some("3D000") =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.context("project database state is unknown")),
    };
    let mut transaction = client.begin().await.context("begin the project read")?;
    let mut installed = BTreeMap::<wamn_catalog::PackageName, Installed>::new();
    for row in sqlx::query(
        "SELECT package_id, package_version FROM catalog.packages",
    )
    .fetch_all(&mut *transaction)
    .await
    .context("read the installed packages")?
    {
        installed.insert(
            row.try_get(0)?,
            Installed {
                current: row.get(1),
            },
        );
    }
    let schema_report = Some(crate::platform_migrations::observe_project(&mut transaction).await?);
    transaction.commit().await.context("end the project read")?;
    Ok(Some(ProjectState {
        installed,
        schema_report,
    }))
}



/// The targeted application's Helm release and host group identity.
pub fn application_release_name(
    document: &EnvironmentDocument,
    application: &wamn_catalog::ApplicationName,
) -> anyhow::Result<String> {
    Ok(
        wamn_provision::environment_names::application_release_name(
            &document.org,
            &document.project,
            &document.env,
            application,
        )?,
    )
}

/// Derive application release names from declarations and observed workloads.
pub(crate) async fn application_releases(
    platform: &Platform,
    system: &LifecycleLock,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<BTreeMap<wamn_catalog::ApplicationName, String>> {
    let mut client = system.client().await;
    let mut tx = crate::provision_project_env::provisioning_transaction(&mut client).await?;
    let mut applications: BTreeSet<wamn_catalog::ApplicationName> = sqlx::query_scalar::<_, wamn_catalog::ApplicationName>(
        "SELECT application FROM registry.environment_applications WHERE org=$1 AND env=$2 AND project=$3",
    )
    .bind(&triple.org).bind(&triple.env).bind(&triple.project)
    .fetch_all(&mut *tx).await?.into_iter().collect();
    tx.commit().await?;
    drop(client);
    let coordinate = triple.to_string();
    let workloads = workloads(platform.target.client.client(), triple.org.as_str());
    for item in workloads.list(&ListParams::default()).await?.items {
        let annotations = item.metadata.annotations.as_ref();
        let annotation = |key| {
            annotations
                .and_then(|values| values.get(key))
                .map(String::as_str)
        };
        if annotation("wamn.environment") == Some(coordinate.as_str()) {
            applications.insert(
                annotation("wamn.application")
                    .context("workload has no application annotation")?
                    .parse()?,
            );
        }
    }
    applications.into_iter().map(|application| {
        let release = wamn_provision::environment_names::application_release_name(
            &triple.org, &triple.project, &triple.env, &application,
        )?;
        Ok((application, release))
    }).collect()
}

/// List the host Pods for the specified host group.
pub(crate) async fn read_pods(platform: &Platform, release_name: &str) -> anyhow::Result<Value> {
    let selector = wamn_registry::name::host_selector(release_name);
    let pods = Api::<Pod>::namespaced(platform.target.client.client(), &platform.target.namespace);
    let objects = pods.list(&ListParams::default().labels(&selector)).await?;
    serde_json::to_value(objects).context("encode the pod list")
}

/// Refuse cloud exclusive bindings unless the supplied host group names one replica.
///
/// # Errors
///
/// When the target or the host group body cannot be read.
pub async fn replica_refusals(
    system: &mut sqlx::PgConnection,
    document: &EnvironmentDocument,
    bindings: &[super::bindings::Binding],
    path: Option<&std::path::Path>,
) -> anyhow::Result<Vec<String>> {
    if !bindings.iter().any(|binding| binding.exclusive) {
        return Ok(Vec::new());
    }
    let cloud: bool = sqlx::query(
        wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_REPLICA_REFUSALS,
    )
    .bind(&document.target)
    .fetch_one(&mut *system)
    .await
    .context("read the environment target type")?
    .get(0);
    if !cloud {
        return Ok(Vec::new());
    }
    let group = super::write::host_group(path)?;
    let replicas = group.get("replicas").and_then(serde_yaml::Value::as_u64);
    Ok(bindings
        .iter()
        .filter(|binding| binding.exclusive)
        .filter_map(|binding| {
            let connection =
                &document.applications[&binding.application].connections[&binding.alias];
            replica_refusal(&connection.to_string(), replicas)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exclusive_cloud_connection_requires_one_explicit_replica() {
        for replicas in [None, Some(0), Some(2)] {
            assert!(replica_refusal("meter", replicas).is_some());
        }
        assert!(replica_refusal("meter", Some(1)).is_none());
    }

}
