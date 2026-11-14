//! Project resources share the runtime, never their database or event authority.

use super::{
    HostArgs, RegistryPullCredential, connect_data_nats, host_credentials, releases,
    session_verifier,
};
use anyhow::Context as _;
use std::collections::HashMap;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;
use tokio::sync::watch;
use wamn_authentication::route_authentication::PlatformRouteAuthenticator;
use wamn_capability_authority::auth_cache::replicate;
use wamn_connections::connection_credentials::ConnectionCredentials;
use wamn_connections::connection_facts::ConnectionFacts;
use wamn_connections::connection_memory::{self, ConnectionMemory};
use wamn_control_provision::{SystemReader, project_env_database_name, system_reader_login};
use wamn_engine::artifact_source::ArtifactSource;
use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, RouteAuthenticator,
    authentication_unavailable,
};
use wamn_engine::release_manifest::ReleaseTarget;
use wamn_events_capability::WamnJetstream;
use wamn_events_capability::WamnJetstreamConfig;
use wamn_execution_host::OperationHostConfig;
use wamn_http_capability::transport::HttpTransport;
use wamn_kubernetes::environment_state::{ProjectResidency, Residency};
use wamn_postgres_capability::AuthorityClass;
use wamn_postgres_capability::WamnPostgres;
use wamn_postgres_capability::{
    CredentialProvider, ProjectConfig, ResolvedCredential, StaticCredentialProvider,
    WamnPostgresConfig,
};
use wamn_registry::{Triple, environment_nats_user, project_env_key};
use wash_runtime::plugin;

#[derive(Debug)]
pub(super) struct StatePoisoned(pub(super) &'static str);

impl std::fmt::Display for StatePoisoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} lock poisoned", self.0)
    }
}

impl std::error::Error for StatePoisoned {}

#[derive(Default)]
struct Project {
    credentials: StaticCredentialProvider,
    authentication: Option<(String, Arc<PlatformRouteAuthenticator>)>,
    releases: Option<Arc<releases::Releases>>,
}

#[derive(Default)]
pub(super) struct Projects(RwLock<BTreeMap<String, Arc<RwLock<Project>>>>);

impl Projects {
    fn entries(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Arc<RwLock<Project>>>> {
        // The index contains independent, fully constructed handles. Insertion
        // cannot corrupt other entries; project data changes under its own lock.
        self.0.read().unwrap_or_else(|error| error.into_inner())
    }

    fn get(&self, project: &str) -> Option<Arc<RwLock<Project>>> {
        self.entries().get(project).map(Arc::clone)
    }

    fn entry(&self, project: &str) -> Arc<RwLock<Project>> {
        // As with reads, recovery exposes only intact handles, never project data.
        let mut projects = self.0.write().unwrap_or_else(|error| error.into_inner());
        Arc::clone(projects.entry(project.to_owned()).or_default())
    }
}

pub(super) struct Credentials(pub(super) Arc<Projects>);
impl CredentialProvider for Credentials {
    fn resolve(
        &self,
        project: &str,
        class: AuthorityClass,
        project_env_key: Option<&str>,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        let Some(state) = self.0.get(project) else {
            return Ok(None);
        };
        state
            .read()
            .map_err(|_| StatePoisoned("project credentials"))?
            .credentials
            .resolve(project, class, project_env_key)
    }
}

pub(super) struct Authentication(pub(super) Arc<Projects>);
impl std::fmt::Debug for Authentication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authentication").finish_non_exhaustive()
    }
}
#[async_trait::async_trait]
impl RouteAuthenticator for Authentication {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let coordinate =
            wamn_registry::name::parse_project_env_key(&request.target.project_env_key)
                .ok_or_else(authentication_unavailable)?;
        let state = self
            .0
            .get(&coordinate.project)
            .ok_or_else(authentication_unavailable)?;
        let authenticator = {
            let state = state.read().map_err(|_| authentication_unavailable())?;
            let (_, authenticator) = state
                .authentication
                .as_ref()
                .filter(|(key, _)| key == &request.target.project_env_key)
                .ok_or_else(authentication_unavailable)?;
            Arc::clone(authenticator)
        };
        authenticator.authenticate(request).await
    }
}

pub(super) struct Environment {
    pub args: Arc<HostArgs>,
    pub engine: Arc<wash_runtime::engine::Engine>,
    pub postgres: Arc<WamnPostgres>,
    pub transport: Arc<HttpTransport>,
    pub logging: Arc<plugin::wasi_logging::TracingLogger>,
    pub source: Arc<dyn ArtifactSource>,
    pub router_owner: String,
    projects: Arc<Projects>,
    bytes: releases::ManifestCache,
}
impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Environment")
            .field("org", &self.args.org)
            .field("environment", &self.args.environment)
            .finish_non_exhaustive()
    }
}

impl Environment {
    #[expect(
        clippy::too_many_arguments,
        reason = "The composition root supplies the shared runtime services"
    )]
    pub fn new(
        args: Arc<HostArgs>,
        projects: Arc<Projects>,
        engine: Arc<wash_runtime::engine::Engine>,
        postgres: Arc<WamnPostgres>,
        transport: Arc<HttpTransport>,
        logging: Arc<plugin::wasi_logging::TracingLogger>,
        source: Arc<dyn ArtifactSource>,
        router_owner: String,
    ) -> Self {
        Self {
            args,
            projects,
            engine,
            postgres,
            transport,
            logging,
            source,
            router_owner,
            bytes: Arc::default(),
        }
    }

    fn declared(&self) -> anyhow::Result<HashSet<String>> {
        let path = self
            .args
            .projects
            .as_ref()
            .context("environment has no project projection")?;
        let projects: HashSet<String> = serde_json::from_slice(
            &std::fs::read(path).context("read environment project projection")?,
        )?;
        for project in &projects {
            anyhow::ensure!(
                wamn_registry::identifiers::project_id_reason(project).is_none()
                    && !wamn_registry::identifiers::project_id_is_reserved(project),
                "invalid project in environment projection"
            );
        }
        Ok(projects)
    }

    pub async fn for_workload(
        &self,
        annotations: &HashMap<String, String>,
    ) -> anyhow::Result<Arc<releases::ResidentRelease>> {
        let key = annotations
            .get("wamn.project_env_key")
            .context("workload has no project deployment")?;
        let coordinate = wamn_registry::name::parse_project_env_key(key)
            .context("invalid workload project deployment")?;
        anyhow::ensure!(
            Some(coordinate.org.as_str()) == self.args.org.as_deref()
                && Some(coordinate.env.as_str()) == self.args.environment.as_deref()
                && self.declared()?.contains(&coordinate.project),
            "workload belongs to another environment"
        );
        let state = self
            .projects
            .get(&coordinate.project)
            .context("workload project is not ready")?;
        let project = state
            .read()
            .map_err(|_| StatePoisoned("project releases"))?
            .releases
            .as_ref()
            .map(Arc::clone)
            .context("workload project is not ready")?;
        project.for_workload(annotations).await
    }

    pub fn files(
        &self,
        host: &str,
    ) -> anyhow::Result<Option<Arc<wamn_execution_host::file_route::FileRoutes>>> {
        let mut unavailable = None;
        for (project, state) in self.projects.entries().iter() {
            let files = (|| -> anyhow::Result<_> {
                let state = state
                    .read()
                    .map_err(|_| StatePoisoned("project releases"))?;
                match &state.releases {
                    Some(releases) => releases.files(host),
                    None => Ok(None),
                }
            })();
            match files {
                Ok(Some(files)) => return Ok(Some(files)),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%project, %error, "project file routes unavailable");
                    unavailable = Some(error);
                }
            }
        }
        match unavailable {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    async fn prepare(&self, project: &str) -> anyhow::Result<Arc<releases::Releases>> {
        let args = &self.args;
        let org = args
            .org
            .as_deref()
            .context("environment host requires an org")?;
        let environment = args
            .environment
            .as_deref()
            .context("environment host requires an environment")?;
        let directory = args
            .projects
            .as_ref()
            .context("missing project projection")?
            .parent()
            .context("project projection directory")?
            .join(project);
        let read = |family: wamn_control_provision::WorkloadRoleFamily| -> anyhow::Result<String> {
            std::fs::read_to_string(directory.join(format!("{}.json", family.component_stem())))
                .context("read projected project connection")
        };
        use wamn_control_provision::WorkloadRoleFamily as Family;
        let app = std::fs::read_to_string(directory.join("app.json"))
            .context("read project application connection")?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ApplicationConnections {
            compiled: wamn_postgres::ConnectionConfig,
            dynamic: wamn_postgres::ConnectionConfig,
        }
        let app: ApplicationConnections =
            serde_json::from_str(&app).context("decode project application connections")?;
        let compiled = app.compiled.to_json()?;
        let dynamic = app.dynamic.to_json()?;
        let executor = read(Family::ExecutorPlatform)?;
        let http = read(Family::HttpAdmitter)?;
        let administration = read(Family::Administration)?;
        let identity = read(Family::IdentityReader)?;
        let connection = read(Family::ConnectionReader)?;
        for (input, reader) in [
            (&identity, SystemReader::Identity),
            (&connection, SystemReader::Connection),
        ] {
            let (login, database) = wamn_postgres::identity(input)?;
            anyhow::ensure!(
                login == system_reader_login(reader, org, project, environment, &database),
                "project reader targets another deployment"
            );
        }
        let suffix = std::fs::read_to_string(directory.join("instance"))
            .context("read project database instance")?;
        let database = project_env_database_name(org, project, environment, suffix.trim());
        for input in [&compiled, &dynamic, &executor, &http, &administration] {
            anyhow::ensure!(
                wamn_postgres::identity(input)?.1 == database,
                "project credential targets another database instance"
            );
        }
        let cfg = WamnPostgresConfig::from_env();
        let project_state = self.projects.entry(project);
        project_state
            .write()
            .map_err(|_| StatePoisoned("project credentials"))?
            .credentials
            .insert(
                project.to_owned(),
                ProjectConfig {
                    credentials: host_credentials(
                        Some((compiled, dynamic)),
                        Some(executor),
                        Some(http.clone()),
                        Some(administration.clone()),
                    ),
                    guest_pool_max_size: cfg.guest_pool_max_size,
                    platform_pool_max_size: cfg.platform_pool_max_size,
                    wait_timeout_ms: cfg.wait_timeout_ms,
                    statement_timeout_ms: cfg.statement_timeout_ms,
                    row_limit: cfg.row_limit,
                },
            );
        let key = project_env_key(org, project, environment);
        let nats = connect_data_nats(
            args,
            &self.router_owner,
            Some(&environment_nats_user(&key)),
            Some(&directory.join("nats")),
        )
        .await?;
        let reader = wamn_postgres::pool(&connection, 4).await?;
        let identity = wamn_postgres::pool(&identity, 4).await?;
        let target = ReleaseTarget::new(&key, environment);
        let mut events = WamnJetstreamConfig::from_env().with_nats(nats.clone());
        events.event_target = Some(Triple::new(org, environment, project));
        let jetstream = Arc::new(WamnJetstream::new(events).await?);
        let base = args
            .component_artifact_base
            .as_deref()
            .context("missing component registry")?;
        let registry_name = wamn_catalog::parse_component_artifact_base(base)?;
        let registry_credentials = match RegistryPullCredential::select(args.registry_auth_file.as_deref(), args.registry_token_metadata)? {
            RegistryPullCredential::AuthFile(path) => wamn_package_artifact::registry_credentials::RegistryCredentialSource::Fixed(wamn_package_artifact::registry_credentials::read_registry_credentials(path, registry_name.registry())?),
            RegistryPullCredential::TokenMetadata => wamn_package_artifact::registry_credentials::RegistryCredentialSource::MetadataServer,
        };
        let envelope = args
            .oauth_encryption
            .as_deref()
            .map(|value| -> anyhow::Result<_> {
                Ok(Arc::new(
                    wamn_connections::oauth_encryption::Envelope::open(&serde_json::from_str(
                        value,
                    )?)?,
                ))
            })
            .transpose()?;
        let verifier = if args.session_issuer.is_some() {
            Some(session_verifier(
                args,
                environment,
                project,
                suffix.trim(),
                &http,
            )?)
        } else {
            None
        };
        let allowed_hosts = args
            .allowed_hosts
            .iter()
            .map(|value| value.parse())
            .collect::<Result<Vec<_>, _>>()?
            .into();
        let warm_reuse = wamn_engine::warm_reuse::WarmReuse::new(
            &args.trusted_warm_component_digest,
            args.component_pool_size,
            args.component_reclaim_window_seconds,
        )?;
        let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
        let leases = Arc::new(wamn_connections::resource_lease::ResourceLeases::new(
            Some(&connection),
        )?);
        let memory =
            Arc::new(ConnectionMemory::default().with_resource_leases(Arc::clone(&leases)));
        let changes = connection_memory::replicate(&nats, Some(&key), Arc::clone(&memory));
        let credentials = Arc::new(ConnectionCredentials::from_database(reader.clone(), org));
        let facts = Arc::new(
            ConnectionFacts::new(
                memory,
                changes,
                credentials,
                &self.postgres,
                &key,
                project,
                environment,
            )
            .with_oauth_encryption(envelope),
        );
        let authenticator = Arc::new(PlatformRouteAuthenticator::new(
            Arc::clone(&self.postgres),
            [Arc::clone(&facts)],
            Some((
                identity.clone(),
                verifier
                    .into_iter()
                    .map(|verifier| (key.clone(), verifier))
                    .collect(),
            )),
        ));
        let manager = Arc::new(releases::Releases::new(
            releases::Resources {
                engine: Arc::clone(&self.engine),
                postgres: Arc::clone(&self.postgres),
                blobstore: Arc::new(wamn_blobstore::plugin::WamnBlobstore::new(Arc::clone(
                    &self.postgres,
                ))),
                transport: Arc::clone(&self.transport),
                facts,
                logging: Arc::clone(&self.logging),
                source: Arc::clone(&self.source),
                allowed_hosts,
                configuration: OperationHostConfig {
                    project: project.to_owned(),
                    schema: args.schema.clone(),
                    owner_prefix: self.router_owner.clone(),
                    warm_reuse,
                },
                target,
                org: org.to_owned(),
                connection_reader: Some(reader),
                identity: Some((identity, org.to_owned())),
                administration_url: Some(administration),
                changes: replicate(&nats, Some(&key), Arc::clone(self.postgres.auth_cache())),
                jetstream,
                registry: Some((
                    wamn_package_artifact::PackageRegistry {
                        artifact_base: base.to_owned(),
                        credentials: None,
                        registry_auth_file: args.registry_auth_file.clone().unwrap_or_default(),
                        insecure_registry: args.allow_insecure_registries,
                        oci_ca_paths: args.oci_ca_paths.clone(),
                    },
                    registry_credentials,
                )),
                queue_lease_ttl_ms: args.queue_lease_ttl_ms,
                leases,
            },
            api,
            Arc::clone(&self.bytes),
        ));
        let mut state = project_state
            .write()
            .map_err(|_| StatePoisoned("project resources"))?;
        state.authentication = Some((key, authenticator));
        state.releases = Some(Arc::clone(&manager));
        Ok(manager)
    }

    pub async fn serve(
        self: &Arc<Self>,
        mut stopping: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut initializers =
            tokio::task::JoinSet::<(String, anyhow::Result<Arc<releases::Releases>>)>::new();
        let mut initializing = HashSet::<String>::new();
        let mut managers = tokio::task::JoinSet::new();
        let mut stops = HashMap::<String, watch::Sender<bool>>::new();
        let mut ticks = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = stopping.changed() => break,
                Some(prepared) = initializers.join_next(), if !initializers.is_empty() => {
                    match prepared {
                        Ok((project, result)) => {
                            initializing.remove(&project);
                            match result {
                                Ok(manager) => {
                                    if !self.declared().is_ok_and(|declared|declared.contains(&project)) {manager.pause_admission();}
                                    let (stop, stopped) = watch::channel(false);
                                    stops.insert(project.clone(), stop);
                                    managers.spawn(async move { (project, manager.serve(stopped).await) });
                                }
                                Err(error) => tracing::warn!(%project, error = %format_args!("{error:#}"), "project preparation failed; other projects remain available"),
                            }
                        }
                        Err(error) => tracing::warn!(%error, "project preparation task failed"),
                    }
                }
                Some(result) = managers.join_next(), if !managers.is_empty() => {
                    if let Ok((project, result)) = result {
                        if let Some(state) = self.projects.get(&project) {
                            match state.write().map_err(|_| StatePoisoned("project releases")) {
                                Ok(mut state) => state.releases = None,
                                Err(error) => tracing::warn!(%project, %error, "project removal failed"),
                            }
                        }
                        stops.remove(&project);
                        tracing::warn!(%project, ?result, "project execution stopped");
                    }
                }
                _ = ticks.tick() => {
                    let declared = match self.declared() { Ok(declared) => declared, Err(error) => {tracing::warn!(%error, "project projection unavailable"); continue;} };
                    // Cache entries initialize independently; interrupted eviction
                    // only retains extra reusable bytes and cannot corrupt a cell.
                    self.bytes.lock().unwrap_or_else(|error| error.into_inner()).retain(|_, cell| Arc::strong_count(cell) > 1 || cell.get().is_some_and(|bytes| Arc::strong_count(bytes) > 1));
                    for (project, state) in self.projects.entries().iter() {
                        match state.read().map_err(|_| StatePoisoned("project releases")) {
                            Ok(state) => if !declared.contains(project) {
                                if let Some(manager) = &state.releases {manager.pause_admission();}
                            },
                            Err(error) => tracing::warn!(%project, %error, "project admission update failed"),
                        }
                    }
                    for project in declared {
                        if !stops.contains_key(&project) && initializing.insert(project.clone()) {
                            let environment = Arc::clone(self);
                            initializers.spawn(async move { let result = environment.prepare(&project).await; (project,result) });
                        }
                    }
                }
            }
        }
        initializers.shutdown().await;
        for stop in stops.values() {
            stop.send_replace(true);
        }
        while managers.join_next().await.is_some() {}
        Ok(())
    }
}

#[async_trait::async_trait]
impl wamn_execution_host::file_route::ResidencyReader for Environment {
    async fn residency(&self) -> Residency {
        let mut declared = match self.declared() {
            Ok(projects) => projects,
            Err(_) => return Residency::new(),
        };
        let mut projects: BTreeMap<_, _> = self
            .projects
            .entries()
            .iter()
            .map(|(name, state)| (name.clone(), Arc::clone(state)))
            .collect();
        declared.extend(projects.keys().cloned());
        let mut report = Residency::new();
        let mut reads = tokio::task::JoinSet::new();
        for project in declared {
            let manager = match projects.remove(&project) {
                Some(state) => state
                    .read()
                    .map(|state| state.releases.as_ref().map(Arc::clone))
                    .map_err(|_| StatePoisoned("project releases")),
                None => Ok(None),
            };
            if let Ok(Some(manager)) = manager {
                reads.spawn(async move {
                    let state = match manager.residency().await {
                        Ok(releases) => ProjectResidency {
                            releases,
                            error: None,
                        },
                        Err(_) => ProjectResidency {
                            error: Some("project runs are unavailable".into()),
                            ..ProjectResidency::default()
                        },
                    };
                    (project, state)
                });
            } else {
                report.insert(
                    project,
                    ProjectResidency {
                        error: Some(match manager {
                            Err(error) => error.to_string(),
                            _ => "project is not ready".into(),
                        }),
                        ..ProjectResidency::default()
                    },
                );
            }
        }
        while let Some(read) = reads.join_next().await {
            match read {
                Ok((project, state)) => {
                    report.insert(project, state);
                }
                Err(error) => tracing::warn!(%error,"project residency read failed"),
            }
        }
        report
    }
}

#[async_trait::async_trait]
impl wamn_engine::flow_http_routing::EnvironmentStatus for Environment {
    async fn inactive(&self, key: &str) -> anyhow::Result<bool> {
        let coordinate = wamn_registry::name::parse_project_env_key(key)
            .context("invalid request project deployment")?;
        if Some(coordinate.org.as_str()) != self.args.org.as_deref()
            || Some(coordinate.env.as_str()) != self.args.environment.as_deref()
            || !self.declared()?.contains(&coordinate.project)
        {
            return Ok(true);
        }
        self.postgres
            .environment_inactive(&coordinate.project, key)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn poisoned_project_keeps_other_project_usable() {
        let projects = Arc::new(Projects::default());
        let broken = projects.entry("broken");
        projects
            .entry("healthy")
            .write()
            .unwrap()
            .credentials
            .insert(
                "healthy".into(),
                ProjectConfig {
                    credentials: wamn_postgres_capability::ClassCredentials::default().with_class(
                        AuthorityClass::ExecutorPlatform,
                        "postgres://executor:secret@localhost/healthy",
                    ),
                    guest_pool_max_size: 1,
                    platform_pool_max_size: 1,
                    wait_timeout_ms: 1000,
                    statement_timeout_ms: 1000,
                    row_limit: 1,
                },
            );
        let credentials = Arc::new(Credentials(Arc::clone(&projects)));
        std::thread::scope(|scope| {
            let failed = scope.spawn(|| {
                let _index = projects.0.write().unwrap();
                let _project = broken.write().unwrap();
                panic!("interrupt project update");
            });
            assert!(failed.join().is_err());
        });

        let error = credentials
            .resolve("broken", AuthorityClass::ExecutorPlatform, None)
            .expect_err("poisoned credentials must refuse resolution");
        assert!(error.is::<StatePoisoned>());
        assert!(
            credentials
                .resolve("healthy", AuthorityClass::ExecutorPlatform, None)
                .unwrap()
                .is_some()
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(wamn_execution_host::file_route::serve_selected(
            listener,
            move |project| {
                credentials
                    .resolve(project, AuthorityClass::ExecutorPlatform, None)
                    .map(|_| None)
            },
            Arc::new(wamn_engine::flow_http_routing::FlowHttpRouting::new(
                Arc::default(),
            )),
            None,
        ));
        let client = wamn_package_artifact::http_client::HttpClient::builder()
            .build()
            .unwrap();
        for (host, status) in [("broken", 503), ("healthy", 404)] {
            let response = client
                .get(format!("http://{address}/api/orders/files"))
                .header("Host", host)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
        }
        task.abort();
        let _ = task.await;
    }
}
