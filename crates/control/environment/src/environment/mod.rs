//! `wamn-ctl env`: the environment lifecycle
//! (docs/architecture/overview.md#environment-lifecycle, wamn-snz0).
//!
//! An environment is declared in one document, `environment.k`. `apply` moves
//! the environment toward it, under one lifecycle lock: lock, analyse, ensure,
//! expand, write, readiness, drain, contract (§10.1). `rollback` is `apply`
//! of the previous intended release (§11.2), `delete` the one destructive
//! verb (§12.3). `show` synthesizes the document from the authorities, and
//! `status` reads the current state (§11.4).
//!
//! The platform inputs are environment variables of the verb, not flags and
//! not document fields (epic decision D2).

pub mod analyse;
pub mod bindings;
mod clients;
pub mod connections;
pub mod delete;
pub mod document;
pub mod drain;
pub mod ensure;
pub mod expand;
pub(crate) mod host_report;
pub mod lock;
pub mod readiness;
pub mod rollback;
pub mod show;
pub mod stage;
pub mod status;
pub mod write;

#[cfg(test)]
mod admin_sql_tests;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

use analyse::Analysis;
use document::EnvironmentDocument;
use lock::LifecycleLock;
use wamn_identity_client::PatIssuerConfig;
use wamn_release_control::release_chart;

/// The namespace of the release chart when `WAMN_RELEASE_NAMESPACE` is unset.
const DEFAULT_NAMESPACE: &str = "wamn-system";

/// The platform inputs of the lifecycle verbs.
#[derive(Debug, Clone)]
pub struct Platform {
    /// The system database URL selected for this verb.
    pub system_database_url: String,
    /// The cluster, its context and the operator release's namespace (R6).
    pub target: release_chart::Target,
    /// A stamped release chart directory.
    pub chart: PathBuf,
    /// `<registry>/<repository>` of release manifest artifacts.
    pub release_artifact_base: String,
    pub registry_auth_file: PathBuf,
    pub oci_ca_paths: Vec<PathBuf>,
    /// `<registry>/<repository>` of package artifacts. Needed only when a
    /// release names a package the environment does not have yet, or the
    /// document pins a web artifact.
    pub package_artifact_base: Option<String>,
    /// The repository of component artifacts used during package admission.
    pub component_artifact_base: Option<String>,
    /// The namespace of the CloudNativePG `Cluster`, which the environment's
    /// `Database` CR shares.
    pub database_namespace: String,
    /// The host and port every credential URL names. Absent host: the target
    /// cluster's `<cluster>-rw` service.
    pub database_host: Option<String>,
    pub database_port: u16,
    /// A YAML mapping: the platform's part of the host group body (replicas,
    /// resources, registry mounts). The verb adds the environment's
    /// credential Secret references.
    pub host_group: Option<PathBuf>,
    /// The identity service that issues the management-author PAT. Without
    /// it, `apply` issues no PAT.
    pub pat_issuer: PatIssuerConfig,
    /// The event broker for application capture and entity consumers.
    pub events: Option<EventBroker>,
    /// The scheduler NATS the verbs publish their auth changes on.
    /// [`Platform::from_env`] requires it. A test without hosts sets `None`.
    pub auth_nats: Option<wamn_platform_identity::nats::PlatformNats>,
}

/// The event broker inputs of CDC (`enable-cdc-project-env`).
#[derive(Debug, Clone)]
pub struct EventBroker {
    /// The NATS connection with the client certificate (docs/plan/alignment.md R4).
    pub nats: wamn_platform_identity::nats::PlatformNats,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

impl Platform {
    /// Read the platform inputs: `WAMN_CONTROL_DATABASE_URL`, `KUBECONFIG`
    /// and its current context, `WAMN_RELEASE_NAMESPACE` (default
    /// `wamn-system`), `WAMN_RELEASE_CHART`, `WAMN_RELEASE_ARTIFACT_BASE`,
    /// `WAMN_REGISTRY_AUTH_FILE` and `WAMN_OCI_CA_PATHS` (colon-separated).
    ///
    /// # Errors
    ///
    /// When a required variable is unset, or the kubeconfig has no current context.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_database_env("WAMN_CONTROL_DATABASE_URL")
    }

    /// Read the restricted environment apply inputs.
    pub fn apply_from_env() -> anyhow::Result<Self> {
        Self::from_database_env("WAMN_APPLY_DATABASE_URL")
    }

    fn from_database_env(database_variable: &str) -> anyhow::Result<Self> {
        let variable = |name: &str| {
            std::env::var(name).with_context(|| format!("set {name} for wamn-ctl env"))
        };
        let kubeconfig = match std::env::var_os("KUBECONFIG") {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(variable("HOME")?).join(".kube/config"),
        };
        let context = current_context(&kubeconfig)?;
        let namespace = std::env::var("WAMN_RELEASE_NAMESPACE")
            .unwrap_or_else(|_| DEFAULT_NAMESPACE.to_owned());
        Ok(Self {
            system_database_url: variable(database_variable)?,
            chart: PathBuf::from(variable("WAMN_RELEASE_CHART")?),
            release_artifact_base: variable("WAMN_RELEASE_ARTIFACT_BASE")?,
            registry_auth_file: PathBuf::from(variable("WAMN_REGISTRY_AUTH_FILE")?),
            oci_ca_paths: std::env::var("WAMN_OCI_CA_PATHS")
                .map(|paths| {
                    paths
                        .split(':')
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            package_artifact_base: std::env::var("WAMN_PACKAGE_ARTIFACT_BASE").ok(),
            component_artifact_base: std::env::var("WAMN_COMPONENT_ARTIFACT_BASE").ok(),
            database_namespace: std::env::var("WAMN_DATABASE_NAMESPACE")
                .unwrap_or_else(|_| namespace.clone()),
            database_host: std::env::var("WAMN_DATABASE_HOST").ok(),
            database_port: std::env::var("WAMN_DATABASE_PORT")
                .ok()
                .map(|port| port.parse().context("WAMN_DATABASE_PORT is not a port"))
                .transpose()?
                .unwrap_or(5432),
            host_group: std::env::var_os("WAMN_RELEASE_HOST_GROUP").map(PathBuf::from),
            pat_issuer: PatIssuerConfig {
                endpoint: std::env::var("WAMN_IDENTITY_URL").ok(),
                client_cert: std::env::var_os("WAMN_IDENTITY_CLIENT_CERT").map(PathBuf::from),
                client_key: std::env::var_os("WAMN_IDENTITY_CLIENT_KEY").map(PathBuf::from),
                server_ca: std::env::var_os("WAMN_IDENTITY_SERVER_CA").map(PathBuf::from),
            },
            events: event_broker_from_env()?,
            auth_nats: Some(wamn_platform_identity::nats::PlatformNats::from_env()?),
            target: release_chart::Target {
                kubeconfig,
                context,
                namespace,
            },
        })
    }

    pub(crate) fn component_source(
        &self,
    ) -> anyhow::Result<
        Option<wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig>,
    > {
        self.component_artifact_base
            .as_deref()
            .map(|base| -> anyhow::Result<_> {
                Ok(
                    wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig::new(
                        base,
                        false,
                        std::time::Duration::from_secs(30),
                    )?
                    .with_registry_auth_file(&self.registry_auth_file)?
                    .with_ca_paths(&self.oci_ca_paths)?,
                )
            })
            .transpose()
    }

    /// The publisher of the auth changes of a verb.
    pub async fn auth_changes(
        &self,
    ) -> anyhow::Result<wamn_platform_identity::auth_changes::AuthChanges> {
        match &self.auth_nats {
            Some(connection) => {
                wamn_platform_identity::auth_changes::AuthChanges::connect(connection).await
            }
            None => Ok(wamn_platform_identity::auth_changes::AuthChanges::default()),
        }
    }
}

fn current_context(kubeconfig: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("kubectl")
        .args(["config", "current-context", "--kubeconfig"])
        .arg(kubeconfig)
        .output()
        .context("start kubectl")?;
    if !output.status.success() {
        bail!(
            "{} has no current context: {}",
            kubeconfig.display(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("kubectl wrote a context that is not UTF-8")?
        .trim()
        .to_owned())
}

/// `env apply <file> --dry-run`: analyse without the lock and return the
/// plan. The plan describes this moment only.
///
/// # Errors
///
/// When the document is refused, or an authority cannot be read.
pub async fn dry_run(platform: &Platform, file: &Path) -> anyhow::Result<Analysis> {
    let document = EnvironmentDocument::compile(file)?;
    analyse::analyse(platform, &document).await
}

/// `env apply <file>`: take the lifecycle lock and run the steps of §10.1.
///
/// # Errors
///
/// When the lock is held, the document is refused, or a step fails. The error
/// names the step.
pub async fn apply(platform: &Platform, file: &Path, admin_url: &str) -> anyhow::Result<Analysis> {
    let document = EnvironmentDocument::compile(file)?;
    LifecycleLock::hold(
        &platform.system_database_url,
        &document.triple(),
        async |_| apply_locked(platform, &document, None, admin_url).await,
    )
    .await
}

/// Steps 2 to 8 of §10.1 under a lifecycle lock the caller holds: analyse,
/// ensure the substrate, expand (row, policy projection, connections,
/// packages, the package stage), write the release chart and wait for
/// readiness, or uninstall it for `release = none`, then drain.
/// `reason` is a rollback's, kept in the Helm description (§11.2).
///
/// Every step is an ensure judged by observation, so a crash anywhere is
/// repaired by applying again.
///
/// # Errors
///
/// When the document is refused, or a step fails. The error names the step.
pub(crate) async fn apply_locked(
    platform: &Platform,
    document: &EnvironmentDocument,
    reason: Option<&str>,
    admin_url: &str,
) -> anyhow::Result<Analysis> {
    wamn_package_control::schema_baseline::compare_system(&platform.system_database_url).await?;
    let mut analysis = analyse::analyse(platform, document).await?;
    ensure::ensure(platform, document, &analysis, admin_url)
        .await
        .context("step 4, ensure the substrate")?;
    let clients = expand::expand(platform, document, &analysis)
        .await
        .context("step 5, expand")?;
    let release_name = analysis.authorities.release_name.clone();
    if let Some(release) = &analysis.authorities.release {
        let written = write::write(platform, document, &analysis, release, reason, &clients)
            .await
            .context("step 6, write the release chart")?;
        readiness::wait(platform, &written)
            .await
            .context("step 7, readiness")?;
        analysis
            .plan
            .push(format!("wrote a revision of {release_name}"));
    } else if analyse::release_present(platform, &release_name).await? {
        // Release none: no qualification, stage or readiness. The uninstall
        // starts the drain (§12.2).
        release_chart::uninstall(&platform.target, &release_name)
            .await
            .context("step 6, uninstall the release chart")?;
        analysis.plan.push(format!("uninstalled {release_name}"));
    }
    drain::drain(platform, document, &analysis)
        .await
        .context("step 8, drain")?;
    Ok(analysis)
}

/// Read event broker inputs for both deployed and local workflow verbs.
pub fn event_broker_from_env() -> anyhow::Result<Option<EventBroker>> {
    Ok(match std::env::var_os("WAMN_NATS_URL") {
        None => None,
        Some(_) => Some(EventBroker {
            nats: wamn_platform_identity::nats::PlatformNats::from_env()?,
            stream_replicas: std::env::var("WAMN_EVENT_STREAM_REPLICAS")
                .ok()
                .map(|count| count.parse().context("WAMN_EVENT_STREAM_REPLICAS"))
                .transpose()?
                .unwrap_or(1),
            dup_window_secs: std::env::var("WAMN_EVENT_DUP_WINDOW_SECS")
                .ok()
                .map(|seconds| seconds.parse().context("WAMN_EVENT_DUP_WINDOW_SECS"))
                .transpose()?
                .unwrap_or(120),
        }),
    })
}
