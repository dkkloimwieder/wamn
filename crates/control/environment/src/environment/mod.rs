//! Independent environment provisioning, initialization, and targeted application activation.

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
pub mod operation;
pub mod readiness;
pub mod rollback;
pub mod runtime;
pub mod show;
pub mod sql;
pub mod stage;
pub mod status;
pub mod write;

#[cfg(test)]
mod admin_sql_tests;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

use analyse::Analysis;
use document::EnvironmentDocument;
use wamn_identity_client::PatIssuerConfig;
use wamn_release_control::release_chart;

/// The namespace of the release chart when `WAMN_RELEASE_NAMESPACE` is unset.
const DEFAULT_NAMESPACE: &str = "hosts";

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
    pub registry_token_metadata: bool,
    pub oci_ca_paths: Vec<PathBuf>,
    /// Registry transport selected by the installed platform host.
    pub insecure_registry: bool,
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
    /// Service `<cluster>-rw.<database_namespace>.svc.cluster.local`.
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
    /// `hosts`), `WAMN_RELEASE_CHART`, `WAMN_RELEASE_ARTIFACT_BASE`,
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
        Self::from_inputs(
            database_variable,
            kubernetes_target(
                std::env::var("WAMN_RELEASE_NAMESPACE")
                    .unwrap_or_else(|_| DEFAULT_NAMESPACE.to_owned()),
            )?,
            std::env::var_os("WAMN_RELEASE_CHART")
                .map(PathBuf::from)
                .unwrap_or_default(),
            false,
        )
    }

    /// Read SQL Job inputs without invoking Kubernetes or requiring a release chart.
    pub fn sql_from_env() -> anyhow::Result<Self> {
        Self::from_inputs(
            "WAMN_APPLY_DATABASE_URL",
            release_chart::Target {
                kubeconfig: PathBuf::new(),
                context: String::new(),
                namespace: std::env::var("WAMN_RELEASE_NAMESPACE")
                    .unwrap_or_else(|_| DEFAULT_NAMESPACE.to_owned()),
            },
            PathBuf::new(),
            false,
        )
    }

    /// Read only database inputs for a project substrate SQL Job.
    pub fn project_schema_from_env() -> anyhow::Result<Self> {
        Self::from_inputs(
            "WAMN_APPLY_DATABASE_URL",
            release_chart::Target {
                kubeconfig: PathBuf::new(),
                context: String::new(),
                namespace: std::env::var("WAMN_RELEASE_NAMESPACE")
                    .unwrap_or_else(|_| DEFAULT_NAMESPACE.to_owned()),
            },
            PathBuf::new(),
            true,
        )
    }

    fn from_inputs(
        database_variable: &str,
        target: release_chart::Target,
        chart: PathBuf,
        project_schema: bool,
    ) -> anyhow::Result<Self> {
        let variable = |name: &str| {
            std::env::var(name).with_context(|| format!("set {name} for wamn-ctl env"))
        };
        let registry_token_metadata =
            std::env::var("WAMN_REGISTRY_TOKEN_METADATA").as_deref() == Ok("true");
        let host_group = if project_schema {
            None
        } else {
            std::env::var_os("WAMN_RELEASE_HOST_GROUP").map(PathBuf::from)
        };
        let insecure_registry = match &host_group {
            Some(path) => runtime::insecure_registry(&serde_yaml::from_slice(
                &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
            )?),
            None => false,
        };
        Ok(Self {
            insecure_registry,
            system_database_url: variable(database_variable)?,
            chart,
            release_artifact_base: if project_schema {
                String::new()
            } else {
                variable("WAMN_RELEASE_ARTIFACT_BASE")?
            },
            registry_token_metadata,
            registry_auth_file: if project_schema || registry_token_metadata {
                PathBuf::new()
            } else {
                PathBuf::from(variable("WAMN_REGISTRY_AUTH_FILE")?)
            },
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
                .unwrap_or_else(|_| "platform".to_owned()),
            database_host: std::env::var("WAMN_DATABASE_HOST").ok(),
            database_port: std::env::var("WAMN_DATABASE_PORT")
                .ok()
                .map(|port| port.parse().context("WAMN_DATABASE_PORT is not a port"))
                .transpose()?
                .unwrap_or(5432),
            host_group,
            pat_issuer: PatIssuerConfig {
                endpoint: std::env::var("WAMN_IDENTITY_URL").ok(),
                client_cert: std::env::var_os("WAMN_IDENTITY_CLIENT_CERT").map(PathBuf::from),
                client_key: std::env::var_os("WAMN_IDENTITY_CLIENT_KEY").map(PathBuf::from),
                server_ca: std::env::var_os("WAMN_IDENTITY_SERVER_CA").map(PathBuf::from),
            },
            events: if project_schema {
                None
            } else {
                event_broker_from_env()?
            },
            auth_nats: if project_schema {
                None
            } else {
                Some(wamn_platform_identity::nats::PlatformNats::from_env()?)
            },
            target,
        })
    }

    pub(crate) async fn registry(
        &self,
        base: &str,
    ) -> anyhow::Result<wamn_package_artifact::PackageRegistry> {
        let credentials = if self.registry_token_metadata {
            let base = wamn_catalog::parse_component_artifact_base(base)?;
            Some(wamn_package_artifact::registry_credentials::RegistryCredentialSource::MetadataServer
                .credentials(base.registry()).await?)
        } else {
            None
        };
        Ok(wamn_package_artifact::PackageRegistry {
            artifact_base: base.to_owned(),
            credentials,
            registry_auth_file: self.registry_auth_file.clone(),
            insecure_registry: self.insecure_registry,
            oci_ca_paths: self.oci_ca_paths.clone(),
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
                let source =
                    wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig::new(
                        base,
                        self.insecure_registry,
                        std::time::Duration::from_secs(30),
                    )?;
                let source = if self.registry_token_metadata {
                    source.with_registry_token_metadata()
                } else {
                    source.with_registry_auth_file(&self.registry_auth_file)?
                };
                Ok(source.with_ca_paths(&self.oci_ca_paths)?)
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

/// Read the current Kubernetes target without database or artifact inputs.
pub fn kubernetes_target(namespace: String) -> anyhow::Result<release_chart::Target> {
    let kubeconfig = match std::env::var_os("KUBECONFIG") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var_os("HOME").context("set HOME or KUBECONFIG")?)
            .join(".kube/config"),
    };
    Ok(release_chart::Target {
        context: current_context(&kubeconfig)?,
        kubeconfig,
        namespace,
    })
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
pub async fn dry_run(
    platform: &Platform,
    file: &Path,
    application: Option<&str>,
) -> anyhow::Result<Analysis> {
    let document = EnvironmentDocument::read_deployed(platform, file, application).await?;
    let private = ensure::PrivateDirectory::create()?;
    let mut resolved = platform.clone();
    runtime::resolve_target(&mut resolved, &document, &private.path(".")).await?;
    analyse::analyse(&resolved, &document, application).await
}

/// Apply explicit environment configuration and reapply one recorded application selection.
///
/// # Errors
///
/// When the document is refused or an operation fails. The error
/// names the operation.
pub async fn apply(
    platform: &Platform,
    file: &Path,
    application: &str,
) -> anyhow::Result<Analysis> {
    EnvironmentDocument::compile(file)?
        .provision_document(platform)
        .await?;
    apply_selection(
        platform,
        file,
        application,
        document::SelectionChange::Reapply,
        None,
    )
    .await
}

/// Resolve the published release's application from the document's declared membership.
pub async fn deploy(platform: &Platform, file: &Path, digest: &str) -> anyhow::Result<Analysis> {
    deploy_as(platform, file, digest, None).await
}

async fn deploy_as(
    platform: &Platform,
    file: &Path,
    digest: &str,
    actor: Option<&str>,
) -> anyhow::Result<Analysis> {
    wamn_package_artifact::ensure_digest(digest)?;
    let document = EnvironmentDocument::read_deployed(platform, file, None).await?;
    let mut platform = platform.clone();
    runtime::resolve_platform(&mut platform, &document.org).await?;
    let manifest = analyse::pull_release_manifest(&platform, digest).await?;
    let packages = manifest
        .release
        .packages
        .iter()
        .map(wamn_catalog::PackageCoordinate::package_id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut owners = document
        .applications
        .iter()
        .filter(|(_, selection)| {
            selection
                .packages
                .iter()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>()
                == packages
        })
        .map(|(name, _)| name.as_str());
    let application = owners
        .next()
        .context("published release does not match a declared application")?;
    anyhow::ensure!(
        owners.next().is_none(),
        "published release matches multiple applications"
    );
    apply_selection(
        &platform,
        file,
        application,
        document::SelectionChange::Deploy(digest),
        actor,
    )
    .await
}

/// Persist and apply one code selection under its application lock.
async fn apply_selection(
    platform: &Platform,
    file: &Path,
    application: &str,
    change: document::SelectionChange<'_>,
    actor: Option<&str>,
) -> anyhow::Result<Analysis> {
    let input = EnvironmentDocument::compile(file)?;
    let mut document;
    let release_name = analyse::application_release_name(&input, application)?;
    let private = ensure::PrivateDirectory::create()?;
    let mut platform = platform.clone();
    let control = runtime::resolve_platform(&mut platform, &input.org).await?;
    let lock = lock::LifecycleLock::acquire_application(
        &platform.system_database_url,
        &input.triple(),
        application,
    )
    .await?;
    document = EnvironmentDocument::read_target(&platform, &input, Some(application)).await?;
    let before_selection = document
        .applications
        .get(application)
        .with_context(|| format!("application {application} is not declared"))?;
    let applied = analyse::read_applied_releases(&platform, &release_name).await?;
    if let Some(warning) = document::selection_warning(
        application,
        &before_selection.current,
        applied.current.as_deref(),
    ) {
        eprintln!("WARN {warning}");
    }
    document.select_release(application, change, &applied)?;
    crate::provision_project_env::registry::require_no_reset(lock.client(), &input.triple())
        .await?;
    let result = async {
        if matches!(change, document::SelectionChange::Reapply) {
            if ensure::resources_in_place(&platform, &document).await? {
                let cluster = sql::reset::recorded_cluster(&platform, &document).await?;
                crate::platform::operations::provision(&platform, &document, &cluster).await?;
            } else {
                let admin = std::env::var("WAMN_SYSTEM_ADMIN_URL")
                    .context("set WAMN_SYSTEM_ADMIN_URL for environment provisioning")?;
                Box::pin(ensure::provision_resources(
                    &platform,
                    &document,
                    &[],
                    &admin,
                ))
                .await?;
            }
        }
        runtime::resolve_application(&mut platform, &document, &private.path("."), &control)
            .await?;
        let mut analysis = analyse::analyse(&platform, &document, Some(application)).await?;
        if let Some(actor) = actor {
            analysis.actor = actor.to_owned();
        }
        let prepared = match &analysis.authorities.release {
            Some(release) => Some(write::prepare(&platform, &document, &analysis, release).await?),
            None => None,
        };
        let provisioning;
        let publisher = if let Some(actor) = actor {
            actor
        } else {
            provisioning = wamn_project_state::PlatformComponent::Provisioning
                .principal_id()
                .to_string();
            &provisioning
        };
        expand::expand(&platform, &document, &analysis, publisher)
            .await
            .context("prepare application control records")?;
        let document = document.persist_selection(&platform, application).await?;
        let release_name = &analysis.authorities.release_name;
        if let Some(prepared) = prepared {
            let written = write::write(
                &platform,
                &analysis,
                prepared,
                matches!(change, document::SelectionChange::Rollback(_)),
            )
            .await
            .context("write the application release chart")?;
            readiness::wait(&platform, &written)
                .await
                .context("wait for application readiness")?;
            analysis
                .plan
                .push(format!("wrote a revision of {release_name}"));
        } else if analyse::release_present(&platform, release_name).await? {
            release_chart::recover_pending(&platform.target, release_name).await?;
            release_chart::uninstall(&platform.target, release_name)
                .await
                .context("uninstall the application release chart")?;
            analysis.plan.push(format!("uninstalled {release_name}"));
        }
        drain::drain(&platform, &document, &analysis)
            .await
            .context("observe application drain")?;
        stage::retire(&platform, &document, &analysis)
            .await
            .context("retire drained application facts")?;
        Ok(analysis)
    }
    .await;
    lock.release().await;
    result
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
