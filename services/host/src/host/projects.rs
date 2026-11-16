//! Project resources share the runtime, never their database or event authority.

use super::{HostArgs, releases};
use anyhow::Context as _;
use std::collections::HashMap;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::RwLock;
use wamn_authentication::route_authentication::PlatformRouteAuthenticator;
use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, RouteAuthenticator,
    authentication_unavailable,
};
use wamn_sql::AuthorityClass;
use wamn_sql::WamnPostgres;
use wamn_sql::{
    CredentialProvider, ResolvedCredential, StaticCredentialProvider,
};

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
    authentication: Option<(
        wamn_registry::ProjectEnvKey,
        Arc<PlatformRouteAuthenticator>,
    )>,
    releases: Option<Arc<releases::Releases>>,
}

#[derive(Default)]
pub(super) struct Projects(RwLock<BTreeMap<wamn_registry::ProjectId, Arc<RwLock<Project>>>>);

impl Projects {
    fn entries(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, BTreeMap<wamn_registry::ProjectId, Arc<RwLock<Project>>>>
    {
        // The index contains independent, fully constructed handles. Insertion
        // cannot corrupt other entries; project data changes under its own lock.
        self.0.read().unwrap_or_else(|error| error.into_inner())
    }

    fn get(&self, project: &wamn_registry::ProjectId) -> Option<Arc<RwLock<Project>>> {
        self.entries().get(project).map(Arc::clone)
    }

}

pub(super) struct Credentials(pub(super) Arc<Projects>);
impl CredentialProvider for Credentials {
    fn resolve(
        &self,
        project: &wamn_registry::ProjectId,
        class: AuthorityClass,
        project_env_key: Option<&wamn_registry::ProjectEnvKey>,
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
        let coordinate = wamn_registry::ProjectEnvKey::triple(&request.target.project_env_key);
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
    pub postgres: Arc<WamnPostgres>,
    projects: Arc<Projects>,
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
        postgres: Arc<WamnPostgres>,
    ) -> Self {
        Self {
            args,
            projects,
            postgres,
        }
    }

    fn declared(&self) -> anyhow::Result<HashSet<wamn_registry::ProjectId>> {
        let path = self
            .args
            .projects
            .as_ref()
            .context("environment has no project projection")?;
        let projects: HashSet<wamn_registry::ProjectId> = serde_json::from_slice(
            &std::fs::read(path).context("read environment project projection")?,
        )?;
        Ok(projects)
    }

    pub async fn for_workload(
        &self,
        annotations: &HashMap<String, String>,
    ) -> anyhow::Result<Arc<releases::ResidentRelease>> {
        let key = annotations
            .get("wamn.project_env_key")
            .context("workload has no project deployment")?
            .parse::<wamn_registry::ProjectEnvKey>()?;
        let coordinate = key.triple();
        anyhow::ensure!(
            Some(&coordinate.org) == self.args.org.as_ref()
                && Some(&coordinate.env) == self.args.environment.as_ref()
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

}

#[async_trait::async_trait]
impl wamn_engine::flow_http_routing::EnvironmentStatus for Environment {
    async fn inactive(&self, key: &wamn_registry::ProjectEnvKey) -> anyhow::Result<bool> {
        let coordinate = key.triple();
        if Some(&coordinate.org) != self.args.org.as_ref()
            || Some(&coordinate.env) != self.args.environment.as_ref()
            || !self.declared()?.contains(&coordinate.project)
        {
            return Ok(true);
        }
        self.postgres
            .environment_inactive(&coordinate.project, key)
            .await
    }
}
