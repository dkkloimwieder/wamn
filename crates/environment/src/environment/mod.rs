//! Independent environment provisioning, initialization, and targeted application activation.

pub mod analyse;
pub mod bindings;
pub mod capture_gap;
mod clients;
pub mod connections;
pub mod delete;
pub mod document;
pub mod drain;
pub mod ensure;
pub mod expand;
pub mod lifecycle;
pub mod operation;
pub mod readiness;
pub mod retention;
pub mod runtime;
pub mod show;
pub mod sql;
pub mod stage;
pub mod write;

use std::path::PathBuf;

use anyhow::Context as _;

use analyse::Analysis;
use document::EnvironmentDocument;
use wamn_identity_client::PatIssuerConfig;
use wamn_release_control::release_chart;

/// The platform inputs of the lifecycle verbs.
#[derive(Debug, Clone)]
pub struct Platform {
    /// The system database URL selected for this verb.
    pub system_database_url: String,
    /// The cluster, its context and the operator release's namespace (R6).
    pub target: release_chart::Target,
    /// A stamped release chart directory.
    pub chart: PathBuf,
    /// Keep fetched chart files alive for this operation.
    pub chart_download: Option<std::sync::Arc<crate::platform::artifact_plan::SelectedChart>>,
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
    pub events: Option<wamn_broker::EventBroker>,
    /// The scheduler NATS the verbs publish their auth changes on.
    /// A test without hosts sets `None`.
    pub auth_nats: Option<wamn_broker::nats::PlatformNats>,
}

impl Platform {
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
        Option<wamn_component_artifact::component_artifact_source::ComponentArtifactSourceConfig>,
    > {
        self.component_artifact_base
            .as_deref()
            .map(|base| -> anyhow::Result<_> {
                let source =
                    wamn_component_artifact::component_artifact_source::ComponentArtifactSourceConfig::new(
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
    pub async fn auth_changes(&self) -> anyhow::Result<wamn_broker::Publisher> {
        match &self.auth_nats {
            Some(connection) => wamn_broker::Publisher::connect(connection).await,
            None => Ok(wamn_broker::Publisher::default()),
        }
    }
}

/// Deploy a published release to the named application. Membership comes from its manifest.
pub async fn deploy(
    platform: &Platform,
    triple: &wamn_registry::Triple,
    application: &wamn_catalog::ApplicationName,
    digest: &wamn_digest::Digest,
) -> anyhow::Result<Analysis> {
    apply_selection(
        platform,
        triple,
        application,
        document::ReleaseChange::Deploy(digest),
        None,
    )
    .await
}

/// Stop one application's Helm release and observe its drain.
pub async fn stop(
    platform: &Platform,
    triple: &wamn_registry::Triple,
    application: &wamn_catalog::ApplicationName,
) -> anyhow::Result<Analysis> {
    apply_selection(
        platform,
        triple,
        application,
        document::ReleaseChange::Stop,
        None,
    )
    .await
}

/// Apply one application under its lock and record its desired selection in the registry.
async fn apply_selection(
    platform: &Platform,
    triple: &wamn_registry::Triple,
    application: &wamn_catalog::ApplicationName,
    change: document::ReleaseChange<'_>,
    actor: Option<&wamn_identity_protocol::PrincipalId>,
) -> anyhow::Result<Analysis> {
    let mut platform = platform.clone();
    runtime::resolve_platform(&mut platform, &triple.org).await?;
    let lock = wamn_catalog::lifecycle_lock::LifecycleLock::acquire_application(
        wamn_postgres::connect(&platform.system_database_url).await?,
        triple,
        application,
    )
    .await?;
    let result = lock
        .protect(async {
            let mut document = EnvironmentDocument::read(&platform, triple).await?;
            anyhow::ensure!(
                document.applications.contains_key(application),
                "application {application} is not declared"
            );
            let release_name = analyse::application_release_name(&document, application)?;
            match change {
                document::ReleaseChange::Deploy(digest)
                | document::ReleaseChange::Rollback(Some(digest)) => {
                    document
                        .applications
                        .get_mut(application)
                        .expect("application was declared")
                        .current = document::DeclaredRelease::Digest(digest.to_owned());
                }
                document::ReleaseChange::Rollback(None) => {
                    let revision = analyse::rollback_revision(&platform, &release_name).await?;
                    let digest = revision
                        .manifest_digest()
                        .context("rollback revision has no release digest")?;

                    document
                        .applications
                        .get_mut(application)
                        .expect("application was declared")
                        .current = document::DeclaredRelease::Digest(digest.to_owned());
                    document
                        .applications
                        .get_mut(application)
                        .expect("application was resolved")
                        .client = revision.client()?;
                }
                document::ReleaseChange::Stop => {
                    document
                        .applications
                        .get_mut(application)
                        .expect("application was declared")
                        .current = document::DeclaredRelease::None;
                }
            }
            crate::provision_project_env::registry::require_connected(
                &mut *lock.client().await,
                triple,
            )
            .await?;
            let mut analysis = analyse::analyse(&platform, &document, Some(application)).await?;
            if let Some(actor) = actor {
                analysis.actor = actor.to_string();
            }
            let prepared = match &analysis.authorities.release {
                Some(release) => {
                    Some(write::prepare(&platform, &document, &analysis, release).await?)
                }
                None => None,
            };
            let provisioning;
            let publisher = if let Some(actor) = actor {
                actor
            } else {
                provisioning =
                    wamn_identity_protocol::PlatformComponent::Provisioning.principal_id();
                &provisioning
            };
            expand::expand(&platform, &document, &analysis, publisher)
                .await
                .context("prepare application control records")?;
            if let Some(prepared) = prepared {
                let written = write::write(
                    &platform,
                    &analysis,
                    prepared,
                    matches!(change, document::ReleaseChange::Rollback(_)),
                )
                .await
                .context("write the application release chart")?;
                readiness::wait(&platform, &written)
                    .await
                    .context("wait for application readiness")?;
                analysis
                    .plan
                    .push(format!("wrote a revision of {release_name}"));
            } else if matches!(change, document::ReleaseChange::Stop)
                && analyse::release_present(&platform, &release_name).await?
            {
                release_chart::recover_pending(&platform.target, &release_name).await?;
                release_chart::uninstall(&platform.target, &release_name).await?;
                analysis.plan.push(format!("uninstalled {release_name}"));
            }
            let residency = drain::drain(&platform, &document, &analysis)
                .await
                .context("observe application residency")?;
            analysis.plan.push(format!(
                "release residency: {}",
                serde_json::to_string(&residency)?
            ));
            Ok(analysis)
        })
        .await;
    lock.release().await;
    result
}
