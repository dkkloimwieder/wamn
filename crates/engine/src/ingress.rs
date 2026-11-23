//! Host identity interface for the separate HTTP route component.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;
use wamn_application::application_config::ApplicationConfig;
use wamn_application::ingress::{
    self, AuthRejection, AuthenticatedCaller, EnvironmentStatus, Header, RouteAuthenticator,
    RoutePolicy, RouteProvider, authentication_unavailable,
};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, Resource};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::route_bindings::wamn::ingress::routing;

pub const INGRESS_ID: &str = "wamn-ingress";

/// Authoritative HTTP route supply for the ingress adapter.
pub struct IngressRouting {
    scopes: Arc<crate::invocation_scope::InvocationScopes>,
    authenticator: Option<Arc<dyn RouteAuthenticator>>,
    environment: Option<Arc<dyn EnvironmentStatus>>,
}
impl std::fmt::Debug for IngressRouting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngressRouting").finish_non_exhaustive()
    }
}
impl IngressRouting {
    pub fn new(scopes: Arc<crate::invocation_scope::InvocationScopes>) -> Self {
        Self {
            scopes,
            authenticator: None,
            environment: None,
        }
    }
    /// Read the application facts retained for the bound ingress component.
    pub fn application_config(&self, component_id: &str) -> Option<Arc<ApplicationConfig>> {
        crate::application::bound_application_config(&self.scopes, component_id)
    }

    /// Refuse every route while the host's environment is inactive.
    #[must_use]
    pub fn with_environment_status(mut self, environment: Arc<dyn EnvironmentStatus>) -> Self {
        self.environment = Some(environment);
        self
    }

    /// Hand every protected route to the host's credential mechanism.
    #[must_use]
    pub fn with_authenticator(mut self, authenticator: Arc<dyn RouteAuthenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Apply a host-resolved policy, including a fixed native host route.
    pub async fn authenticate_policy(
        &self,
        target: &wamn_registry::ProjectEnvKey,
        attachment_id: &wamn_package::canonical_name::CanonicalName,
        route: RoutePolicy<'_>,
        headers: &[Header],
        body: &[u8],
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        ingress::authenticate_policy(
            self.environment.as_deref(),
            self.authenticator.as_deref(),
            target,
            attachment_id,
            route,
            headers,
            body,
        )
        .await
    }

    async fn authenticate_accepted(
        &self,
        routes: &dyn RouteProvider,
        attachment_id: &wamn_package::canonical_name::CanonicalName,
        accepted: &Value,
        headers: &[Header],
        body: &[u8],
    ) -> Result<Option<AuthenticatedCaller>, AuthRejection> {
        let route = routes
            .route(attachment_id)
            .ok_or_else(authentication_unavailable)?;
        if route.policy != accepted {
            return Err(authentication_unavailable());
        }
        self.authenticate_policy(routes.target(), attachment_id, route, headers, body)
            .await
    }
}

#[async_trait::async_trait]
impl HostPlugin for IngressRouting {
    fn id(&self) -> &'static str {
        INGRESS_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:ingress/routing@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_unbind(&self, workload: &str, _: WitInterfaces<'_>) -> anyhow::Result<()> {
        self.scopes.revoke_workload(workload);
        Ok(())
    }

    /// `world` only decides whether this callback fires; the linker entry is made
    /// here or the guest's import resolves to nothing.
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "ingress", &["routing"]) {
            return Ok(());
        }
        if let Some(application) = crate::application::bind_application_config(&self.scopes, item)?
        {
            let routes = crate::application::WorkloadRoutes::read(
                &item.local_resources().config,
                application,
            )?;
            self.scopes.bind_routes(item.id(), Arc::new(routes));
        }
        add_to_linker(item.linker()?)?;
        Ok(())
    }
}

/// Link the routing import. A store without this plugin traps on each call.
pub fn add_to_linker(
    linker: &mut wash_runtime::wasmtime::component::Linker<SharedCtx>,
) -> wash_runtime::wasmtime::Result<()> {
    routing::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<IngressRouting>> {
    ctx.try_get_plugin::<IngressRouting>(INGRESS_ID)
}

impl<T: 'static + Send> routing::HostWithStore<T> for SharedCtx {
    async fn authenticate(
        accessor: &Accessor<T, Self>,
        attachment_id: String,
        accepted: String,
        headers: Vec<routing::Header>,
        body: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<
        Result<Option<Resource<AuthenticatedCaller>>, routing::AuthRejection>,
    > {
        let Ok(attachment_id) =
            attachment_id.parse::<wamn_package::canonical_name::CanonicalName>()
        else {
            return Ok(Err(authentication_unavailable().into()));
        };
        let headers: Vec<_> = headers
            .into_iter()
            .map(|routing::Header { name, value }| Header { name, value })
            .collect();
        let (plugin, release, trace) = accessor.with(|mut access| {
            let ctx = access.get();
            let trace = crate::invocation_trace::request_trace(&headers);
            let plugin = plugin_of(&ctx)?;
            let release = plugin.scopes.routes(&ctx.component_id);
            if release.is_some() {
                let lifetime = plugin
                    .scopes
                    .enter_trace(ctx.id.to_string(), trace.clone())
                    .map_err(|error| wash_runtime::wasmtime::Error::msg(error.to_string()))?;
                ctx.table.push(lifetime)?;
            }
            Ok::<_, wash_runtime::wasmtime::Error>((plugin, release, trace))
        })?;
        trace
            .run(async move {
                let Some(release) = release else {
                    return Ok(Err(authentication_unavailable().into()));
                };
                let accepted: Value = serde_json::from_str(&accepted)?;
                let caller = match plugin
                    .authenticate_accepted(
                        release.as_ref(),
                        &attachment_id,
                        &accepted,
                        &headers,
                        &body,
                    )
                    .await
                {
                    Ok(caller) => caller,
                    Err(rejection) => return Ok(Err(rejection.into())),
                };
                accessor.with(|mut access| {
                    Ok(Ok(caller
                        .map(|caller| access.get().table.push(caller))
                        .transpose()?))
                })
            })
            .await
    }
}

impl routing::Host for ActiveCtx<'_> {}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "`routing::HostAuthenticatedCaller` is generated by wit-bindgen and declares \
              `async fn`; dropping a caller awaits nothing but still has to match it"
)]
impl routing::HostAuthenticatedCaller for ActiveCtx<'_> {
    async fn drop(
        &mut self,
        caller: Resource<AuthenticatedCaller>,
    ) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(caller)?;
        Ok(())
    }
}

impl From<AuthRejection> for routing::AuthRejection {
    fn from(AuthRejection { status, code }: AuthRejection) -> Self {
        Self { status, code }
    }
}
