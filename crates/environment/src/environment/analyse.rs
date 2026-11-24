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
use wamn_postgres::LifecycleLock;
pub(super) mod plan;
pub(crate) use plan::project_url;
pub use plan::refusals;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use serde_json::Value;
use sqlx::PgConnection as Client;
use wamn_component_policy::PackageManifest;
use wamn_package::PackageName;
use wamn_postgres::connect;
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
            .bind(document.settings.org.as_str())
            .bind(document.settings.project.as_str())
            .bind(document.settings.env.as_str())
            .fetch_optional(&mut *system)
            .await
            .context("read the environment row")?
            .map(|row| {
                Ok::<_, sqlx::Error>(EnvironmentRow {
                    instance_suffix: wamn_postgres::decode_text(row.try_get(0)?, 0)?,
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
    let mut installed = BTreeMap::<wamn_package::PackageName, Installed>::new();
    for row in sqlx::query("SELECT package_id, package_version FROM catalog.packages")
        .fetch_all(&mut *transaction)
        .await
        .context("read the installed packages")?
    {
        installed.insert(
            row.try_get::<String, _>(0)?
                .try_into()
                .context("decode package_id")?,
            Installed {
                current: row
                    .try_get::<String, _>(1)?
                    .try_into()
                    .context("decode package_version")?,
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
    application: &wamn_registry::ApplicationName,
) -> anyhow::Result<String> {
    Ok(wamn_provision::environment_names::application_release_name(
        &document.settings.org,
        &document.settings.project,
        &document.settings.env,
        application,
    )?)
}

/// Derive application release names from declarations and observed workloads.
pub(crate) async fn application_releases(
    platform: &Platform,
    system: &LifecycleLock,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<BTreeMap<wamn_registry::ApplicationName, String>> {
    let mut client = system.client().await;
    let mut tx = crate::provision_project_env::provisioning_transaction(&mut client).await?;
    let mut applications: BTreeSet<wamn_registry::ApplicationName> = sqlx::query_scalar::<_, String>(
        "SELECT application FROM registry.environment_applications WHERE org=$1 AND env=$2 AND project=$3",
    )
    .bind(triple.org.as_str()).bind(triple.env.as_str()).bind(triple.project.as_str())
    .fetch_all(&mut *tx).await?.into_iter().map(|name| wamn_postgres::decode_text(name, 0)).collect::<Result<_, _>>()?;
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
    applications
        .into_iter()
        .map(|application| {
            let release = wamn_provision::environment_names::application_release_name(
                &triple.org,
                &triple.project,
                &triple.env,
                &application,
            )?;
            Ok((application, release))
        })
        .collect()
}

/// List the host Pods for the specified host group.
pub(crate) async fn read_pods(platform: &Platform, release_name: &str) -> anyhow::Result<Value> {
    let selector = wamn_registry::name::host_selector(release_name);
    let pods = Api::<Pod>::namespaced(platform.target.client.client(), &platform.target.namespace);
    let objects = pods.list(&ListParams::default().labels(&selector)).await?;
    serde_json::to_value(objects).context("encode the pod list")
}
