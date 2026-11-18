//! Analysis of the environment authorities in
//! `docs/architecture/overview.md#environment-lifecycle`.
//!
//! Analysis compiles nothing new and writes nothing. It reads each authority
//! once (§5): the policy and the environment row in the system database, the
//! package closure from OCI, the installed state in the project database, the newest
//! successful Helm revision and the live set. It then tests the document
//! against them and returns the plan, or the refusals.

use sqlx::{Connection as _, Row as _};
pub(super) mod plan;
pub use plan::refusals;
pub(crate) use plan::{newest_successful, project_url};
use plan::replica_refusal;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use serde_json::Value;
use sqlx::PgConnection as Client;
use wamn_catalog::{PackageName, manifest::PackageManifest};
use wamn_postgres::connect;

use super::Platform;
use super::document::EnvironmentDocument;
use wamn_kubernetes::Target;

/// The policy facts `apply` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub name: wamn_registry::EnvName,
    pub readiness_budget_seconds: i32,
    pub drain_bound_seconds: i32,
}

/// The environment row of the system database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRow {
    pub disposable: bool,
    pub instance_suffix: wamn_registry::instance_suffix::InstanceSuffix,
    pub policy_name: wamn_registry::EnvName,
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

/// The newest successful revision of the release chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    pub number: u32,
    pub status: String,
    pub chart: String,
    pub values: Value,
    manifest_digest: Option<wamn_digest::Digest>,
}

impl Revision {
    /// The release digest the revision installed.
    pub fn manifest_digest(&self) -> Option<&wamn_digest::Digest> {
        self.manifest_digest.as_ref()
    }
    /// The web selection written with this release revision.
    pub fn client(&self) -> anyhow::Result<Option<super::document::Client>> {
        let host = self.values["routeHost"]
            .as_str()
            .context("Helm revision has no route host")?;
        match (
            self.values["webClients"][host].as_str(),
            self.values["webDigests"][host].as_str(),
        ) {
            (None, None) => Ok(None),
            (Some(package), Some(web)) => {
                let web = web.parse::<wamn_digest::Digest>()?;
                Ok(Some(super::document::Client {
                    package: package.parse()?,
                    web,
                }))
            }
            _ => anyhow::bail!("Helm revision has an incomplete web client selection"),
        }
    }

    fn pin(&self) -> anyhow::Result<(&wamn_digest::Digest, Option<super::document::Client>)> {
        let digest = self
            .manifest_digest()
            .context("Helm revision has no release digest")?;

        Ok((digest, self.client()?))
    }
}

/// Everything analysis read.
#[derive(Debug, Clone)]
pub struct Authorities {
    pub release_name: String,
    pub connections: BTreeMap<String, crate::org_connection::ResolvedConnection>,
    /// Application-qualified names resolved to published wasm artifacts.
    pub artifact_bindings: BTreeSet<String>,
    pub policy: Policy,
    pub row: Option<EnvironmentRow>,
    pub project: Option<ProjectState>,
    pub release: Option<ReleaseFacts>,
    pub revision: Option<Revision>,
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
        policy: document.policy.clone(),
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
                    .any(|selected| &selected.name == *name)
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

pub(crate) async fn read_policy(
    system: &mut Client,
    org: &wamn_registry::OrgId,
    name: &wamn_registry::EnvName,
) -> anyhow::Result<Policy> {
    let row =
        sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_READ_POLICY)
            .bind(org)
            .bind(name)
            .fetch_optional(&mut *system)
            .await
            .context("read the environment policy")?
            .with_context(|| format!("refused: org {org} has no environment policy {name}"))?;
    Ok(Policy {
        name: name.to_owned(),
        readiness_budget_seconds: row.get(0),
        drain_bound_seconds: row.get(1),
    })
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
                    instance_suffix: row.get(0),
                    policy_name: row.try_get(1)?,
                    disposable: row.get(2),
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

/// Run `helm` with the platform's kubeconfig and context.
pub(crate) async fn helm(
    platform: &Platform,
    arguments: &[&str],
) -> anyhow::Result<std::process::Output> {
    helm_in(&platform.target, arguments).await
}

/// Run `helm` with an explicit kubeconfig, context and namespace.
async fn helm_in(
    target: &Target,
    arguments: &[&str],
) -> anyhow::Result<std::process::Output> {
    let mut args = arguments
        .iter()
        .map(std::ffi::OsStr::new)
        .collect::<Vec<_>>();
    args.extend([
        std::ffi::OsStr::new("--kubeconfig"),
        target.kubeconfig.as_os_str(),
        std::ffi::OsStr::new("--kube-context"),
        std::ffi::OsStr::new(&target.context),
        std::ffi::OsStr::new("--namespace"),
        std::ffi::OsStr::new(&target.namespace),
    ]);
    wamn_kubernetes::helm::output(&args).await
}

/// The newest revision whose status is `deployed` or `superseded`, with its
/// values. A failed or pending revision is not a predecessor for anything.
pub(crate) async fn read_revision(
    platform: &Platform,
    release_name: &str,
) -> anyhow::Result<Option<Revision>> {
    read_revision_in(&platform.target, release_name).await
}

/// [`read_revision`] with an explicit kubeconfig, context and namespace.
async fn read_revision_in(
    target: &Target,
    release_name: &str,
) -> anyhow::Result<Option<Revision>> {
    let history = read_history(target, release_name).await?;
    match newest_successful(&history) {
        Some(entry) => read_history_revision(target, release_name, entry)
            .await
            .map(Some),
        None => Ok(None),
    }
}

/// Choose the rollback revision from Helm's history, including a failed head.
///
/// # Errors
/// Returns an error if history cannot be read or no successful rollback target exists.
pub async fn rollback_revision(
    platform: &Platform,
    release_name: &str,
) -> anyhow::Result<Revision> {
    let mut history = read_history(&platform.target, release_name).await?;
    history.sort_unstable_by_key(|entry| std::cmp::Reverse(entry["revision"].as_u64()));
    let mut revisions = Vec::new();
    for entry in &history {
        revisions.push(read_history_revision(&platform.target, release_name, entry).await?);
        if let Some(target) = rollback_target(&revisions)? {
            return Ok(target.clone());
        }
    }
    anyhow::bail!("application has no successful rollback target")
}

/// Select rollback from observed revisions. A failed head restores the latest success.
/// A successful head restores the preceding distinct release and web client pair.
///
/// # Errors
/// Refuses a successful revision with an invalid release or client selection.
pub fn rollback_target(history: &[Revision]) -> anyhow::Result<Option<&Revision>> {
    let Some(head) = history.iter().max_by_key(|revision| revision.number) else {
        return Ok(None);
    };
    let successful =
        |revision: &&Revision| matches!(revision.status.as_str(), "deployed" | "superseded");
    let candidates = history.iter().filter(successful);
    if !matches!(head.status.as_str(), "deployed" | "superseded") {
        let target = candidates.max_by_key(|revision| revision.number);
        if let Some(target) = target {
            target.pin()?;
        }
        return Ok(target);
    }
    let head_pin = head.pin()?;
    let mut target: Option<&Revision> = None;
    for revision in candidates {
        if revision.pin()? != head_pin
            && target.is_none_or(|target| target.number < revision.number)
        {
            target = Some(revision);
        }
    }
    Ok(target)
}

async fn read_history(
    target: &Target,
    release_name: &str,
) -> anyhow::Result<Vec<Value>> {
    let output = helm_in(
        target,
        &["history", release_name, "--max", "50", "-o", "json"],
    )
    .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            stderr.contains("not found"),
            "helm history {release_name} exited {}: {stderr}",
            output.status
        );
        return Ok(Vec::new());
    }
    serde_json::from_slice(&output.stdout).context("decode helm history")
}

async fn read_history_revision(
    target: &Target,
    release_name: &str,
    entry: &Value,
) -> anyhow::Result<Revision> {
    let number = entry["revision"]
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .context("helm history gave a revision that is not a number")?;
    let revision = number.to_string();
    let output = helm_in(
        target,
        &[
            "get",
            "values",
            release_name,
            "--revision",
            &revision,
            "-o",
            "json",
        ],
    )
    .await?;
    ensure!(
        output.status.success(),
        "helm get values {release_name} --revision {revision} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let values: Value = serde_json::from_slice(&output.stdout).context("decode helm values")?;
    let manifest_digest = values["release"]["manifestDigest"]
        .as_str()
        .map(str::parse)
        .transpose()?;
    Ok(Revision {
        manifest_digest,
        number,
        status: entry["status"]
            .as_str()
            .context("Helm revision has no status")?
            .to_owned(),
        chart: entry["chart"].as_str().unwrap_or_default().to_owned(),
        values,
    })
}

/// Discover application Helm releases in every status before status or environment deletion.
pub(crate) async fn application_releases(
    platform: &Platform,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<BTreeMap<wamn_catalog::ApplicationName, String>> {
    wamn_provision::validate_project_env(&triple.org, &triple.project, &triple.env)?;
    let coordinate = triple.to_string();
    let filter = wamn_registry::name::application_release_filter(&triple.env);
    let mut entries = Vec::new();
    loop {
        let offset = entries.len().to_string();
        let output = helm(
            platform,
            &[
                "list", "--filter", &filter, "--max", "256", "--offset", &offset, "-o", "json",
            ],
        )
        .await?;
        ensure!(
            output.status.success(),
            "helm list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let page: Vec<Value> = serde_json::from_slice(&output.stdout)?;
        let last = page.len() < 256;
        entries.extend(page);
        if last {
            break;
        }
    }
    let mut applications = BTreeMap::new();
    for entry in entries {
        let name = entry["name"].as_str().context("Helm release has no name")?;
        let output = helm(platform, &["get", "values", name, "-o", "json"]).await?;
        ensure!(
            output.status.success(),
            "helm get values {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values: Value = serde_json::from_slice(&output.stdout)?;
        if values["environment"].as_str() != Some(coordinate.as_str()) {
            continue;
        }
        let membership = super::show::plan::revision_applications(&values)?;
        ensure!(
            membership.len() == 1,
            "release {name} does not belong to one application"
        );
        let (application, _) = membership.into_iter().next().expect("one application");
        let expected = wamn_provision::environment_names::application_release_name(
            &triple.org,
            &triple.project,
            &triple.env,
            &application,
        )?;
        ensure!(
            expected == name,
            "application {application} has an unexpected Helm release {name}"
        );
        ensure!(
            applications.insert(application, name.to_owned()).is_none(),
            "application has multiple Helm releases"
        );
    }
    Ok(applications)
}


/// The host pods of the environment's host group, as `kubectl get pods -o json`.
pub(crate) async fn read_pods(platform: &Platform, release_name: &str) -> anyhow::Result<Value> {
    let selector = wamn_registry::name::host_selector(release_name);
    let output = wamn_kubernetes::kubectl::kubectl(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        &["get", "pods", "-l", &selector, "-o", "json"],
        None,
    )
    .await?;
    serde_json::from_str(&output).context("decode the pod list")
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
    use serde_json::json;

    #[test]
    fn an_exclusive_cloud_connection_requires_one_explicit_replica() {
        for replicas in [None, Some(0), Some(2)] {
            assert!(replica_refusal("meter", replicas).is_some());
        }
        assert!(replica_refusal("meter", Some(1)).is_none());
    }

    #[test]
    fn the_newest_successful_revision_skips_failed_and_pending() {
        let history = vec![
            json!({"revision": 1, "status": "superseded"}),
            json!({"revision": 2, "status": "deployed"}),
            json!({"revision": 3, "status": "failed"}),
            json!({"revision": 4, "status": "pending-upgrade"}),
        ];
        assert_eq!(newest_successful(&history).unwrap()["revision"], 2);
        assert!(newest_successful(&history[2..]).is_none());
    }

    #[test]
    fn rollback_uses_the_failed_head_and_the_web_selection() -> anyhow::Result<()> {
        let revision = |number, status: &str, code: u8, web: Option<u8>| Revision {
            manifest_digest: Some(
                format!("sha256:{}", code.to_string().repeat(64))
                    .parse()
                    .unwrap(),
            ),
            number,
            status: status.to_owned(),
            chart: String::new(),
            values: json!({"release":{"manifestDigest":format!("sha256:{code:064x}")},
                "routeHost":"app.example", "webClients": web.map(|_| json!({"app.example":"app"})).unwrap_or(json!({})),
                "webDigests":web.map(|web| json!({"app.example":format!("sha256:{web:064x}")})).unwrap_or(json!({}))}),
        };
        let mut history = vec![
            revision(1, "superseded", 1, Some(1)),
            revision(2, "deployed", 2, Some(2)),
            revision(3, "failed", 3, Some(3)),
        ];
        assert_eq!(
            rollback_target(&history)?.map(|target| target.number),
            Some(2)
        );
        history.pop();
        assert_eq!(
            rollback_target(&history)?.map(|target| target.number),
            Some(1)
        );
        history.push(revision(3, "deployed", 2, Some(4)));
        assert_eq!(
            rollback_target(&history)?.map(|target| target.number),
            Some(2)
        );
        history.push(revision(4, "deployed", 2, Some(4)));
        assert_eq!(
            rollback_target(&history)?.map(|target| target.number),
            Some(2)
        );
        history.push(revision(5, "failed", 2, Some(4)));
        assert_eq!(
            rollback_target(&history)?.map(|target| target.number),
            Some(4)
        );
        assert!(rollback_target(&[revision(1, "failed", 1, None)])?.is_none());
        Ok(())
    }
}
