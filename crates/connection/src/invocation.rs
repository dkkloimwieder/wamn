//! Application and connection authority for one invocation scope.

use crate::connection::ConnectionInvocation;
use std::sync::Arc;

/// Immutable application facts and project-owned connection readers for one deployment.
#[derive(Debug)]
pub struct ReleaseContext {
    pub release: Arc<wamn_app::app_config::AppConfig>,
    pub connections: Arc<crate::connection_facts::ConnectionFacts>,
}

impl ReleaseContext {
    pub fn new(
        release: Arc<wamn_app::app_config::AppConfig>,
        connections: Arc<crate::connection_facts::ConnectionFacts>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            &wamn_registry::name::project_env_key(&release.org, &release.project, &release.env)
                == connections.project_env_key()
                && &release.env == connections.env(),
            "connection release target mismatch"
        );
        Ok(Self {
            release,
            connections,
        })
    }
}

/// Connection facts installed by the host after admission.
#[derive(Debug, Default)]
pub struct Scope {
    pub principal: Option<wamn_identity_protocol::PrincipalId>,
    pub context: Option<Arc<ReleaseContext>>,
    pub invocation: Option<ConnectionInvocation>,
}

impl wamn_app::invocation_authority::InvocationAuthority for Scope {}
