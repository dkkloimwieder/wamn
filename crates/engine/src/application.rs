//! Host request facts read from the route component's workload configuration.

use crate::flow_http_routing::{RoutePolicy, RouteProvider};
use std::collections::HashMap;
use wamn_catalog::application_config::{
    APPLICATION_CONFIG, ApplicationConfig, ApplicationOperationType,
};
use wamn_catalog::http_routes::{HTTP_ROUTES_CONFIG, RouteDefinition};

/// The application and route facts shared by host request readers.
#[derive(Debug)]
pub struct WorkloadRoutes {
    pub application: ApplicationConfig,
    pub routes: Vec<RouteDefinition>,
    pub project_env_key: wamn_registry::ProjectEnvKey,
}

impl WorkloadRoutes {
    /// Read the deploy's two config documents, when this component carries them.
    pub fn read(config: &HashMap<String, String>) -> anyhow::Result<Option<Self>> {
        let Some(application) = config.get(APPLICATION_CONFIG) else {
            return Ok(None);
        };
        let application: ApplicationConfig = serde_json::from_str(application)?;
        let routes = serde_json::from_str(
            config
                .get(HTTP_ROUTES_CONFIG)
                .ok_or_else(|| anyhow::anyhow!("application workload has no HTTP route table"))?,
        )?;
        let project_env_key = wamn_registry::project_env_key(
            &application.org,
            &application.project,
            &application.environment,
        );
        Ok(Some(Self {
            application,
            routes,
            project_env_key,
        }))
    }
}

impl RouteProvider for WorkloadRoutes {
    fn target(&self) -> &wamn_registry::ProjectEnvKey {
        &self.project_env_key
    }

    fn route(&self, id: &wamn_catalog::canonical_name::CanonicalName) -> Option<RoutePolicy<'_>> {
        let route = self
            .routes
            .iter()
            .find(|route| route.attachment_id == id.as_str())?;
        let operation =
            self.application.components.iter().find_map(|component| {
                component.operations.get(route.sealed_operation_id.as_str())
            })?;
        Some(RoutePolicy {
            policy: &route.auth_policy,
            read: operation.type_ == Some(ApplicationOperationType::Read),
        })
    }
}
