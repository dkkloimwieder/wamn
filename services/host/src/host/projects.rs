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
use wamn_control_provision::{SystemReader, project_env_database_name, system_reader_login};
use wamn_engine::artifact_source::ArtifactSource;
use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, RouteAuthenticator,
    authentication_unavailable,
};
use wamn_engine::release_manifest::ReleaseTarget;
use wamn_execution_host::OperationHostConfig;
use wamn_kubernetes::environment_state::{ProjectResidency, Residency};
use wamn_registry::{Triple, environment_nats_user, project_env_key};
use wamn_runtime::auth_cache::replicate;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::connection_facts::ConnectionFacts;
use wamn_runtime::connection_memory::{self, ConnectionMemory};
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::route_authentication::PlatformRouteAuthenticator;
use wamn_runtime::plugins::wamn_jetstream::WamnJetstreamConfig;
use wamn_runtime::plugins::wamn_postgres::AuthorityClass;
use wamn_runtime::plugins::wamn_postgres::{
    CredentialProvider, ProjectConfig, ResolvedCredential, StaticCredentialProvider,
    WamnPostgresConfig,
};
use wamn_runtime::plugins::{WamnJetstream, WamnPostgres};
use wash_runtime::plugin;

#[derive(Default)]
pub(super) struct Credentials(RwLock<StaticCredentialProvider>);
impl CredentialProvider for Credentials {
    fn resolve(
        &self,
        project: &str,
        class: AuthorityClass,
        project_env_key: Option<&str>,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        self.0
            .read()
            .expect("project credentials lock poisoned")
            .resolve(project, class, project_env_key)
    }
}

#[derive(Default, Debug)]
pub(super) struct Authentication(RwLock<HashMap<String, Arc<PlatformRouteAuthenticator>>>);
#[async_trait::async_trait]
impl RouteAuthenticator for Authentication {
    async fn authenticate(
        &self,
        request: AuthenticationRequest<'_>,
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let authenticator = self
            .0
            .read()
            .expect("project authenticators lock poisoned")
            .get(&request.target.project_env_key)
            .map(Arc::clone)
            .ok_or_else(authentication_unavailable)?;
        authenticator.authenticate(request).await
    }
}

pub(super) struct Environment {
    pub args: Arc<HostArgs>,
    pub credentials: Arc<Credentials>,
    pub authentication: Arc<Authentication>,
    pub engine: Arc<wash_runtime::engine::Engine>,
    pub postgres: Arc<WamnPostgres>,
    pub transport: Arc<HttpTransport>,
    pub logging: Arc<plugin::wasi_logging::TracingLogger>,
    pub source: Arc<dyn ArtifactSource>,
    pub router_owner: String,
    projects: RwLock<BTreeMap<String, Arc<releases::Releases>>>,
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
        credentials: Arc<Credentials>,
        authentication: Arc<Authentication>,
        engine: Arc<wash_runtime::engine::Engine>,
        postgres: Arc<WamnPostgres>,
        transport: Arc<HttpTransport>,
        logging: Arc<plugin::wasi_logging::TracingLogger>,
        source: Arc<dyn ArtifactSource>,
        router_owner: String,
    ) -> Self {
        Self {
            args,
            credentials,
            authentication,
            engine,
            postgres,
            transport,
            logging,
            source,
            router_owner,
            projects: RwLock::default(),
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
        let project = self
            .projects
            .read()
            .expect("projects lock poisoned")
            .get(&coordinate.project)
            .map(Arc::clone)
            .context("workload project is not ready")?;
        project.for_workload(annotations).await
    }

    pub fn files(&self, host: &str) -> Option<Arc<wamn_execution_host::file_route::FileRoutes>> {
        self.projects
            .read()
            .expect("projects lock poisoned")
            .values()
            .find_map(|project| project.files(host))
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
        self.credentials
            .0
            .write()
            .expect("project credentials lock poisoned")
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
                Ok(Arc::new(wamn_runtime::oauth_encryption::Envelope::open(
                    &serde_json::from_str(value)?,
                )?))
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
        let leases = Arc::new(wamn_runtime::resource_lease::ResourceLeases::new(Some(
            &connection,
        ))?);
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
        self.authentication
            .0
            .write()
            .expect("project authenticators lock poisoned")
            .insert(key, authenticator);
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
                                    self.projects.write().expect("projects lock poisoned").insert(project.clone(), Arc::clone(&manager));
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
                        self.projects.write().expect("projects lock poisoned").remove(&project);
                        stops.remove(&project);
                        tracing::warn!(%project, ?result, "project execution stopped");
                    }
                }
                _ = ticks.tick() => {
                    let declared = match self.declared() { Ok(declared) => declared, Err(error) => {tracing::warn!(%error, "project projection unavailable"); continue;} };
                    self.bytes.lock().expect("release bytes lock poisoned").retain(|_, cell| Arc::strong_count(cell) > 1 || cell.get().is_some_and(|bytes| Arc::strong_count(bytes) > 1));
                    for (project, manager) in self.projects.read().expect("projects lock poisoned").iter() {
                        if !declared.contains(project) {manager.pause_admission();}
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
            .read()
            .expect("projects lock poisoned")
            .iter()
            .map(|(name, manager)| (name.clone(), Arc::clone(manager)))
            .collect();
        declared.extend(projects.keys().cloned());
        let mut report = Residency::new();
        let mut reads = tokio::task::JoinSet::new();
        for project in declared {
            if let Some(manager) = projects.remove(&project) {
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
                        error: Some("project is not ready".into()),
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
