//! `wamn-ctl env`: the environment lifecycle (docs/plan/platform-deploy.md
//! §10, §11, §12.3, epic `wamn-snz0`).
//!
//! An environment is declared in one document, `environment.k`. `apply` moves
//! the environment toward it, under one lifecycle lock: lock, analyse, ensure,
//! expand, write, readiness, drain, contract (§10.1). `show` synthesizes the
//! document from the authorities. This module holds steps 1 to 7; drain and
//! contract (steps 8 and 9) land with `wamn-snz0.4`.
//!
//! The platform inputs are environment variables of the verb, not flags and
//! not document fields (epic decision D2).

pub mod analyse;
pub mod document;
pub mod ensure;
pub mod expand;
pub mod lock;
pub mod readiness;
pub mod show;
pub mod stage;
pub mod write;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

use crate::pat_client::PatIssuerConfig;
use crate::release_chart;
use analyse::Analysis;
use document::EnvironmentDocument;
use lock::LifecycleLock;

/// The namespace of the release chart when `WAMN_RELEASE_NAMESPACE` is unset.
const DEFAULT_NAMESPACE: &str = "wamn-system";

/// The platform inputs of the lifecycle verbs.
#[derive(Debug, Clone)]
pub struct Platform {
    /// A URL of the system database whose login may `SET ROLE wamn_system`.
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
    /// release names a package the environment does not have yet.
    pub package_artifact_base: Option<String>,
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
    /// The event broker of CDC. Needed only when a release's packages declare
    /// a model schema and the environment has no CDC reader yet.
    pub events: Option<EventBroker>,
}

/// The event broker inputs of CDC (`enable-cdc-project-env`).
#[derive(Debug, Clone)]
pub struct EventBroker {
    pub nats_url: String,
    pub nats_username: String,
    pub nats_password_file: PathBuf,
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
            system_database_url: variable("WAMN_CONTROL_DATABASE_URL")?,
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
            events: match std::env::var("WAMN_EVENT_NATS_URL") {
                Err(_) => None,
                Ok(nats_url) => Some(EventBroker {
                    nats_url,
                    nats_username: variable("WAMN_EVENT_NATS_USERNAME")?,
                    nats_password_file: PathBuf::from(variable("WAMN_EVENT_NATS_PASSWORD_FILE")?),
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
            },
            target: release_chart::Target {
                kubeconfig,
                context,
                namespace,
            },
        })
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

/// `env apply <file>`: take the lifecycle lock, analyse, then run steps 4 to
/// 7 of §10.1: ensure the substrate, expand (row, policy projection,
/// connections, packages, the package stage), write the release chart, and
/// wait for readiness. Drain and contract are `wamn-snz0.4`.
///
/// Every step is an ensure judged by observation, so a crash anywhere is
/// repaired by applying again.
///
/// # Errors
///
/// When the lock is held, the document is refused, or a step fails. The error
/// names the step.
pub async fn apply(platform: &Platform, file: &Path) -> anyhow::Result<Analysis> {
    let document = EnvironmentDocument::compile(file)?;
    let _lock = LifecycleLock::acquire(&platform.system_database_url, &document.triple()).await?;
    let mut analysis = analyse::analyse(platform, &document).await?;
    if analysis.authorities.release.is_none() && analysis.authorities.revision.is_some() {
        bail!(
            "release none uninstalls the release chart, and that write lands with wamn-snz0.4; \
             nothing was written"
        );
    }
    ensure::ensure(platform, &document, &analysis)
        .await
        .context("step 4, ensure the substrate")?;
    expand::expand(platform, &document, &analysis)
        .await
        .context("step 5, expand")?;
    if let Some(release) = &analysis.authorities.release {
        let written = write::write(platform, &document, &analysis, release)
            .await
            .context("step 6, write the release chart")?;
        readiness::wait(platform, &written)
            .await
            .context("step 7, readiness")?;
        analysis.plan.push(if written.helm_written {
            format!("wrote a revision of {}", written.release_name)
        } else {
            format!(
                "the values of {} are unchanged; Helm wrote nothing",
                written.release_name
            )
        });
    }
    Ok(analysis)
}
