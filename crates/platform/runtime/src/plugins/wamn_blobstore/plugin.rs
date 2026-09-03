//! The blobstore host plugin: registration, invocation facts, and the live
//! binding lookup.
//!
//! Mirrors `ConnectionHttp`'s shape deliberately. It keeps its OWN invocation
//! registry rather than reading the HTTP plugin's, because a second reader on
//! one registry is the defect class `wamn-0h0g.21.9` records, and two sibling
//! plugins should not couple over something neither owns. Extracting a shared
//! registry is the right end state and is deferred to capability three
//! (`wamn-jpxo`, and inherited by the seam template) — two consumers is the
//! wrong count to extract at.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use super::binding::{self, BindingError};
use super::store::BoundContainer;
use crate::plugins::connection_http::{ConnectionExecutionClosure, ConnectionInvocation};
use crate::plugins::wamn_credentials::WamnCredentials;
use crate::plugins::wamn_postgres::{ConnectionEffectLookup, WamnPostgres};

/// Plugin id, as the host registry knows it.
pub const WAMN_BLOBSTORE_ID: &str = "wamn-blobstore";

/// The WAMN blobstore capability.
pub struct WamnBlobstore {
    postgres: Arc<WamnPostgres>,
    vault: Arc<WamnCredentials>,
    tenant: Box<str>,
    pub(super) project: Box<str>,
    /// Component-store owner id to the invocation currently using that pooled
    /// instance. The driver binds before `handler.run` and revokes before
    /// returning the instance to the pool.
    invocations: RwLock<HashMap<String, ConnectionInvocation>>,
}

impl std::fmt::Debug for WamnBlobstore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The vault is never rendered.
        formatter
            .debug_struct("WamnBlobstore")
            .field("tenant", &self.tenant)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl WamnBlobstore {
    /// Build the plugin over the connection-authority reader and the vault.
    pub fn new(
        postgres: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        tenant: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
    ) -> Self {
        Self {
            postgres,
            vault,
            tenant: tenant.into(),
            project: project.into(),
            invocations: RwLock::new(HashMap::new()),
        }
    }

    /// Bind the exact invocation facts before entering one pooled component.
    ///
    /// A still-bound owner refuses rather than silently replacing leaked
    /// state — the same rule the HTTP plugin holds, for the same reason: a
    /// stale invocation would authorize the next guest against the previous
    /// one's wiring position.
    pub fn bind_invocation(
        &self,
        component_id: &str,
        invocation: ConnectionInvocation,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!component_id.is_empty(), "component id must be present");
        let mut invocations = self
            .invocations
            .write()
            .map_err(|_| anyhow::anyhow!("blobstore invocation registry is poisoned"))?;
        anyhow::ensure!(
            !invocations.contains_key(component_id),
            "component {component_id} still holds a bound blobstore invocation"
        );
        invocations.insert(component_id.to_owned(), invocation);
        Ok(())
    }

    /// Release the invocation when the instance returns to the pool.
    pub fn revoke_invocation(&self, component_id: &str) {
        if let Ok(mut invocations) = self.invocations.write() {
            invocations.remove(component_id);
        }
    }

    /// The invocation currently bound to one pooled instance.
    #[must_use]
    pub fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        self.invocations
            .read()
            .ok()
            .and_then(|invocations| invocations.get(component_id).cloned())
    }

    /// Resolve one store alias into a confined container.
    ///
    /// The guest names its own declared STORE ALIAS, never a container: the
    /// container is environment-owned, so a guest that had to name it would
    /// need the coordinate the confinement exists to keep from it.
    pub(super) async fn container_for(
        &self,
        component_id: &str,
        store_alias: &str,
    ) -> Result<BoundContainer, BindingError> {
        let invocation = self
            .invocation(component_id)
            .ok_or(BindingError::Unauthorized)?;
        let wiring_version =
            i32::try_from(invocation.wiring_version).map_err(|_| BindingError::Unauthorized)?;

        // The release-closure path needs the mounted serving manifest to supply
        // the effective release and environment. This plugin holds no manifest
        // weld yet, so a released closure REFUSES rather than guessing at
        // coordinates that decide which binding authorizes. Fail-closed until
        // the weld is wired.
        let (effective_release_id, environment) = match &invocation.closure {
            ConnectionExecutionClosure::Candidate {
                effective_release_id,
                environment,
                ..
            } => (
                i32::try_from(*effective_release_id).map_err(|_| BindingError::Unauthorized)?,
                environment.clone(),
            ),
            ConnectionExecutionClosure::Released => return Err(BindingError::Unauthorized),
        };

        let snapshot = self
            .postgres
            .connection_effect_snapshot(
                component_id,
                &self.project,
                &self.tenant,
                &ConnectionEffectLookup {
                    package_id: &invocation.package_id,
                    effective_release_id,
                    environment: &environment,
                    wiring_id: &invocation.wiring_id,
                    wiring_version,
                    node_id: &invocation.node_id,
                    component_digest: &invocation.component_digest,
                    store_alias,
                    candidate_binding: None,
                },
            )
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "blobstore connection authority snapshot failed");
                BindingError::Unauthorized
            })?
            .ok_or(BindingError::Unauthorized)?;

        let bound = binding::resolve(&snapshot)?;
        let secret = self
            .vault
            .lookup(&self.project, &bound.credential_handle)
            .ok_or(BindingError::NoCredential)?;
        let store = build_store(&bound, &secret).map_err(|error| {
            tracing::warn!(error = %error, "blobstore client construction failed");
            BindingError::Unauthorized
        })?;
        Ok(BoundContainer::new(store, bound.container, bound.prefix))
    }
}

/// Build the S3 client for one binding.
///
/// The secret enters HERE and nowhere else: it is handed straight to the
/// signer and is never stored on [`BoundContainer`], never logged, and never
/// reachable from a guest-visible structure.
fn build_store(
    bound: &binding::BlobstoreBinding,
    secret: &str,
) -> anyhow::Result<Arc<dyn ObjectStore>> {
    let credential: serde_json::Value = serde_json::from_str(secret)
        .map_err(|_| anyhow::anyhow!("object-store credential is not JSON"))?;
    let access_key = credential
        .get("ACCESS_KEY_ID")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("object-store credential lacks ACCESS_KEY_ID"))?;
    let secret_key = credential
        .get("ACCESS_SECRET_KEY")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("object-store credential lacks ACCESS_SECRET_KEY"))?;

    let store = AmazonS3Builder::new()
        .with_endpoint(&bound.endpoint)
        .with_bucket_name(&bound.container)
        .with_access_key_id(access_key)
        .with_secret_access_key(secret_key)
        .with_region("us-east-1")
        .with_allow_http(bound.endpoint.starts_with("http://"))
        .with_virtual_hosted_style_request(false)
        .build()?;
    Ok(Arc::new(store))
}

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
}

/// Wire the blobstore host functions into a linker.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    super::bindings::wasmcloud::blobstore::types::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )?;
    super::bindings::wasmcloud::blobstore::container::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )?;
    super::bindings::wasmcloud::blobstore::blobstore::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )
}
