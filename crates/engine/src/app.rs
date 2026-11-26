//! Host request facts read from the route component's workload configuration.

use crate::invocation_scope::InvocationScopes;
use std::collections::HashMap;
use std::sync::Arc;
use wamn_app::app_config::{APP_CONFIG, AppConfig};
use wamn_app::ingress::{RoutePolicy, RouteProvider};
use wamn_app::invocation_authority::InvocationAuthority;
use wamn_routes::HTTP_ROUTES_CONFIG;
use wamn_routes::http_route_data::RouteDefinition;
use wash_runtime::engine::workload::WorkloadItem;

#[derive(Default)]
struct BoundApp(Option<Arc<AppConfig>>);

impl InvocationAuthority for BoundApp {}

/// Decode the application once, on the first bind that needs its configuration.
pub fn bind_app_config(
    scopes: &InvocationScopes,
    item: &WorkloadItem<'_>,
) -> anyhow::Result<Option<Arc<AppConfig>>> {
    let Some(document) = item.local_resources().config.get(APP_CONFIG) else {
        return Ok(None);
    };
    let app = scopes.update::<BoundApp, _>(item.id(), |bound| {
        if let Some(app) = &bound.0 {
            return Ok(Arc::clone(app));
        }
        let app = Arc::new(serde_json::from_str(document)?);
        bound.0 = Some(Arc::clone(&app));
        Ok::<_, serde_json::Error>(app)
    });
    match app {
        Ok(app) => {
            scopes.bind_workload(item.id(), item.workload_id());
            Ok(Some(app))
        }
        Err(error) => {
            scopes.revoke(item.id());
            Err(error.into())
        }
    }
}

/// Read the configuration retained during component binding.
pub fn bound_app_config(scopes: &InvocationScopes, component_id: &str) -> Option<Arc<AppConfig>> {
    scopes.read::<BoundApp, _>(component_id, |bound| bound.0.as_ref().map(Arc::clone))?
}

/// The application and route facts shared by host request readers.
#[derive(Debug)]
pub struct WorkloadRoutes {
    pub app: Arc<AppConfig>,
    pub routes: Vec<RouteDefinition>,
    pub project_env_key: wamn_registry::ProjectEnvKey,
}

impl WorkloadRoutes {
    /// Read the HTTP route table for the bound application.
    pub fn read(config: &HashMap<String, String>, app: Arc<AppConfig>) -> anyhow::Result<Self> {
        let routes = serde_json::from_str(
            config
                .get(HTTP_ROUTES_CONFIG)
                .ok_or_else(|| anyhow::anyhow!("application workload has no HTTP route table"))?,
        )?;
        let project_env_key = wamn_registry::project_env_key(&app.org, &app.project, &app.env);
        Ok(Self {
            app,
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
        self.app
            .components
            .iter()
            .find_map(|component| component.operations.get(route.sealed_operation_id.as_str()))?;
        Some(RoutePolicy {
            policy: &route.auth_policy,
        })
    }
}
