//! Host request facts read from the route component's workload configuration.

use crate::invocation_scope::InvocationScopes;
use wamn_application::application_config::{APPLICATION_CONFIG, ApplicationConfig};
use wamn_application::ingress::{RoutePolicy, RouteProvider};
use wamn_application::invocation_authority::InvocationAuthority;
use std::collections::HashMap;
use std::sync::Arc;
use wamn_routes::HTTP_ROUTES_CONFIG;
use wamn_routes::http_route_data::RouteDefinition;
use wash_runtime::engine::workload::WorkloadItem;

#[derive(Default)]
struct BoundApplication(Option<Arc<ApplicationConfig>>);

impl InvocationAuthority for BoundApplication {}

/// Decode the application once, on the first bind that needs its configuration.
pub fn bind_application_config(
    scopes: &InvocationScopes,
    item: &WorkloadItem<'_>,
) -> anyhow::Result<Option<Arc<ApplicationConfig>>> {
    let Some(document) = item.local_resources().config.get(APPLICATION_CONFIG) else {
        return Ok(None);
    };
    let application = scopes.update::<BoundApplication, _>(item.id(), |bound| {
        if let Some(application) = &bound.0 {
            return Ok(Arc::clone(application));
        }
        let application = Arc::new(serde_json::from_str(document)?);
        bound.0 = Some(Arc::clone(&application));
        Ok::<_, serde_json::Error>(application)
    });
    match application {
        Ok(application) => {
            scopes.bind_workload(item.id(), item.workload_id());
            Ok(Some(application))
        }
        Err(error) => {
            scopes.revoke(item.id());
            Err(error.into())
        }
    }
}

/// Read the configuration retained during component binding.
pub fn bound_application_config(
    scopes: &InvocationScopes,
    component_id: &str,
) -> Option<Arc<ApplicationConfig>> {
    scopes.read::<BoundApplication, _>(component_id, |bound| bound.0.as_ref().map(Arc::clone))?
}

/// The application and route facts shared by host request readers.
#[derive(Debug)]
pub struct WorkloadRoutes {
    pub application: Arc<ApplicationConfig>,
    pub routes: Vec<RouteDefinition>,
    pub project_env_key: wamn_registry::ProjectEnvKey,
}

impl WorkloadRoutes {
    /// Read the HTTP route table for the bound application.
    pub fn read(
        config: &HashMap<String, String>,
        application: Arc<ApplicationConfig>,
    ) -> anyhow::Result<Self> {
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
        Ok(Self {
            application,
            routes,
            project_env_key,
        })
    }
}

impl RouteProvider for WorkloadRoutes {
    fn target(&self) -> &wamn_registry::ProjectEnvKey {
        &self.project_env_key
    }

    fn route(&self, id: &wamn_package::canonical_name::CanonicalName) -> Option<RoutePolicy<'_>> {
        let route = self
            .routes
            .iter()
            .find(|route| route.attachment_id == id.as_str())?;
        self.application
            .components
            .iter()
            .find_map(|component| component.operations.get(route.sealed_operation_id.as_str()))?;
        Some(RoutePolicy {
            policy: &route.auth_policy,
        })
    }
}
