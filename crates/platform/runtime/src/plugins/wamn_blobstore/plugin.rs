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

use crate::plugins::effect_span::{EffectIdentity, EffectWiring, effect_span, record_wiring};

use super::binding::{self, BindingError};
use super::store::BoundContainer;
use wamn_catalog::ServingManifest;

use crate::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionInvocation, authorize_release_closure,
};
use crate::plugins::wamn_credentials::WamnCredentials;
use crate::plugins::wamn_postgres::{ConnectionEffectLookup, WamnPostgres};
use crate::release_manifest::ReleaseManifestWeld;

/// Plugin id, as the host registry knows it.
pub const WAMN_BLOBSTORE_ID: &str = "wamn-blobstore";

/// The WAMN blobstore capability.
pub struct WamnBlobstore {
    postgres: Arc<WamnPostgres>,
    vault: Arc<WamnCredentials>,
    pub(super) tenant: Box<str>,
    pub(super) project: Box<str>,
    /// The mounted, digest-verified serving manifest, when this host serves a
    /// release. A CANDIDATE closure carries its effective release and
    /// environment in the invocation; a RELEASED one does not, and this is the
    /// only thing that can supply them without guessing at the coordinates
    /// that decide which binding authorizes.
    release: Option<Arc<ReleaseManifestWeld>>,
    /// Component-store owner id to the invocation currently using that pooled
    /// instance. The driver binds before `handler.run` and revokes before
    /// returning the instance to the pool.
    invocations: RwLock<HashMap<String, ConnectionInvocation>>,
}

/// The effective release and environment one invocation authorizes under.
///
/// A CANDIDATE closure states them itself. A RELEASED one takes them from the
/// mounted serving manifest, after TWO CHECKS — because a mounted manifest is
/// an input like any other:
///
/// - it must belong to THIS tenant, or a manifest served for another one would
///   hand a guest an effective release under which some other tenant's binding
///   authorizes;
/// - it must actually CONTAIN the package the invocation claims, or a release
///   that never shipped this package would still supply coordinates for it.
///
/// Pure and separate from the plugin, so the decision gating every released
/// effect can be asserted without a database or an object store.
fn release_coordinates(
    invocation: &ConnectionInvocation,
    manifest: Option<&ServingManifest>,
    tenant: &str,
) -> Result<(i32, String), BindingError> {
    match (&invocation.closure, manifest) {
        (
            ConnectionExecutionClosure::Candidate {
                effective_release_id,
                environment,
                ..
            },
            None,
        ) => Ok((
            i32::try_from(*effective_release_id).map_err(|_| BindingError::Unauthorized)?,
            environment.clone(),
        )),
        (ConnectionExecutionClosure::Released, Some(manifest)) => {
            if manifest.release.tenant_id != tenant
                || !manifest
                    .release
                    .packages
                    .iter()
                    .any(|package| package.package_id() == invocation.package_id.as_str())
            {
                return Err(BindingError::Unauthorized);
            }
            Ok((
                i32::try_from(manifest.release.effective_release_id.get())
                    .map_err(|_| BindingError::Unauthorized)?,
                manifest.release.environment.clone(),
            ))
        }
        // A released closure with no mounted manifest, or a candidate handed
        // one, is a caller mismatch rather than a policy question. It refuses
        // instead of picking whichever arm looks closer, because guessing here
        // decides which binding authorizes.
        _ => Err(BindingError::Unauthorized),
    }
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
        release: Option<Arc<ReleaseManifestWeld>>,
    ) -> Self {
        Self {
            postgres,
            vault,
            tenant: tenant.into(),
            project: project.into(),
            release,
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
        // EVERY REFUSAL NAMES ITSELF. The guest sees one opaque error and the
        // router logs one context line, so a refusal that stays silent here
        // is undiagnosable from any evidence -- six cluster runs' worth
        // (wamn-362o.45). The warn carries the alias and component, never
        // the credential.
        let refused = |reason: &'static str| {
            tracing::warn!(
                store_alias,
                component_id,
                reason,
                "blobstore binding refused"
            );
            BindingError::Unauthorized
        };
        let invocation = self
            .invocation(component_id)
            .ok_or_else(|| refused("no invocation is registered for the component"))?;
        let wiring_version = i32::try_from(invocation.wiring_version)
            .map_err(|_| refused("wiring version does not fit the authority's column"))?;
        let released_manifest = match &invocation.closure {
            ConnectionExecutionClosure::Released => Some(
                self.release
                    .as_deref()
                    .ok_or_else(|| refused("a released closure with no release manifest mounted"))?
                    .manifest(),
            ),
            ConnectionExecutionClosure::Candidate { .. } => None,
        };
        let (effective_release_id, environment) =
            release_coordinates(&invocation, released_manifest, &self.tenant).map_err(|_| {
                refused("release coordinates: tenant, package or closure kind disagree with the manifest")
            })?;
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
            .ok_or_else(|| {
                tracing::warn!(
                    store_alias,
                    component_id,
                    package_id = %invocation.package_id,
                    effective_release_id,
                    environment = %environment,
                    wiring_id = %invocation.wiring_id,
                    wiring_version,
                    node_id = %invocation.node_id,
                    component_digest = %invocation.component_digest,
                    "blobstore binding refused: the connection authority holds no binding for this closure"
                );
                BindingError::Unauthorized
            })?;
        if let Some(manifest) = released_manifest {
            authorize_release_closure(manifest, &invocation, &snapshot)
                .map_err(|_| refused("the release closure does not carry this component and wiring"))?;
        }
        let bound = binding::resolve(&snapshot).map_err(|error| {
            tracing::warn!(store_alias, component_id, error = %error, "blobstore binding refused: the binding does not resolve");
            error
        })?;
        let secret = self
            .vault
            .lookup(&self.project, &bound.credential_handle)
            .ok_or_else(|| {
                tracing::warn!(
                    store_alias,
                    component_id,
                    credential_handle = %bound.credential_handle,
                    project = %self.project,
                    "blobstore binding refused: no credential under this project for the handle"
                );
                BindingError::NoCredential
            })?;
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

/// The `wamn.blobstore` span over one guest object-store effect.
///
/// Carries the shared identity vocabulary plus the wiring position this
/// component was invoked at, copied from the host-attested invocation — never
/// anything the guest sent. A pooled instance with no invocation bound records
/// the wiring keys empty, which says "about to be refused" where a missing
/// field would look like lost instrumentation.
///
/// The object KEY is deliberately absent: keys are guest-authored and can carry
/// tenant data, and a span is a wider audience than the effect itself.
pub(super) fn blobstore_span(
    plugin: &WamnBlobstore,
    component_id: &str,
    operation: &'static str,
) -> tracing::Span {
    let span = effect_span!(
        "wamn.blobstore",
        EffectIdentity {
            tenant: &plugin.tenant,
            project: &plugin.project,
            component: component_id,
        },
        None,
        effect.operation = operation,
    );
    let invocation = plugin.invocation(component_id);
    record_wiring(
        &span,
        invocation.as_ref().map(|invocation| EffectWiring {
            wiring_id: &invocation.wiring_id,
            wiring_version: invocation.wiring_version,
            node_id: &invocation.node_id,
            component_digest: &invocation.component_digest,
        }),
    );
    span
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{
        EffectiveReleaseId, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION, ServingRelease,
    };

    use super::*;
    use crate::plugins::wamn_postgres::CandidateBindingWorld;

    fn released() -> ConnectionInvocation {
        ConnectionInvocation {
            package_id: "package_a".to_string(),
            wiring_id: "orders".to_string(),
            wiring_version: 3,
            node_id: "archive".to_string(),
            occurrence: 1,
            component_digest: format!("sha256:{}", "a".repeat(64)),
            closure: ConnectionExecutionClosure::Released,
        }
    }

    fn candidate() -> ConnectionInvocation {
        ConnectionInvocation {
            closure: ConnectionExecutionClosure::Candidate {
                effective_release_id: 9,
                environment: "staging".to_string(),
                wiring_hash: format!("sha256:{}", "b".repeat(64)),
                component: "archiver".to_string(),
                interface_version: "0.1.0".to_string(),
                binding_world: Arc::new(
                    CandidateBindingWorld::from_json(serde_json::json!([]))
                        .expect("an empty candidate binding world decodes"),
                ),
            },
            ..released()
        }
    }

    fn manifest(tenant: &str, package: &str) -> ServingManifest {
        ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: tenant.to_string(),
                effective_release_id: EffectiveReleaseId::new(4).expect("a positive release id"),
                environment: "warehouse-eu-3".to_string(),
                packages: BTreeSet::from([
                    PackageCoordinate::new(package, "1.0.0").expect("a canonical coordinate")
                ]),
            },
            components: BTreeSet::new(),
            wirings: BTreeSet::new(),
            attachments: BTreeMap::new(),
            registrations: BTreeMap::new(),
        }
    }

    /// A candidate closure states its own coordinates and needs no manifest.
    #[test]
    fn a_candidate_closure_carries_its_own_coordinates() {
        let coordinates =
            release_coordinates(&candidate(), None, "tenant-a").expect("a candidate resolves");
        assert_eq!(coordinates, (9, "staging".to_string()));
    }

    /// EXIT GATE: a released closure resolves through the mounted manifest,
    /// where before it refused outright and the capability was unusable in any
    /// released deployment.
    ///
    /// The fixture environment is deliberately UNGUESSABLE. A hardcoded
    /// "prod" reads the same as a manifest lookup when the fixture itself says
    /// "prod", and a mutant that hardcoded it survived this test until the
    /// value was changed — the distinguishing-step law in miniature.
    #[test]
    fn a_released_closure_resolves_through_the_mounted_manifest() {
        let manifest = manifest("tenant-a", "package_a");
        let coordinates = release_coordinates(&released(), Some(&manifest), "tenant-a")
            .expect("a released closure resolves");
        assert_eq!(coordinates, (4, "warehouse-eu-3".to_string()));
    }

    /// A manifest for ANOTHER tenant must not supply coordinates: it would
    /// hand this guest an effective release under which some other tenant's
    /// binding authorizes — a cross-tenant reach wearing a mounted file.
    #[test]
    fn a_manifest_for_another_tenant_refuses() {
        let manifest = manifest("tenant-b", "package_a");
        assert_eq!(
            release_coordinates(&released(), Some(&manifest), "tenant-a"),
            Err(BindingError::Unauthorized)
        );
    }

    /// A release that never shipped this package must not supply coordinates
    /// for it.
    #[test]
    fn a_manifest_without_the_invoked_package_refuses() {
        let manifest = manifest("tenant-a", "package_b");
        assert_eq!(
            release_coordinates(&released(), Some(&manifest), "tenant-a"),
            Err(BindingError::Unauthorized)
        );
    }

    /// No mounted manifest means no released coordinates. This is the
    /// fail-closed arm the capability had for EVERY release before the weld
    /// was wired, and it stays correct when the weld is absent.
    #[test]
    fn a_released_closure_without_a_manifest_refuses() {
        assert_eq!(
            release_coordinates(&released(), None, "tenant-a"),
            Err(BindingError::Unauthorized)
        );
    }

    /// A candidate closure handed a manifest is a caller mismatch, not a
    /// policy question — it refuses rather than picking whichever arm looks
    /// closer, because guessing here decides which binding authorizes.
    #[test]
    fn a_candidate_closure_handed_a_manifest_refuses() {
        let manifest = manifest("tenant-a", "package_a");
        assert_eq!(
            release_coordinates(&candidate(), Some(&manifest), "tenant-a"),
            Err(BindingError::Unauthorized)
        );
    }
}
