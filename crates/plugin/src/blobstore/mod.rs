//! Blob storage resolves bindings under the shared invocation authority.

use std::collections::HashSet;
use std::sync::Arc;
use wamn_authority::effect_span;
use wamn_authority::effect_span::{EffectIdentity, record_graph};
use wamn_blobstore::Storage;
use wamn_blobstore::binding::BindingError;
use wamn_blobstore::store::ObjectContainer;
use wamn_engine::invocation_scope::InvocationScopes;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

/// The host registration for wasmcloud:blobstore.
pub const WAMN_BLOBSTORE_ID: &str = "wamn-blobstore";

use wamn_connection::connection::ConnectionInvocation;

mod bindings;
mod drain;
pub mod file_route;
mod host;
mod wit_error;

/// The WAMN blobstore host service.
pub struct WamnBlobstore {
    scopes: Arc<InvocationScopes>,
    storage: Arc<dyn Storage>,
}

impl std::fmt::Debug for WamnBlobstore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The vault is never rendered.
        formatter
            .debug_struct("WamnBlobstore")
            .finish_non_exhaustive()
    }
}

impl WamnBlobstore {
    /// Build the plugin with the host's invocation scopes and storage implementation.
    pub fn new(scopes: Arc<InvocationScopes>, storage: Arc<dyn Storage>) -> Self {
        Self { scopes, storage }
    }

    /// Read signing and effect authority from the invocation record.
    pub fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        crate::invocation::invocation(&self.scopes, component_id)
    }

    pub(super) async fn container_for(
        &self,
        component_id: &str,
        alias: &wamn_package::canonical_name::CanonicalName,
    ) -> Result<Arc<dyn ObjectContainer>, BindingError> {
        let resolve = async {
            let context = crate::invocation::context(&self.scopes, component_id)?;
            let invocation = self
                .invocation(component_id)
                .ok_or_else(|| anyhow::anyhow!("no connection invocation"))?;
            let binding = invocation
                .connection(&context.release, alias)
                .filter(|binding| binding.interface == "wasmcloud:blobstore")
                .ok_or_else(|| anyhow::anyhow!("no declared blobstore binding"))?;
            let package = &invocation.package_id;
            let read_only = binding
                .mode
                .read_only(package, &binding.name)
                .ok_or_else(|| anyhow::anyhow!("no connection mode"))?;
            let id = binding.connection_id.parse()?;
            self.storage
                .container(
                    &context.connections,
                    id,
                    &binding.integration_type,
                    read_only,
                )
                .await
        }
        .await;
        resolve.map_err(|error| {
            tracing::warn!(%error, component_id, %alias, "blobstore authority refused");
            BindingError::unauthorized()
        })
    }
}

pub(super) fn blobstore_span(
    plugin: &WamnBlobstore,
    component_id: &str,
    operation: &'static str,
) -> tracing::Span {
    let make_span = |identity: Option<&wamn_application::session_identity::SessionIdentity>| {
        effect_span!(
            "wamn.blobstore",
            EffectIdentity {
                project_env_key: identity
                    .as_ref()
                    .and_then(|identity| identity.project_env_key.as_ref()),
                project: identity
                    .as_ref()
                    .and_then(|identity| identity
                        .project
                        .as_ref()
                        .map(wamn_registry::ProjectId::as_str))
                    .unwrap_or(""),
                component: component_id,
            },
            None,
            effect.operation = operation,
        )
    };
    let span = plugin
        .scopes
        .read::<crate::scope_registry::Authority, _>(component_id, |record| {
            make_span(Some(&record.identity))
        })
        .unwrap_or_else(|| make_span(None));
    let invocation = plugin.invocation(component_id);
    record_graph(
        &span,
        invocation
            .as_ref()
            .map(wamn_connection::connection::ConnectionInvocation::effect_graph),
    );
    span
}

#[async_trait::async_trait]
impl HostPlugin for WamnBlobstore {
    fn id(&self) -> &'static str {
        WAMN_BLOBSTORE_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wasmcloud:blobstore/types@0.1.0"),
                WitInterface::from("wasmcloud:blobstore/container@0.1.0"),
                WitInterface::from("wasmcloud:blobstore/blobstore@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if interfaces.contains("wasmcloud", "blobstore", &[]) {
            add_to_linker(item.linker()?)?;
        }
        Ok(())
    }
}

/// Wire the blobstore host functions into a linker.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    bindings::wasmcloud::blobstore::types::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )?;
    bindings::wasmcloud::blobstore::container::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )?;
    bindings::wasmcloud::blobstore::blobstore::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )
}
