//! Project resources share the runtime, never their database or event authority.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use wamn_authentication::route_authentication::PlatformRouteAuthenticator;
use wamn_engine::flow_http_routing::{
    AuthRejection, AuthenticatedCaller, AuthenticationRequest, RouteAuthenticator,
    authentication_unavailable,
};
use wamn_sql::AuthorityClass;
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
