//! Analysis of the environment authorities in
//! `docs/architecture/overview.md#environment-lifecycle`.
//!
//! Analysis compiles nothing new and writes nothing. It reads each authority
//! once (§5): the policy and the environment row in the system database, the
//! release manifest from OCI, the installed state in the project database, the newest
//! successful Helm revision and the live set. It then tests the document
//! against them and returns the plan, or the refusals.

use sqlx::{Connection as _, Row as _};
pub(super) mod plan;
pub(crate) use plan::notices;
pub use plan::refusals;
pub(crate) use plan::{newest_successful, project_url};
use plan::{plan, replica_refusal, sequence_refusals};

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use sqlx::PgConnection as Client;
use wamn_catalog::ServingManifest;
use wamn_package_artifact::release_manifest_source::ReleaseManifestSource;
use wamn_postgres::connect;

use super::Platform;
use super::document::{DeclaredRelease, EnvironmentDocument};
use wamn_release_control::release_chart;

/// The policy facts `apply` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub name: String,
    pub readiness_budget_seconds: i32,
    pub drain_bound_seconds: i32,
}

/// The environment row of the system database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentRow {
    pub disposable: bool,
    pub instance_suffix: String,
    pub policy_name: String,
}

/// One installed package in the project database.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Installed {
    /// The installed package baseline.
    pub current: String,
}

/// What the project database holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub installed: BTreeMap<String, Installed>,
    pub schema_report: Option<crate::migrations::SchemaReport>,
    /// `(manifest_digest, component_digest, store_alias, instance_id)`.
    pub bindings: BTreeSet<(String, String, String, String)>,
}

/// The release the document names, as OCI holds it.
#[derive(Debug, Clone)]
pub struct ReleaseFacts {
    pub digest: String,
    pub manifest: ServingManifest,
    /// The store aliases the release's components require.
    pub required_connections: BTreeSet<String>,
}

/// The newest successful revision of the release chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    pub number: u32,
    pub status: String,
    pub chart: String,
    pub values: Value,
}

impl Revision {
    /// The release digest the revision installed.
    pub fn manifest_digest(&self) -> Option<&str> {
        self.values["release"]["manifestDigest"].as_str()
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
                wamn_package_artifact::ensure_digest(web)?;
                Ok(Some(super::document::Client {
                    package: package.to_owned(),
                    web: web.to_owned(),
                }))
            }
            _ => anyhow::bail!("Helm revision has an incomplete web client selection"),
        }
    }

    fn pin(&self) -> anyhow::Result<(&str, Option<super::document::Client>)> {
        let digest = self
            .manifest_digest()
            .context("Helm revision has no release digest")?;
        wamn_package_artifact::ensure_digest(digest)?;
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
    pub live: BTreeMap<(String, String), usize>,
}

/// The result of analysis: the authorities and the differences to apply.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub schema_mode: super::document::SchemaMode,
    pub authorities: Authorities,
    pub application: String,
    pub notices: Vec<Notice>,
    pub actor: String,
    pub plan: Vec<String>,
    pub packages: Vec<wamn_compiler::manifest::PackageManifest>,
}

/// Evidence that does not prevent application activation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Notice {
    pub application: String,
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
    application: Option<&str>,
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
                    .any(|selected| selected == *name)
            })
            .map(|(name, integration)| (name.clone(), integration.clone()))
            .collect(),
    })
}

/// Read the targeted application's authorities without writing control or application state.
///
/// # Errors
///
/// When an authority cannot be read, or when the document is refused. A
/// refusal names each reason.
pub async fn analyse(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: Option<&str>,
) -> anyhow::Result<Analysis> {
    let mut selected = select_application(document, application)?;
    let document = &selected;
    let application = document
        .applications
        .keys()
        .next()
        .expect("one application")
        .clone();
    let release_name = application_release_name(document, &application)?;
    let mut system = connect(&platform.system_database_url).await?;
    let policy = read_policy(&mut system, &document.org, &document.policy).await?;
    let row = read_row(&mut system, document).await?;
    let project = match &row {
        Some(row) => {
            read_project(
                &project_url(platform, document, row).await?,
                &wamn_control_provision::project_env_key(
                    &document.org,
                    &document.project,
                    &document.env,
                ),
            )
            .await?
        }
        None => None,
    };
    let release = match &document.applications[&application].current {
        DeclaredRelease::None => None,
        DeclaredRelease::Digest(declared) => Some(read_release(platform, declared).await?),
    };
    if let Some(release) = &release {
        selected
            .applications
            .get_mut(&application)
            .expect("application was selected")
            .packages = release
            .manifest
            .release
            .packages
            .iter()
            .map(|package| package.package_id().to_owned())
            .collect();
    }
    let document = &selected;
    if release.is_some() {
        super::clients::analyse(
            platform,
            &document.applications[&application],
            release.as_ref(),
        )
        .await?;
    }
    let revision = read_revision(platform, &release_name).await?;
    let live = super::drain::loaded(&super::drain::observe(platform, &document.triple()).await?);
    let packages = match &release {
        Some(release) => read_release_packages(platform, &release.manifest).await?,
        None => Vec::new(),
    };
    let bindings = super::bindings::analyse(&mut system, document, &packages).await?;
    let replica_refusals = replica_refusals(
        &mut system,
        document,
        &bindings,
        platform.host_group.as_deref(),
    )
    .await?;
    let connections = bindings
        .iter()
        .filter_map(|binding| {
            binding.resolved.clone().map(|resolved| {
                (
                    super::bindings::key(&binding.application, &binding.alias),
                    resolved,
                )
            })
        })
        .collect();
    let authorities = Authorities {
        release_name,
        artifact_bindings: bindings
            .iter()
            .filter(|binding| binding.resolved.is_none())
            .map(|binding| super::bindings::key(&binding.application, &binding.alias))
            .collect(),
        connections,
        policy,
        row,
        project,
        release,
        revision,
        live,
    };
    let actor = actor();
    let mut refusals = refusals(document, &authorities);
    refusals.extend(replica_refusals);
    if refusals.is_empty() {
        refusals = sequence_refusals(&document.membership(), &packages);
    }
    if !refusals.is_empty() {
        bail!("refused:\n- {}", refusals.join("\n- "));
    }
    let plan = plan(document, &authorities);
    let mut notices = notices(document, &application, &authorities);
    let report = crate::platform_migrations::observe_system(&mut system).await?;
    if !report.warnings.is_empty() {
        notices.push(Notice {
            application: application.clone(),
            identity: format!(
                "{}/registry._sqlx_migrations",
                wamn_postgres::identity(&platform.system_database_url)?.1
            ),
            observed: Observation::PlatformSchema { report },
            next_action:
                "Inspect system SQL with wamn platform schema diff --target system --chart DIR"
                    .into(),
        });
    }
    Ok(Analysis {
        schema_mode: document.schema_mode(authorities.row.as_ref().map(|row| row.disposable)),
        authorities,
        application: application.clone(),
        notices,
        actor,
        plan,
        packages,
    })
}

/// The package manifests of the release, read from their verified artifacts
/// for their sequence defaults.
pub(super) async fn read_release_packages(
    platform: &Platform,
    manifest: &ServingManifest,
) -> anyhow::Result<Vec<wamn_compiler::manifest::PackageManifest>> {
    let mut packages = Vec::new();
    for package in &manifest.release.packages {
        let (id, version) = (package.package_id(), package.package_version());
        let opened = super::stage::open(platform, id, version).await?;
        let path = wamn_compiler::package_manifest_path(opened.root());
        let package_manifest = wamn_compiler::manifest::PackageManifest::from_slice(
            &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        packages.push(package_manifest);
    }
    Ok(packages)
}

/// The actor: the identity principal the CLI authenticates as, or `$USER`
/// when it has none (owner ruling 2026-10-09). `wamn-ctl` has none today.
fn actor() -> String {
    std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned())
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
    org: &str,
    name: &str,
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
            .map(|row| EnvironmentRow {
                instance_suffix: row.get(0),
                policy_name: row.get(1),
                disposable: row.get(2),
            }),
    )
}

/// Read installed SQL and release bindings as the project database owner.
/// Only an absent database returns `None`; unreadable state returns its cause.
pub async fn read_project(
    url: &str,
    project_env_key: &str,
) -> anyhow::Result<Option<ProjectState>> {
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
    sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
        .bind(project_env_key)
        .fetch_one(&mut *transaction)
        .await
        .context("bind the project identity")?;
    let mut installed = BTreeMap::<String, Installed>::new();
    for row in sqlx::query(
        "SELECT package_id, package_version FROM catalog.packages \
              WHERE project_env_key = $1",
    )
    .bind(project_env_key)
    .fetch_all(&mut *transaction)
    .await
    .context("read the installed packages")?
    {
        installed.insert(
            row.get(0),
            Installed {
                current: row.get(1),
            },
        );
    }
    let bindings = sqlx::query(
        "SELECT manifest_digest, component_digest, name, connection_id::text \
               FROM catalog.connection_binding WHERE project_env_key = $1",
    )
    .bind(project_env_key)
    .fetch_all(&mut *transaction)
    .await
    .context("read the connection bindings")?
    .into_iter()
    .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
    .collect();
    let schema_report = Some(crate::platform_migrations::observe_project(&mut transaction).await?);
    transaction.commit().await.context("end the project read")?;
    Ok(Some(ProjectState {
        installed,
        schema_report,
        bindings,
    }))
}

/// Pull the release manifest `digest` from the platform's release artifacts.
///
/// # Errors
///
/// When the manifest cannot be pulled or does not hash to `digest`.
pub async fn pull_release_manifest(
    platform: &Platform,
    digest: &str,
) -> anyhow::Result<ServingManifest> {
    let source = if platform.registry_token_metadata {
        ReleaseManifestSource::with_registry_token_metadata(
            &platform.release_artifact_base,
            platform.insecure_registry,
        )
    } else {
        ReleaseManifestSource::new(
            &platform.release_artifact_base,
            platform.insecure_registry,
            &platform.registry_auth_file,
        )
    }
    .context("open the release manifest source")?
    .with_ca_paths(&platform.oci_ca_paths)
    .context("read the registry CA")?;
    let bytes = source
        .pull_verified(digest)
        .await
        .with_context(|| format!("pull release manifest {digest}"))?;
    let (manifest, _) = ServingManifest::from_canonical_bytes(&bytes)
        .with_context(|| format!("read release manifest {digest}"))?;
    Ok(manifest)
}

async fn read_release(platform: &Platform, digest: &str) -> anyhow::Result<ReleaseFacts> {
    let manifest = pull_release_manifest(platform, digest).await?;
    // The admitted descriptors the manifest carries name the release's
    // connections (R2 (3)).
    let required_connections = manifest
        .components
        .iter()
        .flat_map(|component| &component.descriptor.connections)
        .filter_map(|connection| connection.store_alias.name().map(str::to_owned))
        .collect();
    Ok(ReleaseFacts {
        digest: digest.to_owned(),
        manifest,
        required_connections,
    })
}

/// The targeted application's Helm release and host group identity.
pub fn application_release_name(
    document: &EnvironmentDocument,
    application: &str,
) -> anyhow::Result<String> {
    Ok(
        wamn_control_provision::environment_names::application_release_name(
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
    target: &release_chart::Target,
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
    target: &release_chart::Target,
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
    target: &release_chart::Target,
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
    target: &release_chart::Target,
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
    Ok(Revision {
        number,
        status: entry["status"]
            .as_str()
            .context("Helm revision has no status")?
            .to_owned(),
        chart: entry["chart"].as_str().unwrap_or_default().to_owned(),
        values: serde_json::from_slice(&output.stdout).context("decode helm values")?,
    })
}

/// Discover application Helm releases in every status before status or environment deletion.
pub(crate) async fn application_releases(
    platform: &Platform,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<BTreeMap<String, String>> {
    wamn_control_provision::validate_project_env(
        &triple.org,
        &triple.project,
        triple.env.as_str(),
    )?;
    let coordinate = triple.to_string();
    let filter = wamn_registry::name::application_release_filter(triple.env.as_str());
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
        let expected = wamn_control_provision::environment_names::application_release_name(
            &triple.org,
            &triple.project,
            triple.env.as_str(),
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

/// Whether Helm holds the release chart of the environment, in any status.
pub(crate) async fn release_present(
    platform: &Platform,
    release_name: &str,
) -> anyhow::Result<bool> {
    let output = helm(platform, &["status", release_name]).await?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        stderr.contains("not found"),
        "helm status {release_name} exited {}: {stderr}",
        output.status
    );
    Ok(false)
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
            replica_refusal(connection, replicas)
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

    fn applications(entries: &[(&str, &[&str])]) -> wamn_catalog::EnvironmentApplications {
        entries
            .iter()
            .map(|(name, packages)| {
                (
                    (*name).to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        connections: BTreeMap::new(),
                        packages: packages.iter().map(|id| (*id).to_owned()).collect(),
                    },
                )
            })
            .collect()
    }

    fn sequences(
        entries: &[(&str, &[(&str, &str)])],
    ) -> Vec<wamn_compiler::manifest::PackageManifest> {
        entries
            .iter()
            .map(|(package, declared)| {
                let mut manifest = wamn_publish::test_support::source_manifest();
                manifest.package.id = (*package).to_owned();
                manifest.sequences = declared
                    .iter()
                    .map(|(name, pattern)| {
                        (
                            (*name).to_owned(),
                            wamn_compiler::manifest::SequenceDeclaration {
                                pattern: (*pattern).to_owned(),
                            },
                        )
                    })
                    .collect();
                manifest
            })
            .collect()
    }

    #[test]
    fn one_sequence_name_with_two_patterns_in_one_application_is_refused() {
        let declared = applications(&[("receiving", &["acme_receiving", "wamn_receiving"])]);
        let declarations = sequences(&[
            ("acme_receiving", &[("purchase_order", "ACME-{seq:po}")]),
            (
                "wamn_receiving",
                &[("purchase_order", "PO-{seq:po,width=6}")],
            ),
        ]);
        assert_eq!(
            sequence_refusals(&declared, &declarations),
            [
                "application receiving: packages acme_receiving and wamn_receiving declare \
                 sequence purchase_order with different patterns"
            ]
        );
    }

    #[test]
    fn one_sequence_name_with_one_pattern_in_one_application_is_allowed() {
        let declared = applications(&[("receiving", &["acme_receiving", "wamn_receiving"])]);
        let declarations = sequences(&[
            (
                "acme_receiving",
                &[("purchase_order", "PO-{seq:po,width=6}")],
            ),
            (
                "wamn_receiving",
                &[("purchase_order", "PO-{seq:po,width=6}")],
            ),
        ]);
        assert!(sequence_refusals(&declared, &declarations).is_empty());
    }

    #[test]
    fn one_sequence_name_in_two_applications_is_allowed() {
        let declared = applications(&[("receiving", &["wamn_receiving"]), ("wms", &["wamn_wms"])]);
        let declarations = sequences(&[
            (
                "wamn_receiving",
                &[("purchase_order", "PO-{seq:po,width=6}")],
            ),
            ("wamn_wms", &[("purchase_order", "W-{seq:po}")]),
        ]);
        assert!(sequence_refusals(&declared, &declarations).is_empty());
    }
}
