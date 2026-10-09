//! `wamn-ctl env`: the environment lifecycle (docs/plan/platform-deploy.md
//! §10, §11, §12.3, epic `wamn-snz0`).
//!
//! An environment is declared in one document, `environment.k`. `apply` moves
//! the environment toward it, under one lifecycle lock: lock, analyse, ensure,
//! expand, write, readiness, drain, contract (§10.1). `show` synthesizes the
//! document from the authorities. This module holds steps 1 to 3 so far.
//!
//! The platform inputs are environment variables of the verb, not flags and
//! not document fields (epic decision D2).

pub mod analyse;
pub mod document;
pub mod lock;
pub mod show;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

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
        Ok(Self {
            system_database_url: variable("WAMN_CONTROL_DATABASE_URL")?,
            target: release_chart::Target {
                kubeconfig,
                context,
                namespace: std::env::var("WAMN_RELEASE_NAMESPACE")
                    .unwrap_or_else(|_| DEFAULT_NAMESPACE.to_owned()),
            },
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

/// `env apply <file>`: take the lifecycle lock, then analyse.
///
/// Steps 4 to 9 of §10.1 land with `wamn-snz0.3` and `wamn-snz0.4`. Until
/// then the verb refuses after analysis and writes nothing.
///
/// # Errors
///
/// When the lock is held, the document is refused, or the write steps are
/// not built yet.
pub async fn apply(platform: &Platform, file: &Path) -> anyhow::Result<Analysis> {
    let document = EnvironmentDocument::compile(file)?;
    let _lock = LifecycleLock::acquire(&platform.system_database_url, &document.triple()).await?;
    let analysis = analyse::analyse(platform, &document).await?;
    bail!(
        "analysis passed with plan:\n- {}\nthe write steps of env apply are not built yet (wamn-snz0.3)",
        analysis.plan.join("\n- ")
    )
}
