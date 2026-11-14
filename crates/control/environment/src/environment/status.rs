//! Authored selections and independently observed application and database state.

pub(super) mod plan;
use plan::terminating;

use std::collections::BTreeMap;

use anyhow::Context as _;
use serde::Serialize;
use serde_json::Value;
use wamn_kubernetes::environment_state::{environment_state, observe, release_digest};
use wamn_registry::Triple;

use super::Platform;
use super::analyse::read_revision;
use super::drain::{Stranded, stranded};
use super::ensure::{kubectl, project_url};
use wamn_postgres::connect;

pub use wamn_kubernetes::environment_state::DeploymentStatus;

/// The status of one environment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub applications: BTreeMap<String, ApplicationStatus>,
    /// Unfinished runs with no live host anywhere in this environment.
    pub stranded: Option<Vec<Stranded>>,
    pub installed: Option<BTreeMap<String, super::analyse::Installed>>,
    pub warnings: Vec<String>,
    pub schema: Option<super::sql::schema::Status>,
    pub system_schema: Option<crate::migrations::SchemaReport>,
    pub project_schema: Option<crate::migrations::SchemaReport>,
}

/// Independently observed state of one application's Helm release and host group.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApplicationStatus {
    pub current: super::document::DeclaredRelease,
    pub hostname: String,
    pub observed: Option<Observed>,
    pub warnings: Vec<String>,
}

/// Runtime state is absent when its authority cannot be read.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Observed {
    pub release_name: String,
    /// The newest successful Helm revision, or none.
    pub revision: Option<RevisionStatus>,
    /// The host Deployment, or none.
    pub deployment: Option<DeploymentStatus>,
    /// Each WorkloadDeployment of the release, with its conditions.
    pub workloads: BTreeMap<String, BTreeMap<String, String>>,
    /// Release digest to the count of host pods that are not terminated.
    pub live: BTreeMap<String, usize>,
    /// Release digest to the count of host pods that are terminating.
    pub terminating: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionStatus {
    pub number: u32,
    pub chart: String,
    pub manifest_digest: Option<String>,
}

/// Read canonical selections even when runtime or database state is unavailable.
pub async fn status(
    platform: &mut Platform,
    triple: &Triple,
    admin_url: &str,
) -> anyhow::Result<Status> {
    let document = super::document::EnvironmentDocument::read(platform, triple).await?;
    let triple = document.triple();
    let mut warnings = Vec::new();
    let system_schema = match async {
        let mut system = connect(&platform.system_database_url).await?;
        crate::platform_migrations::observe_system(&mut system).await
    }
    .await
    {
        Ok(report) => {
            warnings.extend(report.warnings.iter().cloned());
            Some(report)
        }
        Err(error) => {
            warnings.push(format!("system schema unknown: {error:#}"));
            None
        }
    };
    let project_schema = match async {
        let mut system = connect(&platform.system_database_url).await?;
        let Some(row) = super::analyse::read_row(&mut system, &document).await? else {
            return Ok::<_, anyhow::Error>(None);
        };
        let url = super::analyse::project_url(platform, &document, &row)?;
        let mut project = super::analyse::connect_project(&url).await?;
        crate::platform_migrations::observe_project(&mut project)
            .await
            .map(Some)
    }
    .await
    {
        Ok(report) => {
            if let Some(report) = &report {
                warnings.extend(report.warnings.iter().cloned());
            }
            report
        }
        Err(error) => {
            warnings.push(format!("project schema unknown: {error:#}"));
            None
        }
    };
    let schema = match super::sql::schema::status(platform, &document).await {
        Ok(schema) => Some(schema),
        Err(error) => {
            warnings.push(format!("schema unknown: {error:#}"));
            None
        }
    };
    let platform: &Platform = platform;
    let installed = schema.as_ref().map(|schema| {
        schema
            .installed
            .iter()
            .map(|(id, source)| {
                (
                    id.clone(),
                    super::analyse::Installed {
                        current: source.version.clone(),
                    },
                )
            })
            .collect()
    });
    let stranded = match async {
        let observed = wamn_kubernetes::environment_state::observe_environment(
            &platform.target.namespace,
            &triple.to_string(),
            |path| async move {
                let body = kubectl(
                    platform,
                    &platform.target.namespace,
                    &["get", "--raw", &path],
                    None,
                )
                .await?;
                serde_json::from_str(&body).context("read environment hosts")
            },
        )
        .await?;
        let live = wamn_kubernetes::environment_state::live_set(&observed.pods);
        stranded_runs(platform, &triple, &live, admin_url).await
    }
    .await
    {
        Ok(runs) => Some(runs),
        Err(error) => {
            warnings.push(format!("stranded runs unknown: {error:#}"));
            None
        }
    };
    let mut applications = BTreeMap::new();
    for (application, selection) in &document.applications {
        let mut warnings = Vec::new();
        let release_name = super::analyse::application_release_name(&document, application)?;
        let observed = async {
            let revision = read_revision(platform, &release_name)
                .await?
                .map(|revision| RevisionStatus {
                    manifest_digest: revision.manifest_digest().map(str::to_owned),
                    number: revision.number,
                    chart: revision.chart,
                });
            let namespace = platform.target.namespace.as_str();
            let observed = observe(namespace, &release_name, |path| async move {
                let body = kubectl(platform, namespace, &["get", "--raw", &path], None).await?;
                serde_json::from_str::<Value>(&body).with_context(|| format!("decode {path}"))
            })
            .await?;
            let state = environment_state(&observed, &release_name);
            Ok::<_, anyhow::Error>(Observed {
                terminating: terminating(&observed.pods),
                release_name,
                revision,
                deployment: state.deployment,
                workloads: state.workloads,
                live: state.live,
            })
        }
        .await;
        let observed = match observed {
            Ok(observed) => Some(observed),
            Err(error) => {
                warnings.push(format!("runtime unknown: {error:#}"));
                None
            }
        };
        applications.insert(
            application.clone(),
            ApplicationStatus {
                current: observed
                    .as_ref()
                    .and_then(|observed| observed.revision.as_ref())
                    .and_then(|revision| revision.manifest_digest.as_ref())
                    .map_or(super::document::DeclaredRelease::None, |digest| {
                        super::document::DeclaredRelease::Digest(digest.clone())
                    }),
                hostname: selection.hostname.clone(),
                observed,
                warnings,
            },
        );
    }
    Ok(Status {
        applications,
        stranded,
        installed,
        warnings,
        schema,
        system_schema,
        project_schema,
    })
}

/// Read pinned runs. An absent or unreadable database is unknown.
async fn stranded_runs(
    platform: &Platform,
    triple: &Triple,
    live: &BTreeMap<String, usize>,
    admin_url: &str,
) -> anyhow::Result<Vec<Stranded>> {
    // A lifecycle verb: the provisioning credential, as the install Jobs use.
    let mut system = connect(admin_url).await?;
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut system)
        .await
        .map(drop)
        .context("SET ROLE wamn_system")?;
    let present =
        sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_STATUS_STRANDED_RUNS)
            .bind(&triple.org)
            .bind(&triple.project)
            .bind(triple.env.as_str())
            .fetch_optional(&mut system)
            .await
            .context("read the environment row")?
            .is_some();
    if !present {
        anyhow::bail!("environment has no provisioned database");
    }
    let url = project_url(platform, triple).await?;
    let project_env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, triple.env.as_str());
    stranded(&url, &project_env_key, triple.env.as_str(), None, live).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_draining_pods_are_terminating() {
        let pods = json!({"items": [
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}, "deletionTimestamp": "t"}, "status": {"phase": "Running"}},
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Running"}}
        ]});
        assert_eq!(
            terminating(&pods),
            BTreeMap::from([("sha256:a".to_owned(), 1)])
        );
    }
}
